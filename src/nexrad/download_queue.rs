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
        }
    }
}

/// Action the caller should take after a queue operation.
#[allow(dead_code)]
pub(crate) enum QueueAction {
    /// Start downloading a specific file.
    StartDownload {
        idx: usize,
        date: chrono::NaiveDate,
        file_name: String,
        scan_start: i64,
        scan_end: i64,
        elevation_filter: Option<u8>,
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

    /// Advance the queue: start the next pending item if a concurrency slot
    /// is available and the queue is not paused.
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
            .position(|item| matches!(item.state, QueueItemState::Pending));

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
