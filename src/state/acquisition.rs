//! Unified acquisition state: tracks all data acquisition operations (archive downloads,
//! realtime streaming, backfill) and correlates them with service worker network requests.

use std::collections::VecDeque;

use super::DownloadPhase;

/// Unique identifier for an acquisition operation.
pub type OperationId = u64;

/// Maximum operations retained in the ring buffer.
const MAX_RETAINED: usize = 200;

/// The kind of acquisition operation.
#[derive(Clone, Debug, PartialEq)]
pub enum OperationKind {
    /// Archive listing fetch (S3 LIST).
    ArchiveListing {
        site_id: String,
        date: chrono::NaiveDate,
    },
    /// Archive scan download (S3 GET for a volume file).
    ArchiveDownload {
        site_id: String,
        file_name: String,
        scan_start: i64,
        scan_end: i64,
    },
    /// Realtime chunk acquisition.
    RealtimeChunk {
        site_id: String,
        chunk_index: u32,
        is_start: bool,
        is_end: bool,
        /// Volume start timestamp (Unix seconds) shared by all chunks in the same scan.
        scan_timestamp: i64,
    },
    /// Backfill chunk download during initial volume load.
    BackfillChunk { site_id: String, chunk_index: u32 },
}

/// Key for grouping network requests in the drawer's Network tab.
///
/// Realtime chunks are grouped by scan (site + timestamp) so that all chunks
/// in the same volume appear under one collapsible header.  Other operations
/// are keyed by their individual `OperationId`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NetworkGroupKey {
    /// A single acquisition operation (archive download, listing, backfill).
    Operation(OperationId),
    /// All realtime chunks sharing the same volume/scan timestamp.
    RealtimeScan { site_id: String, scan_timestamp: i64 },
    /// Requests not correlated to any operation.
    Ungrouped,
}

/// Status of an acquisition operation.
#[derive(Clone, Debug, PartialEq)]
pub enum OperationStatus {
    /// Waiting in queue.
    Queued,
    /// Currently downloading/processing.
    Active,
    /// Successfully completed.
    Completed { duration_ms: f64, bytes: u64 },
    /// Failed with an error message.
    Failed { error: String },
    /// Cancelled by user or selection change.
    Cancelled,
}

/// A single acquisition operation.
#[derive(Clone, Debug)]
pub struct AcquisitionOperation {
    pub id: OperationId,
    pub kind: OperationKind,
    pub status: OperationStatus,
    pub created_at_ms: f64,
    pub started_at_ms: Option<f64>,
    pub completed_at_ms: Option<f64>,
    /// Indices into `recent_network_requests` correlated to this operation.
    pub network_request_ids: Vec<usize>,
    /// Current pipeline phase.
    pub phase: DownloadPhase,
}

/// Per-chunk latency metrics for streaming mode.
#[derive(Clone, Debug)]
pub struct ChunkLatencyMetrics {
    pub chunk_index: u32,
    pub first_radial_time_secs: Option<f64>,
    pub last_radial_time_secs: Option<f64>,
    /// Time to download the chunk from S3 (ms).
    pub fetch_latency_ms: f64,
    /// Wall-clock time when download completed (ms since epoch).
    pub download_complete_time_ms: f64,
    /// Computed: download_complete - first_radial_time (ms). Radar collection to app.
    pub end_to_end_latency_ms: Option<f64>,
}

/// State of the acquisition queue.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum QueueState {
    /// Queue is processing items.
    Running,
    /// User-initiated pause.
    Paused,
    /// Paused due to a failed operation.
    ErrorPaused,
    /// No operations in queue.
    #[default]
    Empty,
}

/// Which tab is active in the acquisition drawer.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum DrawerTab {
    #[default]
    Queue,
    Network,
}

/// Latency summary statistics.
#[derive(Clone, Debug, Default)]
pub struct LatencySummary {
    pub avg_fetch_ms: f64,
    pub p50_fetch_ms: f64,
    pub p95_fetch_ms: f64,
    pub avg_e2e_ms: Option<f64>,
}

/// Root acquisition state, lives on `AppState`.
pub struct AcquisitionState {
    /// Monotonically increasing operation ID counter.
    next_id: OperationId,
    /// All operations, ordered by creation time. Ring buffer of last MAX_RETAINED.
    pub operations: VecDeque<AcquisitionOperation>,
    /// Current queue state.
    pub queue_state: QueueState,
    /// The operation that caused an error-pause, if any.
    pub error_pause_operation_id: Option<OperationId>,
    /// Whether the acquisition drawer is expanded.
    pub drawer_expanded: bool,
    /// Drawer height in pixels (user-resizable).
    pub drawer_height: f32,
    /// Active drawer tab.
    pub active_tab: DrawerTab,
    /// Per-chunk latency metrics for the current streaming session.
    pub chunk_latencies: Vec<ChunkLatencyMetrics>,
    /// Set of expanded network groups in the drawer.
    pub expanded_network_groups: std::collections::HashSet<NetworkGroupKey>,
}

impl Default for AcquisitionState {
    fn default() -> Self {
        Self {
            next_id: 1,
            operations: VecDeque::with_capacity(MAX_RETAINED),
            queue_state: QueueState::Empty,
            error_pause_operation_id: None,
            drawer_expanded: false,
            drawer_height: 250.0,
            active_tab: DrawerTab::Queue,
            chunk_latencies: Vec::new(),
            expanded_network_groups: std::collections::HashSet::<NetworkGroupKey>::new(),
        }
    }
}

impl AcquisitionState {
    /// Create a new operation and return its ID.
    pub fn create_operation(&mut self, kind: OperationKind) -> OperationId {
        let id = self.next_id;
        self.next_id += 1;

        let op = AcquisitionOperation {
            id,
            kind,
            status: OperationStatus::Queued,
            created_at_ms: js_sys::Date::now(),
            started_at_ms: None,
            completed_at_ms: None,
            network_request_ids: Vec::new(),
            phase: DownloadPhase::Idle,
        };

        // Evict oldest if at capacity
        if self.operations.len() >= MAX_RETAINED {
            self.operations.pop_front();
        }
        self.operations.push_back(op);

        if self.queue_state == QueueState::Empty {
            self.queue_state = QueueState::Running;
        }

        id
    }

    /// Mark an operation as active (download started).
    pub fn mark_active(&mut self, id: OperationId) {
        if let Some(op) = self.find_mut(id) {
            op.status = OperationStatus::Active;
            op.started_at_ms = Some(js_sys::Date::now());
            op.phase = DownloadPhase::Downloading;
        }
    }

    /// Update the phase of an active operation.
    pub fn set_phase(&mut self, id: OperationId, phase: DownloadPhase) {
        if let Some(op) = self.find_mut(id) {
            op.phase = phase;
        }
    }

    /// Mark an operation as completed.
    pub fn mark_completed(&mut self, id: OperationId, bytes: u64) {
        let now = js_sys::Date::now();
        if let Some(op) = self.find_mut(id) {
            let duration_ms = op.started_at_ms.map(|s| now - s).unwrap_or(0.0);
            op.status = OperationStatus::Completed { duration_ms, bytes };
            op.completed_at_ms = Some(now);
            op.phase = DownloadPhase::Done;
        }
        self.update_queue_state();
    }

    /// Mark an operation as failed and enter error-pause.
    pub fn mark_failed(&mut self, id: OperationId, error: String) {
        if let Some(op) = self.find_mut(id) {
            op.status = OperationStatus::Failed { error };
            op.completed_at_ms = Some(js_sys::Date::now());
            op.phase = DownloadPhase::Done;
        }
        self.queue_state = QueueState::ErrorPaused;
        self.error_pause_operation_id = Some(id);
        self.drawer_expanded = true;
    }

    /// Cancel a specific operation.
    pub fn cancel_operation(&mut self, id: OperationId) {
        if let Some(op) = self.find_mut(id) {
            op.status = OperationStatus::Cancelled;
            op.completed_at_ms = Some(js_sys::Date::now());
        }
        self.update_queue_state();
    }

    /// Cancel all queued operations (e.g., on selection change).
    pub fn cancel_all_queued(&mut self) {
        let now = js_sys::Date::now();
        for op in self.operations.iter_mut() {
            if op.status == OperationStatus::Queued {
                op.status = OperationStatus::Cancelled;
                op.completed_at_ms = Some(now);
            }
        }
        self.update_queue_state();
    }

    /// Cancel all pending and active operations (selection change: cancel all + rebuild).
    pub fn cancel_all(&mut self) {
        let now = js_sys::Date::now();
        for op in self.operations.iter_mut() {
            match &op.status {
                OperationStatus::Queued | OperationStatus::Active => {
                    op.status = OperationStatus::Cancelled;
                    op.completed_at_ms = Some(now);
                }
                _ => {}
            }
        }
        self.queue_state = QueueState::Empty;
        self.error_pause_operation_id = None;
    }

    /// Retry a failed operation: reset to Queued, move to front of queue.
    pub fn retry_failed(&mut self, id: OperationId) {
        if let Some(op) = self.find_mut(id) {
            op.status = OperationStatus::Queued;
            op.started_at_ms = None;
            op.completed_at_ms = None;
            op.phase = DownloadPhase::Idle;
        }
        // Move to front of pending operations
        if let Some(idx) = self.operations.iter().position(|o| o.id == id) {
            if let Some(op) = self.operations.remove(idx) {
                // Find the first queued/active position and insert before it
                let insert_pos = self
                    .operations
                    .iter()
                    .position(|o| {
                        matches!(o.status, OperationStatus::Queued | OperationStatus::Active)
                    })
                    .unwrap_or(self.operations.len());
                self.operations.insert(insert_pos, op);
            }
        }
        self.queue_state = QueueState::Running;
        self.error_pause_operation_id = None;
    }

    /// Skip a failed operation: mark as cancelled and resume queue.
    pub fn skip_failed(&mut self, id: OperationId) {
        self.cancel_operation(id);
        self.queue_state = QueueState::Running;
        self.error_pause_operation_id = None;
    }

    /// Resume a paused queue.
    pub fn resume(&mut self) {
        if matches!(
            self.queue_state,
            QueueState::Paused | QueueState::ErrorPaused
        ) {
            self.queue_state = QueueState::Running;
            self.error_pause_operation_id = None;
        }
    }

    /// Pause the queue.
    pub fn pause(&mut self) {
        if self.queue_state == QueueState::Running {
            self.queue_state = QueueState::Paused;
        }
    }

    /// Reorder an operation by a delta (-1 = move up, +1 = move down).
    pub fn reorder_operation(&mut self, id: OperationId, delta: isize) {
        if let Some(idx) = self.operations.iter().position(|o| o.id == id) {
            let new_idx =
                (idx as isize + delta).clamp(0, self.operations.len() as isize - 1) as usize;
            if new_idx != idx {
                if let Some(op) = self.operations.remove(idx) {
                    self.operations.insert(new_idx, op);
                }
            }
        }
    }

    /// Number of queued operations.
    pub fn queued_count(&self) -> usize {
        self.operations
            .iter()
            .filter(|o| o.status == OperationStatus::Queued)
            .count()
    }

    /// Number of active operations.
    pub fn active_count(&self) -> usize {
        self.operations
            .iter()
            .filter(|o| o.status == OperationStatus::Active)
            .count()
    }

    /// Whether there are any active or queued operations.
    pub fn has_active_operations(&self) -> bool {
        self.operations
            .iter()
            .any(|o| matches!(o.status, OperationStatus::Queued | OperationStatus::Active))
    }

    /// Correlate a network request URL with an active/recent operation.
    /// Returns the matching operation ID, if any.
    pub fn correlate_network_request(&self, url: &str) -> Option<OperationId> {
        // Search active operations first (most recent first)
        for op in self.operations.iter().rev() {
            if !matches!(
                op.status,
                OperationStatus::Active | OperationStatus::Completed { .. }
            ) {
                continue;
            }
            if self.url_matches_operation(url, &op.kind) {
                return Some(op.id);
            }
        }
        None
    }

    /// Check if a URL matches an operation kind.
    fn url_matches_operation(&self, url: &str, kind: &OperationKind) -> bool {
        match kind {
            OperationKind::ArchiveDownload { file_name, .. } => {
                // Archive download URLs contain the file name
                url.contains(file_name.as_str())
            }
            OperationKind::ArchiveListing { site_id, date } => {
                // Listing URLs contain the site ID and date prefix
                let date_prefix = date.format("%Y/%m/%d").to_string();
                url.contains(&date_prefix) && url.contains(site_id.as_str())
            }
            OperationKind::RealtimeChunk { site_id, .. } => {
                // Realtime chunk URLs are on the chunks bucket and contain the site ID
                url.contains("nexrad-level2-chunks") && url.contains(site_id.as_str())
            }
            OperationKind::BackfillChunk { site_id, .. } => {
                url.contains("nexrad-level2-chunks") && url.contains(site_id.as_str())
            }
        }
    }

    /// Record per-chunk latency metrics from a streaming result.
    pub fn record_chunk_latency(
        &mut self,
        chunk_index: u32,
        fetch_latency_ms: f64,
        first_radial_secs: Option<f64>,
        last_radial_secs: Option<f64>,
    ) {
        let now_ms = js_sys::Date::now();
        let metrics = ChunkLatencyMetrics {
            chunk_index,
            first_radial_time_secs: first_radial_secs,
            last_radial_time_secs: last_radial_secs,
            fetch_latency_ms,
            download_complete_time_ms: now_ms,
            end_to_end_latency_ms: first_radial_secs.map(|frs| (now_ms / 1000.0 - frs) * 1000.0),
        };
        self.chunk_latencies.push(metrics);
    }

    /// Compute latency summary statistics from chunk latencies.
    pub fn latency_summary(&self) -> Option<LatencySummary> {
        if self.chunk_latencies.is_empty() {
            return None;
        }

        let mut fetch_values: Vec<f64> = self
            .chunk_latencies
            .iter()
            .map(|c| c.fetch_latency_ms)
            .collect();
        fetch_values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let n = fetch_values.len();
        let avg_fetch = fetch_values.iter().sum::<f64>() / n as f64;
        let p50_fetch = fetch_values[n / 2];
        let p95_fetch = fetch_values[(n as f64 * 0.95) as usize];

        let e2e_values: Vec<f64> = self
            .chunk_latencies
            .iter()
            .filter_map(|c| c.end_to_end_latency_ms)
            .collect();
        let avg_e2e = if e2e_values.is_empty() {
            None
        } else {
            Some(e2e_values.iter().sum::<f64>() / e2e_values.len() as f64)
        };

        Some(LatencySummary {
            avg_fetch_ms: avg_fetch,
            p50_fetch_ms: p50_fetch,
            p95_fetch_ms: p95_fetch,
            avg_e2e_ms: avg_e2e,
        })
    }

    /// Clear streaming latency data (e.g., when stopping live mode).
    pub fn clear_latencies(&mut self) {
        self.chunk_latencies.clear();
    }

    /// Get a short description for an operation kind.
    pub fn operation_description(kind: &OperationKind) -> String {
        match kind {
            OperationKind::ArchiveListing { site_id, date } => {
                format!("List {} {}", site_id, date)
            }
            OperationKind::ArchiveDownload {
                site_id, file_name, ..
            } => {
                // Extract time portion from file name if possible
                let time_part = file_name
                    .find('_')
                    .and_then(|i| file_name.get(i + 1..i + 7))
                    .map(|t| format!("{}:{}:{}", &t[0..2], &t[2..4], &t[4..6]))
                    .unwrap_or_else(|| file_name.clone());
                format!("{} {}", site_id, time_part)
            }
            OperationKind::RealtimeChunk {
                site_id,
                chunk_index,
                scan_timestamp,
                ..
            } => {
                // Format scan timestamp as HH:MM:SS UTC for display
                let dt = chrono::DateTime::from_timestamp(*scan_timestamp, 0);
                if let Some(dt) = dt {
                    format!(
                        "{} live {} chunk #{}",
                        site_id,
                        dt.format("%H:%M:%S"),
                        chunk_index
                    )
                } else {
                    format!("{} chunk #{}", site_id, chunk_index)
                }
            }
            OperationKind::BackfillChunk {
                site_id,
                chunk_index,
            } => {
                format!("{} backfill #{}", site_id, chunk_index)
            }
        }
    }

    /// Return the `NetworkGroupKey` for an operation.
    ///
    /// Realtime chunks get grouped by scan timestamp; everything else
    /// by operation ID.
    pub fn network_group_key(op: &AcquisitionOperation) -> NetworkGroupKey {
        match &op.kind {
            OperationKind::RealtimeChunk {
                site_id,
                scan_timestamp,
                ..
            } => NetworkGroupKey::RealtimeScan {
                site_id: site_id.clone(),
                scan_timestamp: *scan_timestamp,
            },
            _ => NetworkGroupKey::Operation(op.id),
        }
    }

    /// Return a scan-level group key for an operation kind.
    ///
    /// For realtime chunks this returns `Some((site_id, scan_timestamp))` so
    /// that all chunks belonging to the same volume are grouped together in
    /// the network tab. For other operation kinds returns `None`.
    pub fn scan_group_key(kind: &OperationKind) -> Option<(String, i64)> {
        match kind {
            OperationKind::RealtimeChunk {
                site_id,
                scan_timestamp,
                ..
            } => Some((site_id.clone(), *scan_timestamp)),
            _ => None,
        }
    }

    /// Human-readable description for a scan-level group (all chunks sharing
    /// the same `scan_timestamp`).
    pub fn scan_group_description(site_id: &str, scan_timestamp: i64) -> String {
        let dt = chrono::DateTime::from_timestamp(scan_timestamp, 0);
        if let Some(dt) = dt {
            format!("{} live scan {}", site_id, dt.format("%H:%M:%SZ"))
        } else {
            format!("{} live scan {}", site_id, scan_timestamp)
        }
    }

    /// Find an operation by ID (mutable).
    fn find_mut(&mut self, id: OperationId) -> Option<&mut AcquisitionOperation> {
        self.operations.iter_mut().find(|o| o.id == id)
    }

    /// Find an operation by ID (immutable).
    pub fn find(&self, id: OperationId) -> Option<&AcquisitionOperation> {
        self.operations.iter().find(|o| o.id == id)
    }

    /// Update queue state based on remaining operations.
    fn update_queue_state(&mut self) {
        if !self.has_active_operations()
            && !matches!(
                self.queue_state,
                QueueState::Paused | QueueState::ErrorPaused
            )
        {
            self.queue_state = QueueState::Empty;
        }
    }

    /// Get the next queued operation ID (for the download pump to start).
    pub fn next_queued_id(&self) -> Option<OperationId> {
        self.operations
            .iter()
            .find(|o| o.status == OperationStatus::Queued)
            .map(|o| o.id)
    }

    /// Whether the queue is paused (user or error).
    pub fn is_paused(&self) -> bool {
        matches!(
            self.queue_state,
            QueueState::Paused | QueueState::ErrorPaused
        )
    }
}
