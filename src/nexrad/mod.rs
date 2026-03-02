//! NEXRAD data integration module.
//!
//! This module provides functionality for:
//! - Downloading archival NEXRAD data from AWS
//! - Caching downloaded data in IndexedDB via record-based storage
//! - GPU-based radar rendering via WebGL2 shaders

mod archive_index;
mod cache_channel;
mod decode_worker;
mod download;
pub mod gpu_renderer;
mod realtime;
pub(crate) mod record_decode;
#[allow(dead_code)]
mod types;

pub use archive_index::ArchiveIndex;
pub use cache_channel::{CacheLoadChannel, CacheLoadResult, ScrubLoadChannel, ScrubLoadResult};
pub use decode_worker::{DecodeWorker, WorkerOutcome};
pub use download::{DownloadChannel, ListingResult, NetworkStats};
pub use gpu_renderer::RadarGpuRenderer;
pub use realtime::{RealtimeChannel, RealtimeResult};
pub use record_decode::{extract_elevation_numbers, probe_record_elevations};
pub use types::{CachedScan, DownloadResult, ScanMetadata};

/// Standard NEXRAD coverage range in km.
pub const RADAR_COVERAGE_RANGE_KM: f64 = 300.0;
