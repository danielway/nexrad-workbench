//! Session and performance statistics for the status bar.

use crate::nexrad::NetworkStats;

/// Active pipeline phase flags (3 high-level groups).
///
/// Each group tracks both a live flag and a "last completed" timestamp (ms).
/// The UI uses the timestamp to keep groups visually lit for a short period
/// after they finish, so the user can see which stages ran even when they
/// complete within a single frame.
#[derive(Default, Clone)]
pub struct PipelineStatus {
    /// Number of active downloads (group 1: Download).
    pub downloading: u32,
    /// Whether processing is in progress: ingest + decode in worker (group 2: Processing).
    pub processing: bool,
    /// Whether GPU rendering/upload is in progress (group 3: Rendering).
    pub rendering: bool,

    /// Timestamp (ms since epoch) when each group last completed.
    /// Used by the UI to keep indicators lit briefly after completion.
    pub last_download_done_ms: f64,
    pub last_processing_done_ms: f64,
    pub last_render_done_ms: f64,

    /// Whether any pipeline activity has occurred this session.
    pub ever_active: bool,
}

impl PipelineStatus {
    /// How long (in ms) a phase stays "recently completed" in the UI.
    const LINGER_MS: f64 = 1500.0;

    /// Whether a phase is active or recently completed.
    pub fn phase_visible(&self, active: bool, last_done_ms: f64) -> bool {
        if active {
            return true;
        }
        if last_done_ms <= 0.0 {
            return false;
        }
        let now = js_sys::Date::now();
        (now - last_done_ms) < Self::LINGER_MS
    }

    pub fn is_active(&self) -> bool {
        self.downloading > 0 || self.processing || self.rendering
    }

    /// Whether the indicator row should be shown at all.
    pub fn should_show(&self) -> bool {
        if self.is_active() {
            return true;
        }
        // Show if any group completed recently
        let now = js_sys::Date::now();
        (now - self.last_download_done_ms) < Self::LINGER_MS
            || (now - self.last_processing_done_ms) < Self::LINGER_MS
            || (now - self.last_render_done_ms) < Self::LINGER_MS
    }

    /// Mark processing phase as completed (ingest + decode finished).
    pub fn mark_processing_done(&mut self) {
        self.processing = false;
        self.last_processing_done_ms = js_sys::Date::now();
        self.ever_active = true;
    }

    /// Mark rendering phase as completed (GPU upload finished).
    pub fn mark_render_done(&mut self) {
        self.rendering = false;
        self.last_render_done_ms = js_sys::Date::now();
        self.ever_active = true;
    }
}

/// Detailed sub-phase timings from the most recent ingest operation.
#[derive(Default, Clone)]
pub struct IngestTimingDetail {
    pub split_ms: f64,
    pub decompress_ms: f64,
    pub decode_ms: f64,
    pub extract_ms: f64,
    pub store_ms: f64,
    pub index_ms: f64,
}

/// Detailed sub-phase timings from the most recent render/decode operation.
#[derive(Default, Clone)]
pub struct RenderTimingDetail {
    pub fetch_ms: f64,
    pub deser_ms: f64,
    pub marshal_ms: f64,
    pub gpu_upload_ms: f64,
}

/// Statistics displayed in the status bar.
#[derive(Default, Clone)]
pub struct SessionStats {
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
}

impl SessionStats {
    /// Create stats with initial (zero) values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update stats from live network statistics.
    pub fn update_from_network_stats(&mut self, network_stats: &NetworkStats) {
        self.session_request_count = network_stats.total_count();
        self.session_transferred_bytes = network_stats.bytes_transferred();
        self.active_request_count = network_stats.active_count();
        self.pipeline.downloading = self.active_request_count;
    }

    /// Record a frame time sample from `stable_dt`, updating the FPS average.
    /// Uses exponential moving average with alpha=0.05 for a smooth readout.
    pub fn record_frame_time(&mut self, dt: f32) {
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
    pub fn record_render_time(&mut self, time_ms: f64) {
        const ALPHA: f64 = 0.2;
        self.avg_render_time_ms = Some(match self.avg_render_time_ms {
            Some(avg) => avg * (1.0 - ALPHA) + time_ms * ALPHA,
            None => time_ms,
        });
    }

    /// Format cache size for display (e.g., "150.2 MB").
    pub fn format_cache_size(&self) -> String {
        format_bytes(self.cache_size_bytes)
    }

    /// Format transferred bytes for display (e.g., "12.0 MB").
    pub fn format_transferred(&self) -> String {
        format_bytes(self.session_transferred_bytes)
    }

    /// Record a fetch latency sample, updating the running average.
    pub fn record_fetch_latency(&mut self, ms: f64) {
        const ALPHA: f64 = 0.2;
        self.median_chunk_latency_ms = Some(match self.median_chunk_latency_ms {
            Some(avg) => avg * (1.0 - ALPHA) + ms * ALPHA,
            None => ms,
        });
    }

    /// Record a processing time sample (full ingest total), updating the running average.
    pub fn record_processing_time(&mut self, ms: f64) {
        const ALPHA: f64 = 0.2;
        self.median_processing_time_ms = Some(match self.median_processing_time_ms {
            Some(avg) => avg * (1.0 - ALPHA) + ms * ALPHA,
            None => ms,
        });
    }

    /// Format latency statistics for display.
    pub fn format_latency_stats(&self) -> String {
        let mut parts = Vec::new();

        if let Some(latency) = self.median_chunk_latency_ms {
            parts.push(format!("dl: {:.0}ms", latency));
        }
        if let Some(proc_time) = self.median_processing_time_ms {
            parts.push(format!("proc: {:.0}ms", proc_time));
        }
        if let Some(render) = self.avg_render_time_ms {
            parts.push(format!("gpu: {:.0}ms", render));
        }

        if parts.is_empty() {
            "\u{2014}".to_string()
        } else {
            parts.join(" \u{00b7} ")
        }
    }
}

/// Which phase of the download pipeline the current file is in.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub enum DownloadPhase {
    #[default]
    Idle,
    /// Fetching from AWS S3.
    Downloading,
    /// Worker is splitting, decompressing, decoding, and storing in IDB.
    Ingesting,
    /// Worker is decoding and rendering the sweep.
    Decoding,
    /// Complete.
    Done,
}

/// Tracks download progress for timeline ghost markers and pipeline display.
///
/// Scan boundaries are `(start_secs, end_secs)` pairs derived from the archive
/// listing's adjacent file timestamps, giving accurate ghost widths on the timeline.
#[derive(Default, Clone)]
pub struct DownloadProgress {
    /// Scan boundaries (start, end) of files queued but not yet loaded.
    /// The timeline renders ghost markers spanning these intervals.
    pub pending_scans: Vec<(i64, i64)>,
    /// Boundaries of files currently being downloaded.
    /// Their ghost markers pulse to distinguish them from queued items.
    /// Multiple entries when parallel downloads are in flight.
    pub active_scans: Vec<(i64, i64)>,
    /// Phase of the currently active file.
    pub phase: DownloadPhase,
    /// Batch total file count.
    pub batch_total: u32,
    /// Number of files completed so far.
    pub batch_completed: u32,
    /// Scan boundaries of files downloaded but still being ingested/decoded/rendered.
    /// Ghosts for these stay visible until processing completes.
    pub in_flight_scans: Vec<(i64, i64)>,
    /// Scans that recently completed processing, with wall-clock completion time.
    /// Used for brief flash animation. Entries older than 1s are pruned in render.
    pub recently_completed: Vec<(i64, f64)>,
}

impl DownloadProgress {
    /// Whether this is a multi-file batch download.
    pub fn is_batch(&self) -> bool {
        self.batch_total > 1
    }

    /// Whether any download operation is active.
    pub fn is_active(&self) -> bool {
        self.phase != DownloadPhase::Idle && self.phase != DownloadPhase::Done
            || !self.pending_scans.is_empty()
            || !self.in_flight_scans.is_empty()
    }

    /// Reset all progress state.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

use super::settings::format_bytes;
