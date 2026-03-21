//! Download queue manager for serial archive downloads.
//!
//! Encapsulates the queue of files to download and the state machine for
//! advancing through them. The manager does NOT perform downloads or access
//! network channels — it returns [`QueueAction`] values telling the caller
//! what to do next.

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
    /// The active download is still in progress — do nothing.
    StillDownloading,
    /// Queue is paused — do nothing.
    Paused,
}

/// Manages the download queue state machine.
///
/// This struct owns the queue of [`QueueItem`]s and the active operation ID.
/// It does **not** hold references to download channels or data facades —
/// the caller acts on the returned [`QueueAction`].
pub(crate) struct DownloadQueueManager {
    queue: Vec<QueueItem>,
    active_operation_id: Option<crate::state::OperationId>,
}

impl DownloadQueueManager {
    pub fn new() -> Self {
        Self {
            queue: Vec::new(),
            active_operation_id: None,
        }
    }

    /// Check if the queue has any active or pending items.
    pub fn has_work(&self) -> bool {
        self.queue
            .iter()
            .any(|item| matches!(item.state, QueueItemState::Pending | QueueItemState::Active))
    }

    /// Mark the currently active item as done (download completed).
    ///
    /// Finds the active item whose `scan_start` matches and transitions it to `Done`.
    pub fn mark_active_done(&mut self, scan_start: i64) {
        if let Some(item) = self.queue.iter_mut().find(|item| {
            matches!(item.state, QueueItemState::Active) && item.scan_start == scan_start
        }) {
            item.state = QueueItemState::Done;
        }
    }

    /// Find the currently active item (if any).
    pub fn active_item(&self) -> Option<&QueueItem> {
        self.queue
            .iter()
            .find(|item| matches!(item.state, QueueItemState::Active))
    }

    /// Advance the queue: find next pending item and return an action.
    ///
    /// Call this after `mark_active_done` to start the next download.
    /// `is_paused` should reflect whether the acquisition system is paused.
    pub fn advance(&mut self, is_paused: bool) -> QueueAction {
        if is_paused {
            return QueueAction::Paused;
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
        } else {
            // All items are Done/Failed — queue drained
            self.queue.clear();
            QueueAction::Complete
        }
    }

    /// Replace the queue with a new set of items.
    ///
    /// Clears the active operation ID and sets the new queue.
    /// The first item is **not** automatically started — call `advance()` or
    /// `start_first()` after this.
    pub fn set_queue(&mut self, items: Vec<QueueItem>) {
        self.active_operation_id = None;
        self.queue = items;
    }

    /// Start the first item in the queue, returning the action.
    ///
    /// This is a convenience for the initial download kick-off after `set_queue`.
    pub fn start_first(&mut self) -> Option<QueueAction> {
        if self.queue.is_empty() {
            return None;
        }
        let item = &self.queue[0];
        let action = QueueAction::StartDownload {
            idx: 0,
            date: item.date,
            file_name: item.file_name.clone(),
            scan_start: item.scan_start,
            scan_end: item.scan_end,
            remaining: self.queue.len(),
        };
        self.queue[0].state = QueueItemState::Active;
        Some(action)
    }

    /// Get progress info: `(completed_count, total_count, pending_scans, active_scan)`.
    #[allow(clippy::type_complexity, dead_code)]
    pub fn progress(&self) -> (u32, u32, Vec<(i64, i64)>, Option<(i64, i64)>) {
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
        let active_scan = self
            .active_item()
            .map(|item| (item.scan_start, item.scan_end));
        (completed, total, pending_scans, active_scan)
    }

    /// Get current queue length.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Get all items for read access.
    pub fn items(&self) -> &[QueueItem] {
        &self.queue
    }

    /// Clear the queue and reset the active operation ID.
    pub fn clear(&mut self) {
        self.queue.clear();
        self.active_operation_id = None;
    }

    /// Get the active download operation ID.
    #[allow(dead_code)]
    pub fn active_operation_id(&self) -> Option<crate::state::OperationId> {
        self.active_operation_id
    }

    /// Set the active download operation ID.
    pub fn set_active_operation_id(&mut self, id: Option<crate::state::OperationId>) {
        self.active_operation_id = id;
    }

    /// Take the active operation ID, leaving `None` in its place.
    pub fn take_active_operation_id(&mut self) -> Option<crate::state::OperationId> {
        self.active_operation_id.take()
    }

    /// Find an item by scan_start timestamp.
    pub fn find_by_scan_start(&self, scan_start: i64) -> Option<&QueueItem> {
        self.queue.iter().find(|item| item.scan_start == scan_start)
    }
}
