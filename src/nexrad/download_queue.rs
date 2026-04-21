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
}

impl QueueItem {
    pub fn new(date: chrono::NaiveDate, file_name: String, scan_start: i64, scan_end: i64) -> Self {
        Self {
            date,
            file_name,
            scan_start,
            scan_end,
            state: QueueItemState::Pending,
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
}

impl DownloadQueueManager {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            active_operation_ids: std::collections::HashMap::new(),
            max_parallel: DEFAULT_MAX_PARALLEL,
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

    /// Replace the queue with a new set of items.
    ///
    /// Clears all active operation IDs and sets the new queue.
    /// Items are **not** automatically started — call `advance()` in a loop
    /// to saturate the concurrency limit.
    pub fn set_queue(&mut self, items: Vec<QueueItem>) {
        self.active_operation_ids.clear();
        self.queue = items;
    }

    /// Get progress info: `(completed_count, total_count, pending_scans, active_scans)`.
    #[allow(clippy::type_complexity, dead_code)]
    pub fn progress(&self) -> (u32, u32, Vec<(i64, i64)>, Vec<(i64, i64)>) {
        let total = self.queue.len() as u32;
        let completed = self
            .queue
            .iter()
            .filter(|item| matches!(item.state, QueueItemState::Done))
            .count() as u32;
        let pending_scans: Vec<(i64, i64)> = self
            .queue
            .iter()
            .map(|item| (item.scan_start, item.scan_end))
            .collect();
        let active_scans: Vec<(i64, i64)> = self
            .active_items()
            .map(|item| (item.scan_start, item.scan_end))
            .collect();
        (completed, total, pending_scans, active_scans)
    }

    /// Get current queue length.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Get all items for read access.
    pub fn items(&self) -> &[QueueItem] {
        &self.queue
    }

    /// Clear the queue and all tracked operation IDs.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.active_operation_ids.clear();
    }

    /// Get the operation ID for the active download with the given scan_start.
    #[allow(dead_code)]
    pub fn operation_id(&self, scan_start: i64) -> Option<crate::state::OperationId> {
        self.active_operation_ids.get(&scan_start).copied()
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
}
