//! Unified acquisition state: tracks all data acquisition operations (archive downloads,
//! realtime streaming) and correlates them with service worker network requests.

use std::collections::VecDeque;

use crate::core::{AcquisitionOperation, OperationId, OperationKind, OperationStatus};

/// Maximum operations retained in the ring buffer.
const MAX_RETAINED: usize = 200;

/// Key for grouping network requests in the drawer's Network tab.
///
/// Realtime chunks are grouped by scan (site + timestamp) so that all chunks
/// in the same volume appear under one collapsible header.  Other operations
/// are keyed by their individual `OperationId`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum NetworkGroupKey {
    /// A single acquisition operation (archive download, listing, realtime).
    Operation(OperationId),
    /// All realtime chunks sharing the same volume/scan timestamp.
    RealtimeScan {
        site_id: String,
        scan_timestamp: i64,
    },
    /// Requests not correlated to any operation.
    Ungrouped,
}

/// Per-chunk latency metrics for streaming mode.
#[derive(Clone, Debug)]
pub(crate) struct ChunkLatencyMetrics {
    #[allow(dead_code)] // Sample identity; asserted by tests, drawer plots fetch_latency_ms only.
    pub chunk_index: u32,
    /// Time to download the chunk from S3 (ms).
    pub fetch_latency_ms: f64,
    /// Computed: download_complete - first_radial_time (ms). Radar collection to app.
    pub end_to_end_latency_ms: Option<f64>,
}

/// State of the acquisition queue.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) enum QueueState {
    /// Queue is processing items.
    Running,
    /// User-initiated pause.
    Paused,
    /// No operations in queue.
    #[default]
    Empty,
}

/// Which tab is active in the acquisition drawer.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) enum DrawerTab {
    #[default]
    Queue,
    Network,
}

/// Latency summary statistics.
#[derive(Clone, Debug, Default)]
pub(crate) struct LatencySummary {
    pub avg_fetch_ms: f64,
    #[allow(dead_code)] // Computed with avg/p95 (tests pin the math); drawer displays avg+p95.
    pub p50_fetch_ms: f64,
    pub p95_fetch_ms: f64,
    pub avg_e2e_ms: Option<f64>,
}

/// Root acquisition state, lives on `AppState`.
pub(crate) struct AcquisitionState {
    /// Monotonically increasing operation ID counter.
    next_id: OperationId,
    /// All operations, ordered by creation time. Ring buffer of last MAX_RETAINED.
    pub operations: VecDeque<AcquisitionOperation>,
    /// Current queue state.
    pub queue_state: QueueState,
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
    pub(crate) fn create_operation(&mut self, kind: OperationKind) -> OperationId {
        let id = self.next_id;
        self.next_id += 1;

        let op = AcquisitionOperation {
            id,
            kind,
            status: OperationStatus::Queued,
            started_at_ms: None,
            completed_at_ms: None,
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
    pub(crate) fn mark_active(&mut self, id: OperationId) {
        if let Some(op) = self.find_mut(id) {
            op.status = OperationStatus::Active;
            op.started_at_ms = Some(js_sys::Date::now());
        }
    }

    /// Mark an operation as completed.
    pub(crate) fn mark_completed(&mut self, id: OperationId, bytes: u64) {
        let now = js_sys::Date::now();
        if let Some(op) = self.find_mut(id) {
            let duration_ms = op.started_at_ms.map(|s| now - s).unwrap_or(0.0);
            op.status = OperationStatus::Completed { duration_ms, bytes };
            op.completed_at_ms = Some(now);
        }
        self.update_queue_state();
    }

    /// Mark an operation as failed.
    ///
    /// Failures are **local and recoverable** (alignment §5 failure model):
    /// one scan's failure does not pause the whole queue — other queued items
    /// keep dispatching, and the failed cell surfaces an alert tick + retry on
    /// the strip / queue sheet. We deliberately do NOT pause the whole queue or
    /// auto-expand the drawer here; `update_queue_state` keeps the queue
    /// Running/Empty as appropriate.
    pub(crate) fn mark_failed(&mut self, id: OperationId, error: String) {
        if let Some(op) = self.find_mut(id) {
            op.status = OperationStatus::Failed { error };
            op.completed_at_ms = Some(js_sys::Date::now());
        }
        self.update_queue_state();
    }

    /// Cancel a specific operation.
    pub(crate) fn cancel_operation(&mut self, id: OperationId) {
        if let Some(op) = self.find_mut(id) {
            op.status = OperationStatus::Cancelled;
            op.completed_at_ms = Some(js_sys::Date::now());
        }
        self.update_queue_state();
    }

    /// Cancel all queued operations (e.g., on selection change).
    #[cfg(test)]
    pub(crate) fn cancel_all_queued(&mut self) {
        let now = js_sys::Date::now();
        for op in self.operations.iter_mut() {
            if op.status == OperationStatus::Queued {
                op.status = OperationStatus::Cancelled;
                op.completed_at_ms = Some(now);
            }
        }
        self.update_queue_state();
    }

    /// Cancel all pending and active operations (cache clear, selection
    /// change: cancel all + rebuild).
    pub(crate) fn cancel_all(&mut self) {
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
    }

    /// Retry a failed operation: reset to Queued, move to front of queue.
    pub(crate) fn retry_failed(&mut self, id: OperationId) {
        if let Some(op) = self.find_mut(id) {
            op.status = OperationStatus::Queued;
            op.started_at_ms = None;
            op.completed_at_ms = None;
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
    }

    /// Skip a failed operation: mark as cancelled and resume queue.
    pub(crate) fn skip_failed(&mut self, id: OperationId) {
        self.cancel_operation(id);
        self.queue_state = QueueState::Running;
    }

    /// Resume a paused queue.
    pub(crate) fn resume(&mut self) {
        if self.queue_state == QueueState::Paused {
            self.queue_state = QueueState::Running;
        }
    }

    /// Pause the queue.
    pub(crate) fn pause(&mut self) {
        if self.queue_state == QueueState::Running {
            self.queue_state = QueueState::Paused;
        }
    }

    /// Reorder an operation by a delta (-1 = move up, +1 = move down).
    pub(crate) fn reorder_operation(&mut self, id: OperationId, delta: isize) {
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
    pub(crate) fn queued_count(&self) -> usize {
        self.operations
            .iter()
            .filter(|o| o.status == OperationStatus::Queued)
            .count()
    }

    /// Number of active operations.
    // The activity surface counts stages in `core::activity` instead, so this
    // survives only as a container-level invariant the tests below pin.
    #[allow(dead_code)]
    pub(crate) fn active_count(&self) -> usize {
        self.operations
            .iter()
            .filter(|o| o.status == OperationStatus::Active)
            .count()
    }

    /// Whether there are any active or queued operations.
    pub(crate) fn has_active_operations(&self) -> bool {
        self.operations
            .iter()
            .any(|o| matches!(o.status, OperationStatus::Queued | OperationStatus::Active))
    }

    /// Correlate a network request URL with an active/recent operation.
    /// Returns the matching operation ID, if any.
    pub(crate) fn correlate_network_request(&self, url: &str) -> Option<OperationId> {
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
        }
    }

    /// Record per-chunk latency metrics from a streaming result.
    pub(crate) fn record_chunk_latency(
        &mut self,
        chunk_index: u32,
        fetch_latency_ms: f64,
        first_radial_secs: Option<f64>,
    ) {
        let now_ms = js_sys::Date::now();
        let metrics = ChunkLatencyMetrics {
            chunk_index,
            fetch_latency_ms,
            end_to_end_latency_ms: first_radial_secs.map(|frs| (now_ms / 1000.0 - frs) * 1000.0),
        };
        self.chunk_latencies.push(metrics);
    }

    /// Compute latency summary statistics from chunk latencies.
    pub(crate) fn latency_summary(&self) -> Option<LatencySummary> {
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
    #[cfg(test)]
    pub(crate) fn clear_latencies(&mut self) {
        self.chunk_latencies.clear();
    }

    /// Return the `NetworkGroupKey` for an operation.
    ///
    /// Realtime chunks get grouped by scan timestamp; everything else
    /// by operation ID.
    pub(crate) fn network_group_key(op: &AcquisitionOperation) -> NetworkGroupKey {
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
    #[cfg(test)]
    pub(crate) fn scan_group_key(kind: &OperationKind) -> Option<(String, i64)> {
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
    pub(crate) fn scan_group_description(site_id: &str, scan_timestamp: i64) -> String {
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
    pub(crate) fn find(&self, id: OperationId) -> Option<&AcquisitionOperation> {
        self.operations.iter().find(|o| o.id == id)
    }

    /// Update queue state based on remaining operations.
    fn update_queue_state(&mut self) {
        if !self.has_active_operations() && self.queue_state != QueueState::Paused {
            self.queue_state = QueueState::Empty;
        }
    }

    /// Scan-start seconds of every currently-Failed archive download. The
    /// frame-cell join uses these to mark failed cells (alert tick); because
    /// `retry_failed` flips the status back to Queued, the marker clears on
    /// retry — unlike the error ring, which never forgets.
    pub(crate) fn failed_scan_starts(&self) -> Vec<i64> {
        self.operations
            .iter()
            .filter_map(|op| match (&op.status, &op.kind) {
                (
                    OperationStatus::Failed { .. },
                    OperationKind::ArchiveDownload { scan_start, .. },
                ) => Some(*scan_start),
                _ => None,
            })
            .collect()
    }

    /// Ids of every currently-Failed archive download, oldest first — the
    /// targets of a "retry all" sweep.
    pub(crate) fn failed_operation_ids(&self) -> Vec<OperationId> {
        self.operations
            .iter()
            .filter(|op| {
                matches!(op.status, OperationStatus::Failed { .. })
                    && matches!(op.kind, OperationKind::ArchiveDownload { .. })
            })
            .map(|op| op.id)
            .collect()
    }

    /// The id of a Failed archive-download operation whose `scan_start` matches
    /// `scan_start_secs` within `tolerance_secs`. Used to wire a timeline
    /// failed-cell tick back to `Intent::RetryFailed`. Returns the most
    /// recent match (operations iterate oldest→newest).
    pub(crate) fn failed_operation_for_scan_start(
        &self,
        scan_start_secs: i64,
        tolerance_secs: i64,
    ) -> Option<OperationId> {
        self.operations
            .iter()
            .rev()
            .find(|op| {
                matches!(op.status, OperationStatus::Failed { .. })
                    && match &op.kind {
                        OperationKind::ArchiveDownload { scan_start, .. } => {
                            (scan_start - scan_start_secs).abs() <= tolerance_secs
                        }
                        _ => false,
                    }
            })
            .map(|op| op.id)
    }

    /// Get the next queued operation ID (for the download pump to start).
    #[cfg(test)]
    pub(crate) fn next_queued_id(&self) -> Option<OperationId> {
        self.operations
            .iter()
            .find(|o| o.status == OperationStatus::Queued)
            .map(|o| o.id)
    }

    /// Whether the queue is paused (user or error).
    pub(crate) fn is_paused(&self) -> bool {
        self.queue_state == QueueState::Paused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn download_kind(scan_start: i64) -> OperationKind {
        OperationKind::ArchiveDownload {
            site_id: "KDMX".to_string(),
            file_name: format!("KDMX_{scan_start}"),
            scan_start,
            scan_end: scan_start + 300,
        }
    }

    /// A single failure must NOT pause the whole queue (alignment §5 failure
    /// model: failures are local and recoverable). With another item still
    /// queued, the queue stays Running and dispatch keeps flowing.
    #[wasm_bindgen_test]
    fn failure_does_not_globally_pause_queue() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        let b = acq.create_operation(download_kind(2000));
        acq.mark_active(a);
        acq.mark_active(b);

        // a fails — must not error-pause, must not block b.
        acq.mark_failed(a, "boom".to_string());

        assert!(
            !acq.is_paused(),
            "a single failure must not pause the queue"
        );
        // b is still active and dispatchable.
        assert_eq!(acq.active_count(), 1);
        assert!(matches!(
            acq.find(a).unwrap().status,
            OperationStatus::Failed { .. }
        ));
    }

    /// When the failing op was the only work, the queue settles to Empty (never
    /// a global error-pause) so the next reactive prefetch can run unobstructed.
    #[wasm_bindgen_test]
    fn lone_failure_settles_to_empty_not_paused() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        acq.mark_active(a);
        acq.mark_failed(a, "boom".to_string());
        assert!(!acq.is_paused());
        assert_eq!(acq.queue_state, QueueState::Empty);
    }

    /// `failed_operation_for_scan_start` locates the right op so the strip's
    /// failed-cell tick can target `RetryFailed`, and `retry_failed` flips it
    /// back to Queued (the operation side of the two-machine retry).
    #[wasm_bindgen_test]
    fn retry_resets_failed_operation_to_queued() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(5000));
        acq.mark_active(a);
        acq.mark_failed(a, "boom".to_string());

        let found = acq.failed_operation_for_scan_start(5000, 60);
        assert_eq!(found, Some(a));

        acq.retry_failed(a);
        assert_eq!(acq.find(a).unwrap().status, OperationStatus::Queued);
        assert!(!acq.is_paused());
    }

    // `describe_operation` moved to `core::domain::ops` with its tests.

    // ── latency_summary: percentile / average math ──────────────────────────

    fn latency(fetch_ms: f64, e2e_ms: Option<f64>) -> ChunkLatencyMetrics {
        ChunkLatencyMetrics {
            chunk_index: 0,
            fetch_latency_ms: fetch_ms,
            end_to_end_latency_ms: e2e_ms,
        }
    }

    /// Empty latency buffer yields no summary at all (not a zeroed/NaN one).
    #[wasm_bindgen_test]
    fn latency_summary_empty_is_none() {
        let acq = AcquisitionState::default();
        assert!(acq.latency_summary().is_none());
    }

    /// A single sample: avg == p50 == p95 == that value (n=1 → both percentile
    /// indices resolve to 0).
    #[wasm_bindgen_test]
    fn latency_summary_single_sample() {
        let mut acq = AcquisitionState::default();
        acq.chunk_latencies.push(latency(42.0, Some(7.0)));
        let s = acq.latency_summary().unwrap();
        assert_eq!(s.avg_fetch_ms, 42.0);
        assert_eq!(s.p50_fetch_ms, 42.0); // sorted[1/2] = sorted[0]
        assert_eq!(s.p95_fetch_ms, 42.0); // sorted[(1*0.95)=0]
        assert_eq!(s.avg_e2e_ms, Some(7.0));
    }

    /// n=2: the implementation indexes p50 = sorted[2/2] = sorted[1] = the MAX,
    /// and p95 = sorted[(2*0.95)=1.9→1] = the max too. Pin that exact (slightly
    /// asymmetric) indexing so a refactor can't silently shift it.
    #[wasm_bindgen_test]
    fn latency_summary_two_samples_index_the_upper() {
        let mut acq = AcquisitionState::default();
        acq.chunk_latencies.push(latency(10.0, None));
        acq.chunk_latencies.push(latency(30.0, None));
        let s = acq.latency_summary().unwrap();
        assert_eq!(s.avg_fetch_ms, 20.0);
        assert_eq!(s.p50_fetch_ms, 30.0); // sorted = [10,30]; index 1
        assert_eq!(s.p95_fetch_ms, 30.0);
    }

    /// A known multi-sample set with hand-computed indices. 20 samples
    /// 5,10,15,…,100 sorted. p50 = sorted[20/2] = sorted[10] = 55.
    /// p95 = sorted[(20*0.95)=19.0→19] = sorted[19] = 100. avg = mean = 52.5.
    #[wasm_bindgen_test]
    fn latency_summary_multi_sample_hand_computed() {
        let mut acq = AcquisitionState::default();
        // Insert out of order to exercise the internal sort.
        for k in [4u32, 0, 2, 1, 3] {
            for j in 0..4 {
                let v = (k * 4 + j + 1) as f64 * 5.0;
                acq.chunk_latencies.push(latency(v, None));
            }
        }
        assert_eq!(acq.chunk_latencies.len(), 20);
        let s = acq.latency_summary().unwrap();
        assert_eq!(s.avg_fetch_ms, 52.5);
        assert_eq!(s.p50_fetch_ms, 55.0);
        assert_eq!(s.p95_fetch_ms, 100.0);
    }

    /// When every chunk lacks an end-to-end latency, avg_e2e is None — never a
    /// NaN from dividing a zero-length sum — while fetch stats still populate.
    #[wasm_bindgen_test]
    fn latency_summary_all_none_e2e_is_none_not_nan() {
        let mut acq = AcquisitionState::default();
        acq.chunk_latencies.push(latency(10.0, None));
        acq.chunk_latencies.push(latency(20.0, None));
        let s = acq.latency_summary().unwrap();
        assert_eq!(s.avg_e2e_ms, None);
        assert!(!s.avg_e2e_ms.unwrap_or(0.0).is_nan());
        // Fetch stats still computed.
        assert_eq!(s.avg_fetch_ms, 15.0);
    }

    // ── lifecycle transitions ───────────────────────────────────────────────

    /// `cancel_all` cancels Active+Queued and forces Empty, but leaves
    /// already-terminal (Completed/Failed) operations untouched.
    #[wasm_bindgen_test]
    fn cancel_all_cancels_active_and_queued_not_completed() {
        let mut acq = AcquisitionState::default();
        let active = acq.create_operation(download_kind(1000));
        let queued = acq.create_operation(download_kind(2000));
        let completed = acq.create_operation(download_kind(3000));
        let failed = acq.create_operation(download_kind(4000));
        acq.mark_active(active);
        acq.mark_active(completed);
        acq.mark_completed(completed, 10);
        acq.mark_active(failed);
        acq.mark_failed(failed, "boom".to_string());
        // `queued` is left Queued.

        acq.cancel_all();

        assert_eq!(acq.find(active).unwrap().status, OperationStatus::Cancelled);
        assert_eq!(acq.find(queued).unwrap().status, OperationStatus::Cancelled);
        assert!(matches!(
            acq.find(completed).unwrap().status,
            OperationStatus::Completed { .. }
        ));
        assert!(matches!(
            acq.find(failed).unwrap().status,
            OperationStatus::Failed { .. }
        ));
        assert_eq!(acq.queue_state, QueueState::Empty);
    }

    /// `cancel_all_queued` cancels only Queued items and routes through
    /// `update_queue_state`, which must NOT clobber a user Paused queue.
    #[wasm_bindgen_test]
    fn cancel_all_queued_leaves_paused_queue_paused() {
        let mut acq = AcquisitionState::default();
        let active = acq.create_operation(download_kind(1000));
        let queued = acq.create_operation(download_kind(2000));
        acq.mark_active(active);
        acq.pause();
        assert!(acq.is_paused());

        acq.cancel_all_queued();

        // Only the queued item was cancelled; the active one survives.
        assert_eq!(acq.find(queued).unwrap().status, OperationStatus::Cancelled);
        assert_eq!(acq.find(active).unwrap().status, OperationStatus::Active);
        // Pause survives even though there's still an active op.
        assert!(acq.is_paused());
    }

    /// `reorder_operation` clamps the delta at both ends of the deque, so moving
    /// the first item up or the last item down is a no-op (no panic, no wrap).
    #[wasm_bindgen_test]
    fn reorder_operation_clamps_at_ends() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        let b = acq.create_operation(download_kind(2000));
        let c = acq.create_operation(download_kind(3000));
        let order = |acq: &AcquisitionState| -> Vec<OperationId> {
            acq.operations.iter().map(|o| o.id).collect()
        };
        assert_eq!(order(&acq), vec![a, b, c]);

        // Move the first item further up — clamped, no change.
        acq.reorder_operation(a, -5);
        assert_eq!(order(&acq), vec![a, b, c]);
        // Move the last item further down — clamped, no change.
        acq.reorder_operation(c, 5);
        assert_eq!(order(&acq), vec![a, b, c]);
        // A real in-bounds move still works (b down one → a, c, b).
        acq.reorder_operation(b, 1);
        assert_eq!(order(&acq), vec![a, c, b]);
    }

    /// `correlate_network_request` matches the NEWEST Active/Completed op and
    /// ignores Queued/Cancelled ones.
    #[wasm_bindgen_test]
    fn correlate_returns_newest_active_match_ignoring_queued() {
        let mut acq = AcquisitionState::default();
        let mk = || OperationKind::ArchiveDownload {
            site_id: "KDMX".to_string(),
            file_name: "KDMX20240501_120000_V06".to_string(),
            scan_start: 1000,
            scan_end: 1300,
        };
        // Three downloads of the same file. `queued` must be ignored (not
        // Active/Completed); `old` and `new` are BOTH Active, so the correlator
        // must pick the newest (its scan walks operations newest-first).
        let queued = acq.create_operation(mk());
        let old = acq.create_operation(mk());
        let new = acq.create_operation(mk());
        acq.mark_active(old);
        acq.mark_active(new);
        let url = "https://s3/.../KDMX20240501_120000_V06";
        assert_eq!(
            acq.correlate_network_request(url),
            Some(new),
            "with two Active matches the newest wins the tie-break"
        );
        let _ = queued;

        // Cancel the newest → the older Active match now wins (still ignores Queued).
        acq.cancel_operation(new);
        assert_eq!(acq.correlate_network_request(url), Some(old));

        // Cancel the last Active → nothing left to correlate (Queued is ignored).
        acq.cancel_operation(old);
        assert_eq!(acq.correlate_network_request(url), None);
    }

    /// `url_matches_operation` table: ArchiveDownload by file-name substring,
    /// ArchiveListing by date-prefix AND site, RealtimeChunk by the chunks
    /// bucket AND site — plus a non-matching URL for each.
    #[wasm_bindgen_test]
    fn url_matches_operation_rules() {
        let acq = AcquisitionState::default();

        let dl = OperationKind::ArchiveDownload {
            site_id: "KDMX".to_string(),
            file_name: "KDMX20240501_120000_V06".to_string(),
            scan_start: 0,
            scan_end: 0,
        };
        assert!(acq.url_matches_operation("https://s3/KDMX20240501_120000_V06", &dl));
        assert!(!acq.url_matches_operation("https://s3/KDMX20240501_999999_V06", &dl));

        let listing = OperationKind::ArchiveListing {
            site_id: "KDMX".to_string(),
            date: chrono::NaiveDate::from_ymd_opt(2024, 5, 1).unwrap(),
        };
        // Needs BOTH the date prefix and the site.
        assert!(acq.url_matches_operation("https://s3/?prefix=2024/05/01/KDMX/", &listing));
        // Right date, wrong site → no match (AND rule).
        assert!(!acq.url_matches_operation("https://s3/?prefix=2024/05/01/KABR/", &listing));
        // Right site, wrong date → no match.
        assert!(!acq.url_matches_operation("https://s3/?prefix=2024/05/02/KDMX/", &listing));

        let chunk = OperationKind::RealtimeChunk {
            site_id: "KDMX".to_string(),
            chunk_index: 3,
            is_start: false,
            is_end: false,
            scan_timestamp: 1000,
        };
        // Needs BOTH the chunks bucket and the site.
        assert!(acq.url_matches_operation("https://nexrad-level2-chunks/KDMX/...", &chunk));
        // Chunks bucket but a different site → no match.
        assert!(!acq.url_matches_operation("https://nexrad-level2-chunks/KABR/...", &chunk));
        // Right site but the archive (not chunks) bucket → no match.
        assert!(!acq.url_matches_operation("https://nexrad-level2/KDMX/...", &chunk));
    }

    /// `mark_completed` with no recorded start time falls back to a 0.0 duration
    /// rather than producing a negative/garbage value.
    #[wasm_bindgen_test]
    fn mark_completed_without_start_uses_zero_duration() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        // Skip mark_active → started_at_ms stays None.
        acq.mark_completed(a, 123);
        match acq.find(a).unwrap().status {
            OperationStatus::Completed { duration_ms, bytes } => {
                assert_eq!(duration_ms, 0.0);
                assert_eq!(bytes, 123);
            }
            ref other => panic!("expected Completed, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn download_kind(scan_start: i64) -> OperationKind {
        OperationKind::ArchiveDownload {
            site_id: "KDMX".to_string(),
            file_name: format!("KDMX_{scan_start}"),
            scan_start,
            scan_end: scan_start + 300,
        }
    }

    fn chunk_kind(site: &str, chunk_index: u32, scan_timestamp: i64) -> OperationKind {
        OperationKind::RealtimeChunk {
            site_id: site.to_string(),
            chunk_index,
            is_start: false,
            is_end: false,
            scan_timestamp,
        }
    }

    fn listing_kind(site: &str, y: i32, m: u32, d: u32) -> OperationKind {
        OperationKind::ArchiveListing {
            site_id: site.to_string(),
            date: chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap(),
        }
    }

    // ── create_operation: ids, default fields, queue activation ──────────────

    /// IDs are monotonically increasing starting from 1, and creating the first
    /// operation flips an Empty queue to Running. New ops start Queued.
    #[wasm_bindgen_test]
    fn create_operation_assigns_increasing_ids_and_starts_queue() {
        let mut acq = AcquisitionState::default();
        assert_eq!(acq.queue_state, QueueState::Empty);
        let a = acq.create_operation(download_kind(1000));
        let b = acq.create_operation(download_kind(2000));
        let c = acq.create_operation(download_kind(3000));
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
        // First create activates the queue.
        assert_eq!(acq.queue_state, QueueState::Running);
        // Default field values on a fresh op.
        let op = acq.find(a).unwrap();
        assert_eq!(op.status, OperationStatus::Queued);
        assert!(op.started_at_ms.is_none());
        assert!(op.completed_at_ms.is_none());
    }

    /// The ring buffer evicts the oldest operation once it exceeds MAX_RETAINED
    /// (200): after 201 inserts the length caps at 200 and the front is the
    /// SECOND id created (id 1 was popped).
    #[wasm_bindgen_test]
    fn create_operation_ring_buffer_evicts_oldest() {
        let mut acq = AcquisitionState::default();
        let mut first_kept = None;
        for i in 0..201u32 {
            let id = acq.create_operation(download_kind(i as i64));
            if i == 1 {
                first_kept = Some(id);
            }
        }
        // Capacity is enforced at 200.
        assert_eq!(acq.operations.len(), 200);
        // id 1 (the very first) was evicted; id 2 is now the oldest retained.
        assert!(acq.find(1).is_none());
        assert_eq!(acq.operations.front().unwrap().id, first_kept.unwrap());
        assert_eq!(acq.operations.front().unwrap().id, 2);
        // The newest id is 201.
        assert_eq!(acq.operations.back().unwrap().id, 201);
    }

    // ── no-op guards on missing ids ──────────────────────────────────────────

    /// Mutating helpers against an unknown id are silent no-ops (find returns
    /// None internally) and never panic or touch other operations.
    #[wasm_bindgen_test]
    fn mutators_on_unknown_id_are_noops() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        // 999 does not exist.
        acq.mark_active(999);
        acq.mark_completed(999, 5);
        acq.mark_failed(999, "x".to_string());
        acq.cancel_operation(999);
        acq.retry_failed(999);
        // The real op is untouched: still Queued.
        let op = acq.find(a).unwrap();
        assert_eq!(op.status, OperationStatus::Queued);
        // find on a missing id is None.
        assert!(acq.find(999).is_none());
    }

    // ── mark_active / mark_completed field effects ───────────────────────────

    /// `mark_active` sets status Active and stamps the start time.
    #[wasm_bindgen_test]
    fn mark_active_sets_status_and_start_time() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        acq.mark_active(a);
        let op = acq.find(a).unwrap();
        assert_eq!(op.status, OperationStatus::Active);
        assert!(op.started_at_ms.is_some());
    }

    /// `mark_completed` records the byte count and (being the only op) settles
    /// the queue to Empty.
    #[wasm_bindgen_test]
    fn mark_completed_records_bytes_and_settles_empty() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        acq.mark_active(a);
        acq.mark_completed(a, 4096);
        let op = acq.find(a).unwrap();
        assert!(op.completed_at_ms.is_some());
        match op.status {
            OperationStatus::Completed { bytes, .. } => assert_eq!(bytes, 4096),
            ref other => panic!("expected Completed, got {other:?}"),
        }
        assert_eq!(acq.queue_state, QueueState::Empty);
    }

    // ── pause / resume guards ────────────────────────────────────────────────

    /// `pause` only fires from Running; on an Empty queue it is a no-op, and
    /// `resume` only un-pauses a Paused queue.
    #[wasm_bindgen_test]
    fn pause_resume_only_transition_from_their_source_state() {
        let mut acq = AcquisitionState::default();
        // Empty queue: pause is a no-op (cannot pause nothing).
        acq.pause();
        assert_eq!(acq.queue_state, QueueState::Empty);
        assert!(!acq.is_paused());
        // resume on Empty is also a no-op.
        acq.resume();
        assert_eq!(acq.queue_state, QueueState::Empty);

        // With work present → Running, pause works, resume restores Running.
        let a = acq.create_operation(download_kind(1000));
        acq.mark_active(a);
        assert_eq!(acq.queue_state, QueueState::Running);
        acq.pause();
        assert!(acq.is_paused());
        // pause again is a no-op (not Running anymore).
        acq.pause();
        assert!(acq.is_paused());
        acq.resume();
        assert_eq!(acq.queue_state, QueueState::Running);
        assert!(!acq.is_paused());
    }

    // ── cancel_operation single ──────────────────────────────────────────────

    /// Cancelling the sole active op marks it Cancelled and settles the queue to
    /// Empty via update_queue_state.
    #[wasm_bindgen_test]
    fn cancel_operation_single_settles_empty() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        acq.mark_active(a);
        acq.cancel_operation(a);
        let op = acq.find(a).unwrap();
        assert_eq!(op.status, OperationStatus::Cancelled);
        assert!(op.completed_at_ms.is_some());
        assert_eq!(acq.queue_state, QueueState::Empty);
    }

    // ── skip_failed ──────────────────────────────────────────────────────────

    /// `skip_failed` cancels the failed op and forces the queue back to Running.
    #[wasm_bindgen_test]
    fn skip_failed_cancels_and_resumes() {
        let mut acq = AcquisitionState::default();
        let a = acq.create_operation(download_kind(1000));
        acq.mark_active(a);
        acq.mark_failed(a, "boom".to_string());
        acq.skip_failed(a);
        assert_eq!(acq.find(a).unwrap().status, OperationStatus::Cancelled);
        assert_eq!(acq.queue_state, QueueState::Running);
    }

    // ── retry_failed reorders to front of pending ────────────────────────────

    /// `retry_failed` resets the op to Queued and inserts it before the first
    /// pending (Queued/Active) op. With [completed, queued, failed], retrying
    /// the failed one yields [completed, retried, queued].
    #[wasm_bindgen_test]
    fn retry_failed_moves_before_first_pending() {
        let mut acq = AcquisitionState::default();
        let done = acq.create_operation(download_kind(1000));
        let queued = acq.create_operation(download_kind(2000));
        let failed = acq.create_operation(download_kind(3000));
        acq.mark_active(done);
        acq.mark_completed(done, 1);
        acq.mark_active(failed);
        acq.mark_failed(failed, "boom".to_string());
        // queued is left Queued.

        let order = |acq: &AcquisitionState| -> Vec<OperationId> {
            acq.operations.iter().map(|o| o.id).collect()
        };
        assert_eq!(order(&acq), vec![done, queued, failed]);

        acq.retry_failed(failed);
        // Inserted before the first pending op (queued), after the completed one.
        assert_eq!(order(&acq), vec![done, failed, queued]);
        assert_eq!(acq.find(failed).unwrap().status, OperationStatus::Queued);
        assert_eq!(acq.queue_state, QueueState::Running);
    }

    // ── counters on empty / mixed ────────────────────────────────────────────

    /// Counters and has_active_operations on a fresh state are all zero/false.
    #[wasm_bindgen_test]
    fn counters_on_empty_state() {
        let acq = AcquisitionState::default();
        assert_eq!(acq.queued_count(), 0);
        assert_eq!(acq.active_count(), 0);
        assert!(!acq.has_active_operations());
        assert!(acq.next_queued_id().is_none());
    }

    /// Counters reflect a mixed set; has_active_operations is true while any
    /// Queued OR Active op exists and false once all are terminal.
    #[wasm_bindgen_test]
    fn counters_on_mixed_set() {
        let mut acq = AcquisitionState::default();
        let active = acq.create_operation(download_kind(1000));
        let q1 = acq.create_operation(download_kind(2000));
        let _q2 = acq.create_operation(download_kind(3000));
        acq.mark_active(active);
        assert_eq!(acq.active_count(), 1);
        assert_eq!(acq.queued_count(), 2);
        assert!(acq.has_active_operations());
        // First queued in insertion order is q1.
        assert_eq!(acq.next_queued_id(), Some(q1));

        // Drain everything to terminal states.
        acq.cancel_operation(active);
        acq.cancel_all_queued();
        assert_eq!(acq.active_count(), 0);
        assert_eq!(acq.queued_count(), 0);
        assert!(!acq.has_active_operations());
        assert!(acq.next_queued_id().is_none());
    }

    // ── failed_scan_starts ───────────────────────────────────────────────────

    /// `failed_scan_starts` collects scan_start ONLY from Failed ArchiveDownload
    /// ops — ignoring failed-but-non-download kinds and non-failed downloads —
    /// in oldest→newest order.
    #[wasm_bindgen_test]
    fn failed_scan_starts_filters_to_failed_downloads() {
        let mut acq = AcquisitionState::default();
        let d1 = acq.create_operation(download_kind(1000));
        let d2 = acq.create_operation(download_kind(2000));
        let ok = acq.create_operation(download_kind(3000));
        let chunk = acq.create_operation(chunk_kind("KDMX", 0, 9999));
        acq.mark_active(d1);
        acq.mark_failed(d1, "boom".to_string());
        acq.mark_active(d2);
        acq.mark_failed(d2, "boom".to_string());
        // ok completes fine; chunk fails but is not an ArchiveDownload.
        acq.mark_active(ok);
        acq.mark_completed(ok, 1);
        acq.mark_active(chunk);
        acq.mark_failed(chunk, "boom".to_string());

        assert_eq!(acq.failed_scan_starts(), vec![1000, 2000]);
    }

    // ── failed_operation_for_scan_start: tolerance + newest + non-match ───────

    /// The tolerance is inclusive at the boundary and excludes anything beyond;
    /// when two failed downloads are within tolerance, the NEWEST (latest in the
    /// deque) wins, and a non-failed download never matches.
    #[wasm_bindgen_test]
    fn failed_operation_for_scan_start_tolerance_and_newest() {
        let mut acq = AcquisitionState::default();
        let old = acq.create_operation(download_kind(1000));
        let new = acq.create_operation(download_kind(1005));
        acq.mark_active(old);
        acq.mark_failed(old, "boom".to_string());
        acq.mark_active(new);
        acq.mark_failed(new, "boom".to_string());

        // Both within tolerance of 1000 → newest (1005) wins.
        assert_eq!(acq.failed_operation_for_scan_start(1000, 10), Some(new));
        // Exactly at the tolerance boundary (|1000-995|=5 <= 5) still matches old only.
        // Query 990: |1000-990|=10 > 5 for old, |1005-990|=15 > 5 for new → None.
        assert_eq!(acq.failed_operation_for_scan_start(990, 5), None);
        // Query 998 with tol 5: |1000-998|=2 ok, |1005-998|=7 no → old only.
        assert_eq!(acq.failed_operation_for_scan_start(998, 5), Some(old));

        // A non-failed (completed) download in range is never returned.
        let mut acq2 = AcquisitionState::default();
        let c = acq2.create_operation(download_kind(2000));
        acq2.mark_active(c);
        acq2.mark_completed(c, 1);
        assert_eq!(acq2.failed_operation_for_scan_start(2000, 60), None);
    }

    // ── network_group_key / scan_group_key / scan_group_description ───────────

    /// Realtime chunks group by (site, scan_timestamp); all other kinds group by
    /// their individual operation id.
    #[wasm_bindgen_test]
    fn network_group_key_realtime_vs_other() {
        let mut acq = AcquisitionState::default();
        let dl_id = acq.create_operation(download_kind(1000));
        let chunk_id = acq.create_operation(chunk_kind("KDMX", 4, 1700));

        let dl = acq.find(dl_id).unwrap();
        assert_eq!(
            AcquisitionState::network_group_key(dl),
            NetworkGroupKey::Operation(dl_id)
        );

        let chunk = acq.find(chunk_id).unwrap();
        assert_eq!(
            AcquisitionState::network_group_key(chunk),
            NetworkGroupKey::RealtimeScan {
                site_id: "KDMX".to_string(),
                scan_timestamp: 1700,
            }
        );
    }

    /// `scan_group_key` returns Some only for realtime chunks; None for download
    /// and listing kinds.
    #[wasm_bindgen_test]
    fn scan_group_key_some_only_for_realtime() {
        assert_eq!(
            AcquisitionState::scan_group_key(&chunk_kind("KDMX", 1, 4242)),
            Some(("KDMX".to_string(), 4242))
        );
        assert_eq!(AcquisitionState::scan_group_key(&download_kind(1000)), None);
        assert_eq!(
            AcquisitionState::scan_group_key(&listing_kind("KDMX", 2024, 5, 1)),
            None
        );
    }

    /// `scan_group_description` formats a valid timestamp as "SITE live scan
    /// HH:MM:SSZ"; an unrepresentable timestamp falls back to the raw integer.
    #[wasm_bindgen_test]
    fn scan_group_description_valid_and_invalid() {
        // ts 0 → 00:00:00Z UTC.
        assert_eq!(
            AcquisitionState::scan_group_description("KDMX", 0),
            "KDMX live scan 00:00:00Z"
        );
        // Unrepresentable → raw integer fallback.
        assert_eq!(
            AcquisitionState::scan_group_description("KABR", i64::MAX),
            format!("KABR live scan {}", i64::MAX)
        );
    }

    // ── record_chunk_latency / clear_latencies ───────────────────────────────

    /// Recording a chunk with no first-radial time leaves end-to-end latency None
    /// (no NaN), while a present first-radial time yields Some(_). clear() empties.
    #[wasm_bindgen_test]
    fn record_chunk_latency_e2e_presence_and_clear() {
        let mut acq = AcquisitionState::default();
        // No first-radial → end_to_end_latency_ms is None.
        acq.record_chunk_latency(0, 12.0, None);
        assert_eq!(acq.chunk_latencies.len(), 1);
        let m0 = &acq.chunk_latencies[0];
        assert_eq!(m0.chunk_index, 0);
        assert!((m0.fetch_latency_ms - 12.0).abs() < 1e-9);
        assert!(m0.end_to_end_latency_ms.is_none());

        // With a first-radial time, e2e is computed as Some(_) (value depends on
        // wall clock, so only assert presence and finiteness).
        acq.record_chunk_latency(1, 8.0, Some(1.0));
        let m1 = &acq.chunk_latencies[1];
        assert_eq!(m1.chunk_index, 1);
        assert!(m1.end_to_end_latency_ms.is_some());
        assert!(m1.end_to_end_latency_ms.unwrap().is_finite());

        acq.clear_latencies();
        assert!(acq.chunk_latencies.is_empty());
        assert!(acq.latency_summary().is_none());
    }

    // ── correlate_network_request: realtime + miss path ──────────────────────

    /// A realtime chunk op matches a chunks-bucket URL containing its site, and a
    /// completely unrelated URL correlates to nothing (None).
    #[wasm_bindgen_test]
    fn correlate_realtime_and_miss() {
        let mut acq = AcquisitionState::default();
        let id = acq.create_operation(chunk_kind("KDMX", 0, 1700));
        acq.mark_active(id);
        assert_eq!(
            acq.correlate_network_request("https://nexrad-level2-chunks/KDMX/0001"),
            Some(id)
        );
        // Unrelated host → no correlation.
        assert_eq!(
            acq.correlate_network_request("https://example.com/unrelated"),
            None
        );
        // Empty state correlates to nothing.
        let empty = AcquisitionState::default();
        assert_eq!(empty.correlate_network_request("anything"), None);
    }

    // ── enum / type defaults ─────────────────────────────────────────────────

    /// Default impls land on the documented variants.
    #[wasm_bindgen_test]
    fn type_defaults() {
        assert_eq!(QueueState::default(), QueueState::Empty);
        assert_eq!(DrawerTab::default(), DrawerTab::Queue);
        let acq = AcquisitionState::default();
        assert!(!acq.drawer_expanded);
        assert!((acq.drawer_height - 250.0).abs() < 1e-6);
        assert_eq!(acq.active_tab, DrawerTab::Queue);
        assert!(acq.operations.is_empty());
        assert!(acq.expanded_network_groups.is_empty());
    }

    // ── latency_summary p95 boundary for a larger n ──────────────────────────

    /// For n=10 the implementation indexes p50 = sorted[10/2]=sorted[5] and
    /// p95 = sorted[(10*0.95)=9.5→9]=sorted[9] (the max). Samples 1..=10 ms.
    #[wasm_bindgen_test]
    fn latency_summary_n10_indices() {
        let mut acq = AcquisitionState::default();
        // Insert reversed to exercise the internal sort.
        for v in (1..=10).rev() {
            acq.chunk_latencies.push(ChunkLatencyMetrics {
                chunk_index: 0,
                fetch_latency_ms: v as f64,
                end_to_end_latency_ms: None,
            });
        }
        let s = acq.latency_summary().unwrap();
        // mean of 1..=10 = 5.5
        assert!((s.avg_fetch_ms - 5.5).abs() < 1e-9);
        // sorted = [1..10]; index 5 → 6
        assert!((s.p50_fetch_ms - 6.0).abs() < 1e-9);
        // index 9 → 10 (the max)
        assert!((s.p95_fetch_ms - 10.0).abs() < 1e-9);
    }
}
