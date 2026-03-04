//! Session and performance statistics for the status bar.

use crate::nexrad::NetworkStats;

/// Active pipeline phase flags.
///
/// Each phase tracks both a live flag and a "last completed" timestamp (ms).
/// The UI uses the timestamp to keep phases visually lit for a short period
/// after they finish, so the user can see which phases ran even when they
/// complete within a single frame.
#[derive(Default, Clone)]
pub struct PipelineStatus {
    /// Number of active downloads.
    pub downloading: u32,
    /// Whether decoding is in progress.
    pub decoding: bool,
    /// Whether IDB store is in progress.
    pub storing: bool,
    /// Whether GPU rendering is in progress.
    pub rendering: bool,

    /// Timestamp (ms since epoch) when each phase last completed.
    /// Used by the UI to keep indicators lit briefly after completion.
    pub last_download_done_ms: f64,
    pub last_store_done_ms: f64,
    pub last_decode_done_ms: f64,
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
        self.downloading > 0 || self.decoding || self.storing || self.rendering
    }

    /// Whether the indicator row should be shown at all.
    pub fn should_show(&self) -> bool {
        if self.is_active() {
            return true;
        }
        // Show if any phase completed recently
        let now = js_sys::Date::now();
        (now - self.last_download_done_ms) < Self::LINGER_MS
            || (now - self.last_store_done_ms) < Self::LINGER_MS
            || (now - self.last_decode_done_ms) < Self::LINGER_MS
            || (now - self.last_render_done_ms) < Self::LINGER_MS
    }

    /// Mark a download as completed (timestamp the finish).
    pub fn mark_download_done(&mut self) {
        self.last_download_done_ms = js_sys::Date::now();
        self.ever_active = true;
    }

    /// Mark store phase as completed.
    pub fn mark_store_done(&mut self) {
        self.storing = false;
        self.last_store_done_ms = js_sys::Date::now();
        self.ever_active = true;
    }

    /// Mark decode phase as completed.
    pub fn mark_decode_done(&mut self) {
        self.decoding = false;
        self.last_decode_done_ms = js_sys::Date::now();
        self.ever_active = true;
    }
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

    /// Median chunk fetch latency in milliseconds.
    pub median_chunk_latency_ms: Option<f64>,

    /// Median archive store (split + IDB write) time in milliseconds.
    pub median_store_time_ms: Option<f64>,

    /// Median decoding time in milliseconds.
    pub median_decode_time_ms: Option<f64>,

    /// Running average of radar render time in milliseconds.
    pub avg_render_time_ms: Option<f64>,

    /// Running average of frames per second.
    pub avg_fps: Option<f64>,

    /// Current pipeline phase status.
    pub pipeline: PipelineStatus,
}

impl SessionStats {
    /// Create stats with initial (zero) values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create stats with dummy data for UI testing.
    #[allow(dead_code)]
    pub fn with_dummy_data() -> Self {
        Self {
            cache_size_bytes: 156_842_496, // ~150 MB
            session_request_count: 47,
            session_transferred_bytes: 12_582_912, // ~12 MB
            active_request_count: 3,
            median_chunk_latency_ms: Some(142.5),
            median_store_time_ms: Some(8.3),
            median_decode_time_ms: Some(23.7),
            avg_render_time_ms: Some(45.0),
            avg_fps: Some(60.0),
            pipeline: PipelineStatus::default(),
        }
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

    /// Record an archive store (split + IDB write) time sample.
    pub fn record_store_time(&mut self, ms: f64) {
        const ALPHA: f64 = 0.2;
        self.median_store_time_ms = Some(match self.median_store_time_ms {
            Some(avg) => avg * (1.0 - ALPHA) + ms * ALPHA,
            None => ms,
        });
    }

    /// Record a decode time sample, updating the running average.
    pub fn record_decode_time(&mut self, ms: f64) {
        const ALPHA: f64 = 0.2;
        self.median_decode_time_ms = Some(match self.median_decode_time_ms {
            Some(avg) => avg * (1.0 - ALPHA) + ms * ALPHA,
            None => ms,
        });
    }

    /// Format latency statistics for display.
    pub fn format_latency_stats(&self) -> String {
        let mut parts = Vec::new();

        if let Some(latency) = self.median_chunk_latency_ms {
            parts.push(format!("fetch: {:.0}ms", latency));
        }
        if let Some(store) = self.median_store_time_ms {
            parts.push(format!("store: {:.1}ms", store));
        }
        if let Some(decode) = self.median_decode_time_ms {
            parts.push(format!("decode: {:.1}ms", decode));
        }
        if let Some(render) = self.avg_render_time_ms {
            parts.push(format!("render: {:.0}ms", render));
        }

        if parts.is_empty() {
            "—".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// Format bytes into a human-readable string.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
