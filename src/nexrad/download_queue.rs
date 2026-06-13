//! Download queue manager for archive downloads.
//!
//! Encapsulates the queue of files to download and the state machine for
//! advancing through them. The manager does NOT perform downloads or access
//! network channels — it returns [`QueueAction`] values telling the caller
//! what to do next.
//!
//! Downloads may run in parallel up to a configurable concurrency limit
//! (see [`DEFAULT_MAX_PARALLEL`]); callers advance the queue until either
//! all pending work is drained or the concurrency ceiling is reached.

/// State of a single item in the download queue.
#[derive(Clone, Debug)]
pub(crate) enum QueueItemState {
    /// Queued but not yet started.
    Pending,
    /// Download has been kicked off.
    Active,
    /// Download completed successfully.
    Done,
    /// Download failed with an error message.
    #[allow(dead_code)]
    Failed(String),
}

/// A single file in the download queue.
#[derive(Clone, Debug)]
pub(crate) struct QueueItem {
    pub date: chrono::NaiveDate,
    pub file_name: String,
    pub scan_start: i64,
    pub scan_end: i64,
    pub state: QueueItemState,
    /// When `Some(n)`, only elevation number `n` should be decoded and stored
    /// from this archive file (the active elevation filter at enqueue time).
    /// `None` means store the whole volume. The whole file is fetched from S3
    /// either way — a NEXRAD archive object is a single blob — so this scopes
    /// decode/storage, not the network transfer.
    pub elevation_filter: Option<u8>,
    /// Acquisition-drawer operation representing this item, created by the
    /// enqueuer. Carried on the item (rather than paired FIFO at dispatch)
    /// so priority-ordered dispatch and pruning stay correlated.
    pub operation_id: Option<crate::state::OperationId>,
    /// Dispatch priority — lower dispatches sooner. Recomputed against the
    /// playhead by [`DownloadQueueManager::reprioritize`] each pump.
    pub priority: i64,
}

impl QueueItem {
    pub fn new(
        date: chrono::NaiveDate,
        file_name: String,
        scan_start: i64,
        scan_end: i64,
        elevation_filter: Option<u8>,
    ) -> Self {
        Self {
            date,
            file_name,
            scan_start,
            scan_end,
            state: QueueItemState::Pending,
            elevation_filter,
            operation_id: None,
            priority: 0,
        }
    }

    /// Attach the acquisition operation that tracks this item.
    pub fn with_operation(mut self, id: crate::state::OperationId) -> Self {
        self.operation_id = Some(id);
        self
    }
}

/// Dispatch priority of a queued scan relative to the playhead — lower is
/// sooner. A scan covering the playhead is most urgent (0); otherwise the
/// distance in seconds to the nearest edge, with scans *against* the playback
/// direction penalized 4x so the queue serves where the cursor is heading
/// without ever fully starving the trail behind it.
pub(crate) fn playhead_priority(
    scan_start: i64,
    scan_end: i64,
    playhead: i64,
    forward: bool,
) -> i64 {
    let dist = if playhead < scan_start {
        scan_start - playhead
    } else if playhead > scan_end {
        playhead - scan_end
    } else {
        return 0;
    };
    let ahead = if forward {
        scan_start >= playhead
    } else {
        scan_end <= playhead
    };
    if ahead {
        dist
    } else {
        dist.saturating_mul(4)
    }
}

/// The `[start, end]` window (Unix seconds) of scans worth having queued for
/// a playhead at `pos`. Ahead of the playback direction: the configured
/// lookahead, scaled with speed while playing so fast playback buffers
/// proportionally further. Behind: a fixed ~2-scan trail so a small backward
/// jog doesn't hit a cold cache. `forward` flips the asymmetry.
pub(crate) fn prefetch_window(
    pos: f64,
    speed_secs_per_sec: f64,
    playing: bool,
    forward: bool,
) -> (i64, i64) {
    let lead = if playing {
        crate::PREFETCH_LOOKAHEAD_SECS_PAUSED
            .max(speed_secs_per_sec * crate::PREFETCH_PLAY_LEAD_SECS)
    } else {
        crate::PREFETCH_LOOKAHEAD_SECS_PAUSED
    };
    let trail = 2.0 * crate::FALLBACK_SCAN_DURATION_SECS as f64;
    if forward {
        ((pos - trail) as i64, (pos + lead) as i64)
    } else {
        ((pos - lead) as i64, (pos + trail) as i64)
    }
}

/// Action the caller should take after a queue operation.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum QueueAction {
    /// Start downloading a specific file.
    StartDownload {
        idx: usize,
        date: chrono::NaiveDate,
        file_name: String,
        scan_start: i64,
        scan_end: i64,
        elevation_filter: Option<u8>,
        operation_id: Option<crate::state::OperationId>,
        remaining: usize,
    },
    /// All items are done/failed — queue is drained.
    Complete,
    /// The concurrency ceiling is reached — caller should poll again once
    /// one or more active downloads complete.
    Saturated,
    /// Queue is paused — do nothing.
    Paused,
}

/// Default maximum number of concurrent downloads.
///
/// Keeping a small cap here avoids overwhelming the browser's per-origin
/// connection limit (commonly 6) while still pipelining enough requests to
/// saturate a residential uplink.
pub(crate) const DEFAULT_MAX_PARALLEL: usize = 4;

/// Default cap on total reactively-prefetched bytes per session (256 MB).
///
/// Reactive prefetch fetches as a side effect of navigation; this is the
/// backstop against runaway background downloading (PRODUCT.md §5.1). Storage
/// is separately bounded by the IDB quota/eviction system — this caps *session
/// bandwidth*, not disk. Adjustable.
pub(crate) const DEFAULT_MAX_AUTO_FETCH_BYTES: u64 = 256 * 1024 * 1024;

/// Manages the download queue state machine.
///
/// This struct owns the queue of [`QueueItem`]s and the per-item operation
/// IDs. It does **not** hold references to download channels or data facades
/// — the caller acts on the returned [`QueueAction`] values.
pub(crate) struct DownloadQueueManager {
    queue: Vec<QueueItem>,
    /// Maps an Active item's `scan_start` to the acquisition operation ID
    /// that represents it. Keeps correlation correct when multiple downloads
    /// are in flight simultaneously.
    active_operation_ids: std::collections::HashMap<i64, crate::state::OperationId>,
    max_parallel: usize,
    /// Running total of bytes fetched via reactive prefetch this session.
    /// Bounds runaway background downloading; not reset on queue clear.
    auto_fetched_bytes: u64,
    max_auto_fetch_bytes: u64,
}

impl DownloadQueueManager {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            active_operation_ids: std::collections::HashMap::new(),
            max_parallel: DEFAULT_MAX_PARALLEL,
            auto_fetched_bytes: 0,
            max_auto_fetch_bytes: DEFAULT_MAX_AUTO_FETCH_BYTES,
        }
    }

    /// Check if the queue has any active or pending items.
    pub fn has_work(&self) -> bool {
        self.queue
            .iter()
            .any(|item| matches!(item.state, QueueItemState::Pending | QueueItemState::Active))
    }

    /// Mark an active item as done (its download completed successfully).
    ///
    /// Idempotent: if the item is already Done or no longer Active, this is
    /// a no-op.
    pub fn mark_active_done(&mut self, scan_start: i64) {
        if let Some(item) = self.queue.iter_mut().find(|item| {
            matches!(item.state, QueueItemState::Active) && item.scan_start == scan_start
        }) {
            item.state = QueueItemState::Done;
        }
    }

    /// Number of items currently in the Active state.
    pub fn active_count(&self) -> usize {
        self.queue
            .iter()
            .filter(|item| matches!(item.state, QueueItemState::Active))
            .count()
    }

    /// All currently active items (for concurrency polling).
    pub fn active_items(&self) -> impl Iterator<Item = &QueueItem> {
        self.queue
            .iter()
            .filter(|item| matches!(item.state, QueueItemState::Active))
    }

    /// All items still waiting to dispatch (Pending). Used to populate the
    /// timeline's queued-cell ghosts each pump — without it queued scans are
    /// invisible on the strip.
    pub fn pending_items(&self) -> impl Iterator<Item = &QueueItem> {
        self.queue
            .iter()
            .filter(|item| matches!(item.state, QueueItemState::Pending))
    }

    /// Recompute every Pending item's dispatch priority against the playhead.
    /// Call once per pump, before the fill loop, so `advance` serves the
    /// scans nearest the cursor (in playback direction) first.
    pub fn reprioritize(&mut self, playhead: i64, forward: bool) {
        for item in &mut self.queue {
            if matches!(item.state, QueueItemState::Pending) {
                item.priority =
                    playhead_priority(item.scan_start, item.scan_end, playhead, forward);
            }
        }
    }

    /// Drop Pending items the predicate rejects (e.g. scans the playhead has
    /// scrubbed far away from), returning them so the caller can cancel their
    /// acquisition operations. Active items always survive — in-flight HTTP
    /// is left to finish and free its slot naturally.
    pub fn prune_pending(&mut self, keep: impl Fn(&QueueItem) -> bool) -> Vec<QueueItem> {
        let mut pruned = Vec::new();
        self.queue.retain(|item| {
            if matches!(item.state, QueueItemState::Pending) && !keep(item) {
                pruned.push(item.clone());
                false
            } else {
                true
            }
        });
        pruned
    }

    /// Advance the queue: start the highest-priority (lowest value) pending
    /// item if a concurrency slot is available and the queue is not paused.
    /// Ties dispatch in enqueue order.
    pub fn advance(&mut self, is_paused: bool) -> QueueAction {
        if is_paused {
            return QueueAction::Paused;
        }

        if self.active_count() >= self.max_parallel {
            return QueueAction::Saturated;
        }

        let next_pending = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item.state, QueueItemState::Pending))
            .min_by_key(|(_, item)| item.priority)
            .map(|(idx, _)| idx);

        if let Some(idx) = next_pending {
            let remaining = self
                .queue
                .iter()
                .filter(|item| matches!(item.state, QueueItemState::Pending))
                .count();
            let item = &self.queue[idx];
            let action = QueueAction::StartDownload {
                idx,
                date: item.date,
                file_name: item.file_name.clone(),
                scan_start: item.scan_start,
                scan_end: item.scan_end,
                elevation_filter: item.elevation_filter,
                operation_id: item.operation_id,
                remaining,
            };
            self.queue[idx].state = QueueItemState::Active;
            action
        } else if self.active_count() == 0 {
            // All items are Done/Failed and nothing is in flight — queue drained.
            self.queue.clear();
            QueueAction::Complete
        } else {
            // Nothing pending, but downloads still in flight. Caller should
            // poll again after they complete.
            QueueAction::Saturated
        }
    }

    /// Clear the queue and all tracked operation IDs.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.active_operation_ids.clear();
    }

    /// Associate an acquisition operation ID with an active download.
    pub fn set_operation_id(&mut self, scan_start: i64, id: crate::state::OperationId) {
        self.active_operation_ids.insert(scan_start, id);
    }

    /// Take (remove) the operation ID for the given scan_start.
    pub fn take_operation_id(&mut self, scan_start: i64) -> Option<crate::state::OperationId> {
        self.active_operation_ids.remove(&scan_start)
    }

    /// Find an item by scan_start timestamp.
    pub fn find_by_scan_start(&self, scan_start: i64) -> Option<&QueueItem> {
        self.queue.iter().find(|item| item.scan_start == scan_start)
    }

    /// Append items to the existing queue (used by reactive prefetch, which
    /// adds to in-flight work rather than replacing it). Skips any scan_start
    /// already present so the same scan is never queued twice.
    pub fn enqueue(&mut self, items: impl IntoIterator<Item = QueueItem>) {
        for item in items {
            if self.find_by_scan_start(item.scan_start).is_none() {
                self.queue.push(item);
            }
        }
    }

    /// Re-enqueue a scan for download, used by explicit retry/fetch (the
    /// failed-cell tick, the queue sheet, the inspector). Unlike [`Self::enqueue`]
    /// this does **not** skip a scan already present — a failed download leaves a
    /// `Done` item behind (failures are marked Done so the slot frees), which
    /// `enqueue` would treat as "already queued" and silently drop the retry.
    /// If an item for `scan_start` exists it is reset to `Pending` (refreshing
    /// its operation id + elevation filter); otherwise the new item is appended.
    ///
    /// An item already `Active` is left untouched: resetting an in-flight
    /// download to `Pending` (e.g. an inspector per-tilt fetch colliding with a
    /// whole-volume reactive fetch) would orphan its completion bookkeeping —
    /// `mark_active_done` only acts on `Active` items, so the reset item would
    /// stay Pending and get redundantly redispatched, and the elevation-filter
    /// swap could re-scope the in-flight ingest. Returns whether work is now
    /// pending (or already in flight) for that scan (always true).
    pub fn requeue(&mut self, item: QueueItem) -> bool {
        if let Some(existing) = self
            .queue
            .iter_mut()
            .find(|i| i.scan_start == item.scan_start)
        {
            // A download already in flight finishes on its own terms; don't
            // clobber it (see the doc comment).
            if matches!(existing.state, QueueItemState::Active) {
                return true;
            }
            existing.state = QueueItemState::Pending;
            existing.operation_id = item.operation_id;
            existing.elevation_filter = item.elevation_filter;
            existing.file_name = item.file_name;
            existing.date = item.date;
            existing.scan_end = item.scan_end;
        } else {
            self.queue.push(item);
        }
        true
    }

    /// Whether the session auto-fetch volume cap has been reached.
    pub fn auto_fetch_cap_reached(&self) -> bool {
        self.auto_fetched_bytes >= self.max_auto_fetch_bytes
    }

    /// Record bytes fetched via reactive prefetch toward the volume cap.
    pub fn record_auto_fetched(&mut self, bytes: u64) {
        self.auto_fetched_bytes = self.auto_fetched_bytes.saturating_add(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn item(scan_start: i64, elevation_filter: Option<u8>) -> QueueItem {
        let date = chrono::NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        QueueItem::new(
            date,
            format!("f{scan_start}"),
            scan_start,
            scan_start + 300,
            elevation_filter,
        )
    }

    #[wasm_bindgen_test]
    fn enqueue_appends_and_skips_duplicate_scan_start() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, Some(1)), item(400, None)]);
        // Same scan_start is skipped even with a different filter/file.
        q.enqueue([item(100, Some(3))]);
        // A new scan_start appends.
        q.enqueue([item(700, Some(2))]);

        // Drain: each distinct scan_start dispatches exactly once.
        let mut started = std::collections::HashSet::new();
        while let QueueAction::StartDownload { scan_start, .. } = q.advance(false) {
            assert!(started.insert(scan_start), "a scan was dispatched twice");
        }
        assert_eq!(started.len(), 3); // 100, 400, 700 — the duplicate 100 skipped
    }

    #[wasm_bindgen_test]
    fn playhead_priority_orders_nearest_first_with_direction_bias() {
        // Covering the playhead = most urgent.
        assert_eq!(playhead_priority(100, 400, 200, true), 0);
        // Ahead (forward): plain distance.
        assert_eq!(playhead_priority(500, 800, 200, true), 300);
        // Behind (forward): distance penalized 4x.
        assert_eq!(playhead_priority(100, 150, 200, true), 200);
        // Same scan ahead when playing backward: now it's against direction.
        assert_eq!(playhead_priority(500, 800, 200, false), 1200);
        // And the behind scan becomes "ahead" backward: plain distance.
        assert_eq!(playhead_priority(100, 150, 200, false), 50);
        // Nearest-first: closer ahead beats farther ahead.
        assert!(playhead_priority(300, 600, 200, true) < playhead_priority(900, 1200, 200, true));
    }

    #[wasm_bindgen_test]
    fn prefetch_window_shapes() {
        let trail = 2.0 * crate::FALLBACK_SCAN_DURATION_SECS as f64;
        // Paused forward: fixed lead, fixed trail.
        let (s, e) = prefetch_window(10_000.0, 300.0, false, true);
        assert_eq!(s, (10_000.0 - trail) as i64);
        assert_eq!(e, (10_000.0 + crate::PREFETCH_LOOKAHEAD_SECS_PAUSED) as i64);
        // Playing fast forward: lead scales with speed.
        let (_, e_fast) = prefetch_window(10_000.0, 1200.0, true, true);
        assert_eq!(
            e_fast,
            (10_000.0 + 1200.0 * crate::PREFETCH_PLAY_LEAD_SECS) as i64
        );
        // Backward play mirrors the asymmetry.
        let (s_b, e_b) = prefetch_window(10_000.0, 300.0, false, false);
        assert_eq!(
            s_b,
            (10_000.0 - crate::PREFETCH_LOOKAHEAD_SECS_PAUSED) as i64
        );
        assert_eq!(e_b, (10_000.0 + trail) as i64);
    }

    #[wasm_bindgen_test]
    fn advance_dispatches_by_priority_not_fifo() {
        let mut q = DownloadQueueManager::new();
        // Enqueued far-first, near-last.
        q.enqueue([item(9000, None), item(3000, None), item(600, None)]);
        // Playhead at 500, forward: 600 is nearest, then 3000, then 9000.
        q.reprioritize(500, true);
        let mut order = Vec::new();
        while let QueueAction::StartDownload { scan_start, .. } = q.advance(false) {
            order.push(scan_start);
        }
        assert_eq!(order, vec![600, 3000, 9000]);
    }

    #[wasm_bindgen_test]
    fn prune_pending_keeps_active_and_returns_dropped() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None), item(5000, None), item(9000, None)]);
        // Activate the first item (playhead near 100).
        q.reprioritize(100, true);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload {
                scan_start: 100,
                ..
            }
        ));
        // Scrub far away and prune everything outside [8000, 10000].
        let pruned = q.prune_pending(|i| i.scan_start >= 8000 && i.scan_start <= 10_000);
        let pruned_starts: Vec<i64> = pruned.iter().map(|i| i.scan_start).collect();
        assert_eq!(pruned_starts, vec![5000]);
        // The Active item (100) survives; 9000 stays pending.
        assert!(q.find_by_scan_start(100).is_some());
        assert!(q.find_by_scan_start(9000).is_some());
        assert!(q.find_by_scan_start(5000).is_none());
    }

    #[wasm_bindgen_test]
    fn requeue_resets_a_done_item_so_retry_actually_redownloads() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(1000, Some(1))]);
        // Dispatch it, then mark it Done — the state a *failed* download leaves
        // behind (failures are marked Done so the concurrency slot frees).
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload {
                scan_start: 1000,
                ..
            }
        ));
        q.mark_active_done(1000);
        // A plain enqueue would see the existing item and skip — no re-fetch.
        q.enqueue([item(1000, Some(1))]);
        assert!(
            matches!(q.advance(false), QueueAction::Complete),
            "enqueue must NOT resurrect a Done item (the retry bug)"
        );

        // requeue resets the existing item to Pending → it dispatches again,
        // carrying the fresh elevation filter.
        let mut retry = item(1000, Some(3));
        retry = retry.with_operation(42);
        assert!(q.requeue(retry));
        match q.advance(false) {
            QueueAction::StartDownload {
                scan_start,
                elevation_filter,
                operation_id,
                ..
            } => {
                assert_eq!(scan_start, 1000);
                assert_eq!(elevation_filter, Some(3));
                assert_eq!(operation_id, Some(42));
            }
            other => panic!("expected the requeued item to dispatch, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn requeue_does_not_clobber_an_in_flight_item() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(2000, None)]);
        // Dispatch it → the item is now Active (in flight).
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload {
                scan_start: 2000,
                ..
            }
        ));

        // An inspector per-tilt requeue lands while the whole-volume download is
        // still in flight. It must NOT reset the Active item to Pending.
        let retry = item(2000, Some(3)).with_operation(99);
        assert!(q.requeue(retry));
        // No second dispatch: the item is still Active (nothing Pending), so the
        // queue reports Saturated, not a fresh StartDownload.
        assert!(
            matches!(q.advance(false), QueueAction::Saturated),
            "requeue must not redispatch an in-flight download"
        );

        // The original completion bookkeeping still applies to the Active item.
        q.mark_active_done(2000);
        assert!(matches!(q.advance(false), QueueAction::Complete));
    }

    #[wasm_bindgen_test]
    fn requeue_appends_when_scan_absent() {
        let mut q = DownloadQueueManager::new();
        // Nothing queued yet — requeue behaves like an append.
        assert!(q.requeue(item(7000, None)));
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload {
                scan_start: 7000,
                ..
            }
        ));
    }

    #[wasm_bindgen_test]
    fn auto_fetch_cap_tracks_recorded_bytes() {
        let mut q = DownloadQueueManager::new();
        assert!(!q.auto_fetch_cap_reached());
        q.record_auto_fetched(DEFAULT_MAX_AUTO_FETCH_BYTES - 1);
        assert!(!q.auto_fetch_cap_reached());
        q.record_auto_fetched(1);
        assert!(q.auto_fetch_cap_reached());
        // Saturating: further recording doesn't overflow or un-trip the cap.
        q.record_auto_fetched(u64::MAX);
        assert!(q.auto_fetch_cap_reached());
    }
}
