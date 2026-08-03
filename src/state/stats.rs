//! Session and performance statistics for the status bar.

use crate::core::{DownloadPhase, ThroughputWindow, WorkerLoad};
use crate::nexrad::NetworkStats;

/// In-flight flags for the two pipeline stages the main thread owns directly.
///
/// The old three-lamp DL/PROC/GPU indicator (and its "recently completed"
/// linger timestamps) is gone: the activity surface shows real per-stage
/// counts instead, derived in [`crate::core::activity`]. What survives here is
/// the pair of flags the shell genuinely needs — `rendering` feeds the
/// view-model's GPU-in-flight input, and `processing` gates the live-mode
/// render request.
#[derive(Default, Clone)]
pub(crate) struct PipelineStatus {
    /// Whether processing is in progress: ingest + decode in worker.
    ///
    /// Hand-maintained, and therefore *not* the activity surface's source of
    /// truth — that comes from the decode workers' own pending maps via
    /// [`SessionStats::worker_load`], which cannot drift.
    pub processing: bool,
    /// Whether GPU rendering/upload is in progress.
    pub rendering: bool,

    /// Timestamps (ms since epoch) of the last completion, kept as a liveness
    /// signal for debugging stuck pipelines.
    pub last_processing_done_ms: f64,
    pub last_render_done_ms: f64,

    /// Whether any pipeline activity has occurred this session.
    pub ever_active: bool,
}

impl PipelineStatus {
    /// Mark processing phase as completed (ingest + decode finished).
    pub(crate) fn mark_processing_done(&mut self) {
        self.processing = false;
        self.last_processing_done_ms = js_sys::Date::now();
        self.ever_active = true;
    }

    /// Mark rendering phase as completed (GPU upload finished).
    pub(crate) fn mark_render_done(&mut self) {
        self.rendering = false;
        self.last_render_done_ms = js_sys::Date::now();
        self.ever_active = true;
    }
}

/// Detailed sub-phase timings from the most recent ingest operation.
#[derive(Default, Clone)]
pub(crate) struct IngestTimingDetail {
    pub split_ms: f64,
    pub decompress_ms: f64,
    pub decode_ms: f64,
    pub extract_ms: f64,
    pub store_ms: f64,
    pub index_ms: f64,
}

/// Detailed sub-phase timings from the most recent render/decode operation.
#[derive(Default, Clone)]
pub(crate) struct RenderTimingDetail {
    pub fetch_ms: f64,
    pub deser_ms: f64,
    pub marshal_ms: f64,
    pub gpu_upload_ms: f64,
}

/// Statistics displayed in the status bar.
#[derive(Default, Clone)]
pub(crate) struct SessionStats {
    /// Total persisted cache size in bytes (IndexedDB).
    pub cache_size_bytes: u64,

    /// Total number of requests made this session.
    pub session_request_count: u32,

    /// Total bytes transferred this session.
    pub session_transferred_bytes: u64,

    /// Number of currently active (in-flight) requests.
    pub active_request_count: u32,

    /// Running average of fetch latency in milliseconds.
    pub median_chunk_latency_ms: Option<f64>,

    /// Running average of full processing time (ingest total) in milliseconds.
    pub median_processing_time_ms: Option<f64>,

    /// Running average of radar render time in milliseconds.
    pub avg_render_time_ms: Option<f64>,

    /// Running average of frames per second.
    pub avg_fps: Option<f64>,

    /// Current pipeline phase status.
    pub pipeline: PipelineStatus,

    /// Most recent ingest timing breakdown (for detail modal).
    pub last_ingest_detail: Option<IngestTimingDetail>,

    /// Most recent render timing breakdown (for detail modal).
    pub last_render_detail: Option<RenderTimingDetail>,

    /// Rolling transfer-rate window behind the activity surface's throughput
    /// readout. Fed each frame from service-worker request metrics when they
    /// are available, and from a [`NetworkStats`] counter delta otherwise.
    pub throughput: ThroughputWindow,

    /// Last value read from the cumulative byte counter, so the fallback
    /// throughput source can diff against it. Only meaningful alongside
    /// [`Self::throughput`].
    pub last_total_bytes: u64,

    /// Snapshot of decode-worker queue depth, refreshed once per frame from
    /// the render coordinator. This is the authoritative "processing" figure
    /// for the activity surface — derived from the correlation maps rather
    /// than hand-maintained, so it cannot drift out of sync the way a
    /// manually set flag can.
    pub worker_load: WorkerLoad,

    /// When the main thread last consumed a worker outcome (ms since epoch).
    ///
    /// A worker that dies mid-job leaks its pending-map entry forever, which
    /// would pin "processing" on permanently. The activity view-model treats
    /// a load that hasn't moved in a long time as stale and reports zero —
    /// a missing indicator is better than a stuck one.
    pub last_worker_outcome_ms: f64,
}

impl SessionStats {
    /// Create stats with initial (zero) values.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Update stats from live network statistics.
    pub(crate) fn update_from_network_stats(&mut self, network_stats: &NetworkStats) {
        self.session_request_count = network_stats.total_count();
        self.session_transferred_bytes = network_stats.bytes_transferred();
        self.active_request_count = network_stats.active_count();
    }

    /// Record a frame time sample from `stable_dt`, updating the FPS average.
    /// Uses exponential moving average with alpha=0.05 for a smooth readout.
    pub(crate) fn record_frame_time(&mut self, dt: f32) {
        if dt > 0.0 {
            let fps = 1.0 / dt as f64;
            const ALPHA: f64 = 0.05;
            self.avg_fps = Some(match self.avg_fps {
                Some(avg) => avg * (1.0 - ALPHA) + fps * ALPHA,
                None => fps,
            });
        }
    }

    /// Record a render time sample, updating the running average.
    /// Uses exponential moving average with alpha=0.2 for smooth updates.
    pub(crate) fn record_render_time(&mut self, time_ms: f64) {
        const ALPHA: f64 = 0.2;
        self.avg_render_time_ms = Some(match self.avg_render_time_ms {
            Some(avg) => avg * (1.0 - ALPHA) + time_ms * ALPHA,
            None => time_ms,
        });
    }

    /// Format cache size for display (e.g., "150.2 MB").
    pub(crate) fn format_cache_size(&self) -> String {
        format_bytes(self.cache_size_bytes)
    }

    /// Format transferred bytes for display (e.g., "12.0 MB").
    pub(crate) fn format_transferred(&self) -> String {
        format_bytes(self.session_transferred_bytes)
    }

    /// Record a fetch latency sample, updating the running average.
    pub(crate) fn record_fetch_latency(&mut self, ms: f64) {
        const ALPHA: f64 = 0.2;
        self.median_chunk_latency_ms = Some(match self.median_chunk_latency_ms {
            Some(avg) => avg * (1.0 - ALPHA) + ms * ALPHA,
            None => ms,
        });
    }

    /// Record a processing time sample (full ingest total), updating the running average.
    pub(crate) fn record_processing_time(&mut self, ms: f64) {
        const ALPHA: f64 = 0.2;
        self.median_processing_time_ms = Some(match self.median_processing_time_ms {
            Some(avg) => avg * (1.0 - ALPHA) + ms * ALPHA,
            None => ms,
        });
    }
}

/// Tracks download progress for timeline ghost markers and pipeline display.
///
/// Scan boundaries are `(start_secs, end_secs)` pairs derived from the archive
/// listing's adjacent file timestamps, giving accurate ghost widths on the timeline.
#[derive(Default, Clone)]
pub(crate) struct DownloadProgress {
    /// Scan boundaries (start, end) of files queued but not yet loaded.
    /// The timeline renders ghost markers spanning these intervals.
    pub pending_scans: Vec<(i64, i64)>,
    /// Boundaries of files currently being downloaded.
    /// Their ghost markers pulse to distinguish them from queued items.
    /// Multiple entries when parallel downloads are in flight.
    pub active_scans: Vec<(i64, i64)>,
    /// Phase of the currently active file.
    pub phase: DownloadPhase,
    /// Number of files completed so far.
    pub batch_completed: u32,
    /// Scan boundaries of files downloaded but still being ingested/decoded/rendered.
    /// Ghosts for these stay visible until processing completes.
    pub in_flight_scans: Vec<(i64, i64)>,
}

impl DownloadProgress {
    /// Whether any download operation is active.
    pub(crate) fn is_active(&self) -> bool {
        self.phase != DownloadPhase::Idle && self.phase != DownloadPhase::Done
            || !self.pending_scans.is_empty()
            || !self.in_flight_scans.is_empty()
    }

    /// Reset all progress state.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

use super::settings::format_bytes;

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── SessionStats EMA recorders ──

    #[wasm_bindgen_test]
    fn record_frame_time_seeds_then_blends() {
        // Power-of-two dts keep the f32→f64 cast and reciprocal exact, so the
        // expected EMA can be asserted by exact equality.
        let mut s = SessionStats::new();
        // First sample seeds the average with the raw fps (1/dt). dt=0.25 → 4 fps.
        s.record_frame_time(0.25);
        assert_eq!(s.avg_fps, Some(4.0));
        // Second sample blends with ALPHA = 0.05: avg*0.95 + fps*0.05.
        // dt=0.5 → 2 fps.
        s.record_frame_time(0.5);
        let expected = 4.0 * 0.95 + 2.0 * 0.05;
        assert_eq!(s.avg_fps, Some(expected));
    }

    #[wasm_bindgen_test]
    fn record_frame_time_ignores_nonpositive_dt() {
        let mut s = SessionStats::new();
        s.record_frame_time(0.0);
        assert_eq!(s.avg_fps, None, "dt == 0 is ignored");
        s.record_frame_time(-1.0);
        assert_eq!(s.avg_fps, None, "dt < 0 is ignored");
        // A valid sample still seeds afterward.
        s.record_frame_time(0.25); // 4 fps
        assert_eq!(s.avg_fps, Some(4.0));
        // And a subsequent non-positive dt leaves the seeded value untouched.
        s.record_frame_time(0.0);
        assert_eq!(s.avg_fps, Some(4.0));
    }

    #[wasm_bindgen_test]
    fn record_render_time_seeds_then_blends() {
        let mut s = SessionStats::new();
        s.record_render_time(100.0);
        assert_eq!(s.avg_render_time_ms, Some(100.0));
        // Blend mirrors the code's exact expression: avg*(1.0-ALPHA) + v*ALPHA.
        s.record_render_time(200.0);
        assert_eq!(
            s.avg_render_time_ms,
            Some(100.0 * (1.0 - 0.2) + 200.0 * 0.2)
        );
    }

    #[wasm_bindgen_test]
    fn record_fetch_latency_seeds_then_blends() {
        let mut s = SessionStats::new();
        s.record_fetch_latency(50.0);
        assert_eq!(s.median_chunk_latency_ms, Some(50.0));
        // Blend mirrors the code's exact expression: avg*(1.0-ALPHA) + v*ALPHA.
        s.record_fetch_latency(150.0);
        assert_eq!(
            s.median_chunk_latency_ms,
            Some(50.0 * (1.0 - 0.2) + 150.0 * 0.2)
        );
    }

    #[wasm_bindgen_test]
    fn record_processing_time_seeds_then_blends() {
        let mut s = SessionStats::new();
        s.record_processing_time(300.0);
        assert_eq!(s.median_processing_time_ms, Some(300.0));
        // Blend mirrors the code's exact expression: avg*(1.0-ALPHA) + v*ALPHA.
        s.record_processing_time(800.0);
        assert_eq!(
            s.median_processing_time_ms,
            Some(300.0 * (1.0 - 0.2) + 800.0 * 0.2)
        );
    }

    #[wasm_bindgen_test]
    fn ema_converges_toward_steady_input() {
        // Repeated identical samples after a different seed converge toward the input.
        let mut s = SessionStats::new();
        s.record_render_time(0.0);
        for _ in 0..200 {
            s.record_render_time(100.0);
        }
        let v = s.avg_render_time_ms.unwrap();
        assert!((v - 100.0).abs() < 0.001, "EMA converged to {v}");
    }

    // ── DownloadProgress::is_active precedence matrix ──

    #[wasm_bindgen_test]
    fn is_active_idle_empty_is_false() {
        let p = DownloadProgress::default();
        assert_eq!(p.phase, DownloadPhase::Idle);
        assert!(!p.is_active());
    }

    #[wasm_bindgen_test]
    fn is_active_done_with_pending_is_true() {
        // Done phase is inactive, but pending scans keep it active (|| branch).
        let mut p = DownloadProgress::default();
        p.phase = DownloadPhase::Done;
        p.pending_scans.push((0, 10));
        assert!(p.is_active());
    }

    #[wasm_bindgen_test]
    fn is_active_downloading_empty_is_true() {
        let mut p = DownloadProgress::default();
        p.phase = DownloadPhase::Downloading;
        assert!(p.is_active(), "active phase alone is enough");
    }

    #[wasm_bindgen_test]
    fn is_active_idle_with_in_flight_is_true() {
        // Idle phase is inactive, but in-flight scans keep it active (|| branch).
        let mut p = DownloadProgress::default();
        p.phase = DownloadPhase::Idle;
        p.in_flight_scans.push((0, 10));
        assert!(p.is_active());
    }

    #[wasm_bindgen_test]
    fn is_active_done_empty_is_false() {
        let mut p = DownloadProgress::default();
        p.phase = DownloadPhase::Done;
        assert!(!p.is_active(), "Done + empty is not active");
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── PipelineStatus mark_* deterministic side effects ──
    // (The timestamp itself comes from js_sys::Date::now() and is not asserted;
    //  the flag clears and ever_active set are deterministic.)

    #[wasm_bindgen_test]
    fn mark_processing_done_clears_flag_and_marks_ever_active() {
        let mut p = PipelineStatus::default();
        p.processing = true;
        assert!(!p.ever_active);
        p.mark_processing_done();
        assert!(!p.processing, "processing flag cleared");
        assert!(p.ever_active, "ever_active set");
    }

    #[wasm_bindgen_test]
    fn mark_render_done_clears_flag_and_marks_ever_active() {
        let mut p = PipelineStatus::default();
        p.rendering = true;
        assert!(!p.ever_active);
        p.mark_render_done();
        assert!(!p.rendering, "rendering flag cleared");
        assert!(p.ever_active, "ever_active set");
    }

    // ── SessionStats::new defaults ──

    #[wasm_bindgen_test]
    fn session_stats_new_is_all_zero_and_none() {
        let s = SessionStats::new();
        assert_eq!(s.cache_size_bytes, 0);
        assert_eq!(s.session_request_count, 0);
        assert_eq!(s.session_transferred_bytes, 0);
        assert_eq!(s.active_request_count, 0);
        assert_eq!(s.median_chunk_latency_ms, None);
        assert_eq!(s.median_processing_time_ms, None);
        assert_eq!(s.avg_render_time_ms, None);
        assert_eq!(s.avg_fps, None);
        assert!(s.last_ingest_detail.is_none());
        assert!(s.last_render_detail.is_none());
    }

    // ── SessionStats::update_from_network_stats ──

    #[wasm_bindgen_test]
    fn update_from_network_stats_copies_counts() {
        let net = NetworkStats::new();
        // request_started increments both active and total counts.
        net.request_started();
        net.request_started();
        net.request_started();
        // One completes, transferring 4096 bytes; active drops to 2, total stays 3.
        net.request_completed(4096);

        let mut s = SessionStats::new();
        s.update_from_network_stats(&net);

        assert_eq!(s.session_request_count, 3, "total requests");
        assert_eq!(s.active_request_count, 2, "active = started - completed");
        assert_eq!(s.session_transferred_bytes, 4096);
    }

    #[wasm_bindgen_test]
    fn update_from_network_stats_zero_state() {
        let net = NetworkStats::new();
        let mut s = SessionStats::new();
        s.update_from_network_stats(&net);
        assert_eq!(s.session_request_count, 0);
        assert_eq!(s.active_request_count, 0);
        assert_eq!(s.session_transferred_bytes, 0);
    }

    // ── SessionStats::format_cache_size / format_transferred (delegate to format_bytes) ──

    #[wasm_bindgen_test]
    fn format_cache_size_units() {
        let mut s = SessionStats::new();
        s.cache_size_bytes = 0;
        assert_eq!(s.format_cache_size(), "0 B");
        s.cache_size_bytes = 1024;
        assert_eq!(s.format_cache_size(), "1 KB");
        s.cache_size_bytes = 1024 * 1024;
        assert_eq!(s.format_cache_size(), "1 MB");
        s.cache_size_bytes = 1024 * 1024 * 1024;
        assert_eq!(s.format_cache_size(), "1.0 GB");
    }

    #[wasm_bindgen_test]
    fn format_transferred_uses_transferred_bytes() {
        let mut s = SessionStats::new();
        s.session_transferred_bytes = 12 * 1024 * 1024; // 12 MB exactly
        assert_eq!(s.format_transferred(), "12 MB");
        // Below 1 KB renders as raw bytes.
        s.session_transferred_bytes = 512;
        assert_eq!(s.format_transferred(), "512 B");
    }

    // ── DownloadPhase default ──

    #[wasm_bindgen_test]
    fn download_phase_default_is_idle() {
        assert_eq!(DownloadPhase::default(), DownloadPhase::Idle);
    }

    // ── DownloadProgress::clear resets every field ──

    #[wasm_bindgen_test]
    fn clear_resets_all_state() {
        let mut p = DownloadProgress::default();
        p.pending_scans.push((1, 2));
        p.active_scans.push((3, 4));
        p.in_flight_scans.push((5, 6));
        p.phase = DownloadPhase::Decoding;
        p.batch_completed = 4;

        p.clear();

        assert!(p.pending_scans.is_empty());
        assert!(p.active_scans.is_empty());
        assert!(p.in_flight_scans.is_empty());
        assert_eq!(p.phase, DownloadPhase::Idle);
        assert_eq!(p.batch_completed, 0);
        assert!(!p.is_active(), "cleared progress is inactive");
    }
}
