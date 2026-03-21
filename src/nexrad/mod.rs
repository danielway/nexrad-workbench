//! NEXRAD data integration module.
//!
//! This module provides functionality for:
//! - Downloading archival NEXRAD data from AWS
//! - Pre-computed sweep storage in IndexedDB for near-zero render latency
//! - GPU-based radar rendering via WebGL2 shaders

mod archive_index;
mod cache_channel;
pub(crate) mod color_table;
mod decode_worker;
mod download;
pub(crate) mod download_queue;
pub(crate) mod globe_radar_renderer;
pub(crate) mod gpu_renderer;
pub(crate) mod ingest_phases;
pub(crate) mod network_monitor;
mod realtime;
pub(crate) mod record_decode;
pub(crate) mod render_coordinator;
pub(crate) mod render_request;
mod shader_common;
mod types;
pub(crate) mod volume_ray_renderer;
mod worker_api;

pub use archive_index::{ArchiveIndex, ScanBoundary};
pub use cache_channel::{CacheLoadChannel, CacheLoadResult};
pub use decode_worker::{
    ChunkIngestResult, DecodeResult, DecodeWorker, IngestResult, VolumeData, VolumeSweepMeta,
    WorkerOutcome,
};
pub use download::{DownloadChannel, ListingResult, NetworkStats};
pub use globe_radar_renderer::GlobeRadarRenderer;
pub use gpu_renderer::RadarGpuRenderer;
pub use network_monitor::{
    is_cross_origin_isolated, NetworkAggregate, NetworkMonitor, NetworkRequest,
};
pub use realtime::{BackfillChannel, RealtimeChannel, RealtimeResult};
pub use record_decode::extract_elevation_numbers;
pub use render_coordinator::RenderCoordinator;
pub use render_request::RenderRequest;
pub use types::{CachedScan, DownloadResult, ScanMetadata};
pub use volume_ray_renderer::VolumeRayRenderer;

/// Standard NEXRAD coverage range in km.
pub const RADAR_COVERAGE_RANGE_KM: f64 = 300.0;
