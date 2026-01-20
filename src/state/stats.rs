//! Session and performance statistics for the top bar.

use crate::nexrad::NetworkStats;

/// Statistics displayed in the top bar.
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

    /// Median decompression time in milliseconds.
    pub median_decompression_time_ms: Option<f64>,

    /// Median decoding time in milliseconds.
    pub median_decode_time_ms: Option<f64>,
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
            median_decompression_time_ms: Some(8.3),
            median_decode_time_ms: Some(23.7),
        }
    }

    /// Update stats from live network statistics.
    pub fn update_from_network_stats(&mut self, network_stats: &NetworkStats) {
        self.session_request_count = network_stats.total_count();
        self.session_transferred_bytes = network_stats.bytes_transferred();
        self.active_request_count = network_stats.active_count();
    }

    /// Format cache size for display (e.g., "150.2 MB").
    pub fn format_cache_size(&self) -> String {
        format_bytes(self.cache_size_bytes)
    }

    /// Format transferred bytes for display (e.g., "12.0 MB").
    pub fn format_transferred(&self) -> String {
        format_bytes(self.session_transferred_bytes)
    }

    /// Format latency statistics for display.
    pub fn format_latency_stats(&self) -> String {
        let mut parts = Vec::new();

        if let Some(latency) = self.median_chunk_latency_ms {
            parts.push(format!("fetch: {:.0}ms", latency));
        }
        if let Some(decomp) = self.median_decompression_time_ms {
            parts.push(format!("decomp: {:.1}ms", decomp));
        }
        if let Some(decode) = self.median_decode_time_ms {
            parts.push(format!("decode: {:.1}ms", decode));
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
