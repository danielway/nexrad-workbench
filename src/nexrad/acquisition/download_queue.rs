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
    pub operation_id: Option<crate::core::OperationId>,
    /// Dispatch priority — lower dispatches sooner. Recomputed against the
    /// playhead by [`DownloadQueueManager::reprioritize`] each pump.
    pub priority: i64,
}

impl QueueItem {
    pub(crate) fn new(
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
    pub(crate) fn with_operation(mut self, id: crate::core::OperationId) -> Self {
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
#[derive(Debug)]
pub(crate) enum QueueAction {
    /// Start downloading a specific file.
    StartDownload {
        date: chrono::NaiveDate,
        file_name: String,
        scan_start: i64,
        scan_end: i64,
        elevation_filter: Option<u8>,
        operation_id: Option<crate::core::OperationId>,
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
    active_operation_ids: std::collections::HashMap<i64, crate::core::OperationId>,
    max_parallel: usize,
    /// Running total of bytes fetched via reactive prefetch this session.
    /// Bounds runaway background downloading; not reset on queue clear.
    auto_fetched_bytes: u64,
    max_auto_fetch_bytes: u64,
}

impl DownloadQueueManager {
    pub(crate) fn new() -> Self {
        Self {
            queue: Vec::new(),
            active_operation_ids: std::collections::HashMap::new(),
            max_parallel: DEFAULT_MAX_PARALLEL,
            auto_fetched_bytes: 0,
            max_auto_fetch_bytes: DEFAULT_MAX_AUTO_FETCH_BYTES,
        }
    }

    /// Check if the queue has any active or pending items.
    pub(crate) fn has_work(&self) -> bool {
        self.queue
            .iter()
            .any(|item| matches!(item.state, QueueItemState::Pending | QueueItemState::Active))
    }

    /// Mark an active item as done (its download completed successfully).
    ///
    /// Idempotent: if the item is already Done or no longer Active, this is
    /// a no-op.
    pub(crate) fn mark_active_done(&mut self, scan_start: i64) {
        if let Some(item) = self.queue.iter_mut().find(|item| {
            matches!(item.state, QueueItemState::Active) && item.scan_start == scan_start
        }) {
            item.state = QueueItemState::Done;
        }
    }

    /// Number of items currently in the Active state.
    pub(crate) fn active_count(&self) -> usize {
        self.queue
            .iter()
            .filter(|item| matches!(item.state, QueueItemState::Active))
            .count()
    }

    /// All currently active items (for concurrency polling).
    pub(crate) fn active_items(&self) -> impl Iterator<Item = &QueueItem> {
        self.queue
            .iter()
            .filter(|item| matches!(item.state, QueueItemState::Active))
    }

    /// All items still waiting to dispatch (Pending). Used to populate the
    /// timeline's queued-cell ghosts each pump — without it queued scans are
    /// invisible on the strip.
    pub(crate) fn pending_items(&self) -> impl Iterator<Item = &QueueItem> {
        self.queue
            .iter()
            .filter(|item| matches!(item.state, QueueItemState::Pending))
    }

    /// Recompute every Pending item's dispatch priority against the playhead.
    /// Call once per pump, before the fill loop, so `advance` serves the
    /// scans nearest the cursor (in playback direction) first.
    pub(crate) fn reprioritize(&mut self, playhead: i64, forward: bool) {
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
    pub(crate) fn prune_pending(&mut self, keep: impl Fn(&QueueItem) -> bool) -> Vec<QueueItem> {
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
    pub(crate) fn advance(&mut self, is_paused: bool) -> QueueAction {
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
            // All items are Done and nothing is in flight — queue drained.
            self.queue.clear();
            QueueAction::Complete
        } else {
            // Nothing pending, but downloads still in flight. Caller should
            // poll again after they complete.
            QueueAction::Saturated
        }
    }

    /// Clear the queue and all tracked operation IDs.
    pub(crate) fn clear(&mut self) {
        self.queue.clear();
        self.active_operation_ids.clear();
    }

    /// Associate an acquisition operation ID with an active download.
    pub(crate) fn set_operation_id(&mut self, scan_start: i64, id: crate::core::OperationId) {
        self.active_operation_ids.insert(scan_start, id);
    }

    /// Take (remove) the operation ID for the given scan_start.
    pub(crate) fn take_operation_id(
        &mut self,
        scan_start: i64,
    ) -> Option<crate::core::OperationId> {
        self.active_operation_ids.remove(&scan_start)
    }

    /// Find an item by scan_start timestamp.
    pub(crate) fn find_by_scan_start(&self, scan_start: i64) -> Option<&QueueItem> {
        self.queue.iter().find(|item| item.scan_start == scan_start)
    }

    /// Append items to the existing queue (used by reactive prefetch, which
    /// adds to in-flight work rather than replacing it). Skips any scan_start
    /// already present so the same scan is never queued twice.
    pub(crate) fn enqueue(&mut self, items: impl IntoIterator<Item = QueueItem>) {
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
    pub(crate) fn requeue(&mut self, item: QueueItem) -> bool {
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
            let scan_start = item.scan_start;
            existing.state = QueueItemState::Pending;
            existing.operation_id = item.operation_id;
            existing.elevation_filter = item.elevation_filter;
            existing.file_name = item.file_name;
            existing.date = item.date;
            existing.scan_end = item.scan_end;
            // Drop any operation-id correlation left over from a previous
            // dispatch of this scan. In normal flow the prior completion's
            // `take_operation_id` already removed it; clearing it here makes the
            // new op id authoritative *by construction* rather than relying on
            // that drain having happened, so a lingering stale entry can never
            // be returned for the requeued retry. The next dispatch re-`set`s
            // the fresh id.
            self.active_operation_ids.remove(&scan_start);
        } else {
            self.queue.push(item);
        }
        true
    }

    /// Whether the session auto-fetch volume cap has been reached.
    pub(crate) fn auto_fetch_cap_reached(&self) -> bool {
        self.auto_fetched_bytes >= self.max_auto_fetch_bytes
    }

    /// Record bytes fetched via reactive prefetch toward the volume cap.
    pub(crate) fn record_auto_fetched(&mut self, bytes: u64) {
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

    // ── active_operation_ids correlation channel ────────────────────────────

    /// `set_operation_id` then `take_operation_id` returns the id exactly once;
    /// a second take finds nothing. This is the channel that drives
    /// `mark_completed`/`mark_failed` in `app/download.rs`.
    #[wasm_bindgen_test]
    fn set_then_take_operation_id_returns_once() {
        let mut q = DownloadQueueManager::new();
        q.set_operation_id(1000, 42);
        assert_eq!(q.take_operation_id(1000), Some(42));
        // Drained — a late second completion finds nothing.
        assert_eq!(q.take_operation_id(1000), None);
        // An unknown scan_start was never mapped.
        assert_eq!(q.take_operation_id(9999), None);
    }

    /// `clear()` wipes the operation-id map. This documents the orphan: a late
    /// completion arriving after a clear (e.g. a selection change) finds no id,
    /// so its operation is never marked Completed/Failed by this channel.
    #[wasm_bindgen_test]
    fn clear_drops_pending_operation_id_entry() {
        let mut q = DownloadQueueManager::new();
        q.set_operation_id(1000, 42);
        q.clear();
        assert_eq!(
            q.take_operation_id(1000),
            None,
            "clear() must drop pending op-id entries (orphaning a late completion)"
        );
    }

    /// Regression: a requeue with a NEW op id must make `take` return the NEW
    /// id, never a stale one left over from a previous dispatch of the same
    /// scan_start. `requeue` drops the stale map entry, and the re-dispatch
    /// re-`set`s the fresh id — so the correlation can't mis-attribute the
    /// completion to the wrong operation.
    #[wasm_bindgen_test]
    fn requeue_with_new_op_id_takes_the_new_id_not_a_stale_one() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(1000, Some(1)).with_operation(7)]);
        // Dispatch under the original op id 7, then map it (mirrors the
        // selection_download dispatch path: StartDownload → set_operation_id).
        match q.advance(false) {
            QueueAction::StartDownload {
                scan_start,
                operation_id,
                ..
            } => {
                assert_eq!(scan_start, 1000);
                assert_eq!(operation_id, Some(7));
                q.set_operation_id(scan_start, 7);
            }
            other => panic!("expected dispatch, got {other:?}"),
        }
        // The download finishes (Done) but — to model the fragile window — the
        // stale map entry for op 7 is deliberately left behind.
        q.mark_active_done(1000);

        // A retry requeues the same scan under a NEW op id 99.
        let retry = item(1000, Some(3)).with_operation(99);
        assert!(q.requeue(retry));

        // Re-dispatch carries the new op id, and the dispatch re-sets the map.
        match q.advance(false) {
            QueueAction::StartDownload {
                scan_start,
                operation_id,
                ..
            } => {
                assert_eq!(operation_id, Some(99));
                q.set_operation_id(scan_start, 99);
            }
            other => panic!("expected re-dispatch, got {other:?}"),
        }

        // The completion now correlates to the NEW op id, never the stale 7.
        assert_eq!(q.take_operation_id(1000), Some(99));
    }

    /// `requeue` itself drops the stale map entry even before the re-dispatch,
    /// so the new op id is authoritative by construction (not merely because the
    /// next `set_operation_id` overwrites it). Without the fix, the stale id
    /// would survive the requeue and a `take` in the pre-dispatch window would
    /// return the WRONG operation.
    #[wasm_bindgen_test]
    fn requeue_clears_the_stale_op_id_entry() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(1000, Some(1)).with_operation(7)]);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));
        q.set_operation_id(1000, 7);
        q.mark_active_done(1000);

        // Requeue under a new op id — the stale 7 must be gone immediately.
        assert!(q.requeue(item(1000, Some(3)).with_operation(99)));
        assert_eq!(
            q.take_operation_id(1000),
            None,
            "requeue must drop the stale op-id entry, not leak op 7"
        );
    }

    /// An Active item's requeue leaves the in-flight op-id correlation intact:
    /// the running download still completes against its original id.
    #[wasm_bindgen_test]
    fn requeue_on_active_item_keeps_in_flight_op_id() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(2000, None).with_operation(11)]);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));
        q.set_operation_id(2000, 11);

        // A requeue lands while the item is Active → it's left untouched, and so
        // is its op-id mapping.
        assert!(q.requeue(item(2000, Some(3)).with_operation(22)));
        assert_eq!(
            q.take_operation_id(2000),
            Some(11),
            "an in-flight download keeps its original op id"
        );
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

#[cfg(test)]
mod coverage_tests {
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

    // ── QueueItem construction ──────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn new_item_defaults_to_pending_no_op_zero_priority() {
        let it = item(500, Some(2));
        assert!(matches!(it.state, QueueItemState::Pending));
        assert_eq!(it.operation_id, None);
        assert_eq!(it.priority, 0);
        assert_eq!(it.scan_start, 500);
        assert_eq!(it.scan_end, 800); // scan_start + 300 from the builder
        assert_eq!(it.elevation_filter, Some(2));
        assert_eq!(it.file_name, "f500");
    }

    #[wasm_bindgen_test]
    fn with_operation_attaches_id_and_preserves_rest() {
        let it = item(500, None).with_operation(77);
        assert_eq!(it.operation_id, Some(77));
        assert_eq!(it.scan_start, 500);
        assert!(matches!(it.state, QueueItemState::Pending));
    }

    // ── has_work / active_count / clear ─────────────────────────────────────

    #[wasm_bindgen_test]
    fn has_work_false_when_empty_true_with_pending_and_active() {
        let mut q = DownloadQueueManager::new();
        assert!(!q.has_work(), "empty queue has no work");
        q.enqueue([item(100, None)]);
        assert!(q.has_work(), "a Pending item is work");
        // Dispatch → Active is still work.
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));
        assert!(q.has_work(), "an Active item is still work");
        // Mark done → no more work pending/active.
        q.mark_active_done(100);
        assert!(!q.has_work(), "only a Done item remains — no work");
    }

    #[wasm_bindgen_test]
    fn clear_empties_queue_and_drops_work() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None), item(400, None)]);
        assert!(q.has_work());
        q.clear();
        assert!(!q.has_work());
        assert!(q.find_by_scan_start(100).is_none());
        // After clear, advance reports Complete (nothing pending, nothing active).
        assert!(matches!(q.advance(false), QueueAction::Complete));
    }

    #[wasm_bindgen_test]
    fn active_count_reflects_dispatched_items() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None), item(400, None)]);
        assert_eq!(q.active_count(), 0);
        q.reprioritize(0, true);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));
        assert_eq!(q.active_count(), 1);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));
        assert_eq!(q.active_count(), 2);
        q.mark_active_done(100);
        assert_eq!(q.active_count(), 1);
    }

    // ── active_items / pending_items iterators ──────────────────────────────

    #[wasm_bindgen_test]
    fn active_and_pending_iterators_partition_by_state() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None), item(400, None), item(900, None)]);
        // Activate exactly one (nearest the playhead at 100).
        q.reprioritize(100, true);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));

        let active: Vec<i64> = q.active_items().map(|i| i.scan_start).collect();
        assert_eq!(active, vec![100]);

        let mut pending: Vec<i64> = q.pending_items().map(|i| i.scan_start).collect();
        pending.sort();
        assert_eq!(pending, vec![400, 900]);
    }

    #[wasm_bindgen_test]
    fn iterators_empty_when_no_matching_state() {
        let q = DownloadQueueManager::new();
        assert_eq!(q.active_items().count(), 0);
        assert_eq!(q.pending_items().count(), 0);
    }

    // ── advance: Paused / Saturated / remaining ─────────────────────────────

    #[wasm_bindgen_test]
    fn advance_paused_short_circuits_without_dispatch() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None)]);
        assert!(matches!(q.advance(true), QueueAction::Paused));
        // Paused did NOT dispatch — the item is still Pending.
        assert_eq!(q.active_count(), 0);
        assert_eq!(q.pending_items().count(), 1);
    }

    #[wasm_bindgen_test]
    fn advance_saturated_at_concurrency_ceiling() {
        let mut q = DownloadQueueManager::new();
        // DEFAULT_MAX_PARALLEL == 4; enqueue 5 so one stays pending at the cap.
        q.enqueue([
            item(100, None),
            item(200, None),
            item(300, None),
            item(400, None),
            item(500, None),
        ]);
        for _ in 0..DEFAULT_MAX_PARALLEL {
            assert!(matches!(
                q.advance(false),
                QueueAction::StartDownload { .. }
            ));
        }
        assert_eq!(q.active_count(), DEFAULT_MAX_PARALLEL);
        // Ceiling reached, one still pending → Saturated, not a dispatch.
        assert!(matches!(q.advance(false), QueueAction::Saturated));
        assert_eq!(q.pending_items().count(), 1);
    }

    #[wasm_bindgen_test]
    fn advance_remaining_counts_pending_including_self() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None), item(200, None), item(300, None)]);
        q.reprioritize(0, true);
        // First dispatch: 3 items pending at the moment it's measured.
        match q.advance(false) {
            QueueAction::StartDownload { remaining, .. } => assert_eq!(remaining, 3),
            other => panic!("expected dispatch, got {other:?}"),
        }
        // Second dispatch: 2 remain.
        match q.advance(false) {
            QueueAction::StartDownload { remaining, .. } => assert_eq!(remaining, 2),
            other => panic!("expected dispatch, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn advance_complete_only_when_nothing_pending_or_active() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None)]);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));
        // In flight, nothing pending → Saturated (NOT Complete), since active>0.
        assert!(matches!(q.advance(false), QueueAction::Saturated));
        q.mark_active_done(100);
        // Now nothing pending and nothing active → Complete (and queue cleared).
        assert!(matches!(q.advance(false), QueueAction::Complete));
        assert!(
            q.find_by_scan_start(100).is_none(),
            "Complete clears the queue"
        );
    }

    #[wasm_bindgen_test]
    fn advance_ties_dispatch_in_enqueue_order() {
        let mut q = DownloadQueueManager::new();
        // Distinct scan_starts but all equally far ahead of the playhead → same
        // priority; min_by_key is stable, so enqueue order wins the tie.
        q.enqueue([item(1000, None), item(2000, None), item(3000, None)]);
        // Playhead far behind all; forward. Priorities differ by distance, so to
        // force a tie, reprioritize against a playhead that makes them equal is
        // hard — instead leave priorities at default 0 (no reprioritize).
        let mut order = Vec::new();
        while let QueueAction::StartDownload { scan_start, .. } = q.advance(false) {
            order.push(scan_start);
            q.mark_active_done(scan_start);
        }
        // All priority 0 (default) → enqueue order preserved.
        assert_eq!(order, vec![1000, 2000, 3000]);
    }

    // ── mark_active_done idempotency / state guards ─────────────────────────

    #[wasm_bindgen_test]
    fn mark_active_done_is_noop_on_pending() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None)]);
        // Item is Pending, not Active — marking done must NOT touch it.
        q.mark_active_done(100);
        assert_eq!(q.pending_items().count(), 1, "Pending item untouched");
        // It still dispatches normally.
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));
    }

    #[wasm_bindgen_test]
    fn mark_active_done_is_idempotent_and_ignores_unknown() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None)]);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload { .. }
        ));
        q.mark_active_done(100);
        assert_eq!(q.active_count(), 0);
        // Second call is a no-op (already Done), and an unknown scan_start too.
        q.mark_active_done(100);
        q.mark_active_done(424242);
        assert_eq!(q.active_count(), 0);
    }

    // ── find_by_scan_start ──────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn find_by_scan_start_returns_none_when_absent() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None)]);
        assert!(q.find_by_scan_start(100).is_some());
        assert!(q.find_by_scan_start(101).is_none());
    }

    // ── reprioritize only touches Pending ───────────────────────────────────

    #[wasm_bindgen_test]
    fn reprioritize_skips_active_items() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(1000, None), item(5000, None)]);
        // Dispatch the nearest (1000) so it's Active.
        q.reprioritize(900, true);
        assert!(matches!(
            q.advance(false),
            QueueAction::StartDownload {
                scan_start: 1000,
                ..
            }
        ));
        // Reprioritize with a far playhead. The Active 1000 must keep whatever it
        // had; the Pending 5000 gets its priority recomputed.
        q.reprioritize(4900, true);
        // 5000 covers... playhead 4900 < scan_start 5000 → dist 100, ahead → 100.
        let pending: Vec<_> = q.pending_items().collect();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].scan_start, 5000);
        assert_eq!(pending[0].priority, 100);
    }

    // ── playhead_priority edge / boundary cases ─────────────────────────────

    #[wasm_bindgen_test]
    fn playhead_priority_boundaries_are_inclusive_zero() {
        // playhead exactly at scan_start → inside window → 0.
        assert_eq!(playhead_priority(100, 400, 100, true), 0);
        // playhead exactly at scan_end → inside window → 0.
        assert_eq!(playhead_priority(100, 400, 400, true), 0);
        // playhead one before start: ahead forward, plain distance 1.
        assert_eq!(playhead_priority(100, 400, 99, true), 1);
        // playhead one after end, forward → behind → 4x penalty.
        assert_eq!(playhead_priority(100, 400, 401, true), 4);
    }

    #[wasm_bindgen_test]
    fn playhead_priority_ahead_boundary_scan_start_eq_playhead_is_inside() {
        // When playhead < scan_start is false AND playhead > scan_end is false,
        // we're inside → 0, regardless of the `ahead` test. Verify the just-ahead
        // case (playhead just below scan_start) is treated as ahead forward.
        // scan_start == playhead is inside (covered above); test scan ahead by 1.
        assert_eq!(playhead_priority(200, 500, 199, true), 1); // ahead, plain
                                                               // Backward: scan_end <= playhead defines "ahead". scan ends below playhead.
        assert_eq!(playhead_priority(100, 150, 200, false), 50); // ahead backward, plain
                                                                 // Backward against direction: scan starts above playhead → 4x.
        assert_eq!(playhead_priority(300, 600, 200, false), 400); // dist 100 *4
    }

    #[wasm_bindgen_test]
    fn playhead_priority_saturates_on_overflow() {
        // A behind scan whose 4x distance would overflow i64 must saturate, not
        // panic. dist = i64::MAX (playhead far above scan_end), behind forward.
        // scan_end = 0, playhead = i64::MAX → dist = i64::MAX; *4 saturates.
        let p = playhead_priority(-10, 0, i64::MAX, true);
        assert_eq!(p, i64::MAX, "4x penalty saturates at i64::MAX");
    }

    // ── prefetch_window: paused ignores speed; playing takes the max ─────────

    #[wasm_bindgen_test]
    fn prefetch_window_paused_lead_independent_of_speed() {
        let trail = 2.0 * crate::FALLBACK_SCAN_DURATION_SECS as f64;
        // Huge speed but paused → lead is the fixed PAUSED constant, not scaled.
        let (s, e) = prefetch_window(0.0, 1_000_000.0, false, true);
        assert_eq!(s, (-trail) as i64);
        assert_eq!(e, crate::PREFETCH_LOOKAHEAD_SECS_PAUSED as i64);
    }

    #[wasm_bindgen_test]
    fn prefetch_window_playing_slow_floors_at_paused_lead() {
        // Playing but slow: speed*PLAY_LEAD < PAUSED, so .max() picks PAUSED.
        // speed 1.0 * PREFETCH_PLAY_LEAD_SECS(4.0) = 4.0 < 600.0.
        let (_, e) = prefetch_window(0.0, 1.0, true, true);
        assert_eq!(e, crate::PREFETCH_LOOKAHEAD_SECS_PAUSED as i64);
    }

    #[wasm_bindgen_test]
    fn prefetch_window_playing_fast_backward_mirrors_lead() {
        let trail = 2.0 * crate::FALLBACK_SCAN_DURATION_SECS as f64;
        let speed = 1000.0;
        let expected_lead = speed * crate::PREFETCH_PLAY_LEAD_SECS; // 4000 > 600
        let (s, e) = prefetch_window(50_000.0, speed, true, false);
        // Backward: lead extends behind, trail ahead.
        assert_eq!(s, (50_000.0 - expected_lead) as i64);
        assert_eq!(e, (50_000.0 + trail) as i64);
    }

    // ── prune_pending: keep-all and drop-all extremes ───────────────────────

    #[wasm_bindgen_test]
    fn prune_pending_keep_all_returns_nothing() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None), item(200, None)]);
        let pruned = q.prune_pending(|_| true);
        assert!(pruned.is_empty());
        assert_eq!(q.pending_items().count(), 2);
    }

    #[wasm_bindgen_test]
    fn prune_pending_drop_all_returns_every_pending() {
        let mut q = DownloadQueueManager::new();
        q.enqueue([item(100, None), item(200, None)]);
        let pruned = q.prune_pending(|_| false);
        let mut starts: Vec<i64> = pruned.iter().map(|i| i.scan_start).collect();
        starts.sort();
        assert_eq!(starts, vec![100, 200]);
        assert_eq!(q.pending_items().count(), 0);
        assert!(!q.has_work());
    }
}
