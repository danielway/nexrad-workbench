//! NEXRAD data integration module.
//!
//! This module provides functionality for:
//! - Downloading archival NEXRAD data from AWS
//! - Pre-computed sweep storage in IndexedDB for near-zero render latency
//! - GPU-based radar rendering via WebGL2 shaders

mod archive_index;
mod cache_channel;
mod decode_worker;
mod download;
pub(crate) mod globe_radar_renderer;
pub(crate) mod gpu_renderer;
mod realtime;
pub(crate) mod record_decode;
mod types;
mod worker_api;

pub use archive_index::ArchiveIndex;
pub use cache_channel::{CacheLoadChannel, CacheLoadResult};
pub use decode_worker::{DecodeWorker, WorkerOutcome};
pub use download::{DownloadChannel, ListingResult, NetworkStats};
pub use globe_radar_renderer::GlobeRadarRenderer;
pub use gpu_renderer::RadarGpuRenderer;
pub use realtime::{RealtimeChannel, RealtimeResult};
pub use record_decode::extract_elevation_numbers;
pub use types::{CachedScan, DownloadResult, ScanMetadata};

/// Standard NEXRAD coverage range in km.
pub const RADAR_COVERAGE_RANGE_KM: f64 = 300.0;
