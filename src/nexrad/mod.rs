//! NEXRAD data integration module.
//!
//! This module provides the full data pipeline from network to pixels:
//! - **Acquisition**: Archive downloads from AWS S3 and real-time chunk streaming
//! - **Ingestion**: Record splitting, bzip2 decompression, VCP extraction, and
//!   pre-computed sweep storage in IndexedDB (runs in Web Worker)
//! - **Rendering**: GPU-based radar rendering via WebGL2 shaders with polar-to-Cartesian
//!   conversion, OKLab color interpolation, and 3D globe/volume ray-marching
//! - **Coordination**: Request deduplication, download queuing, streaming lifecycle,
//!   URL persistence, and service worker network monitoring

pub(crate) mod acquisition_coordinator;
mod archive_index;
mod cache_channel;
pub(crate) mod color_table;
mod decode_worker;
pub(crate) mod detection;
mod download;
pub(crate) mod download_queue;
pub(crate) mod globe_radar_renderer;
pub(crate) mod gpu_renderer;
pub(crate) mod ingest_phases;
mod national_mosaic;
pub(crate) mod network_monitor;
pub(crate) mod persistence_manager;
mod realtime;
pub(crate) mod record_decode;
pub(crate) mod render_coordinator;
pub(crate) mod render_request;
pub(crate) mod streaming_manager;
mod streaming_state;
mod types;
mod volume_discovery;
pub(crate) mod volume_ray_renderer;
mod worker_api;

pub use acquisition_coordinator::AcquisitionCoordinator;
pub use archive_index::ScanBoundary;
pub use cache_channel::CacheLoadResult;
pub use decode_worker::{
    default_pool_size, ChunkIngestResult, DecodeResult, IngestResult, VolumeData, VolumeSweepMeta,
    WorkerOutcome, WorkerPool,
};
pub use download::{ListingResult, NetworkStats};
pub use globe_radar_renderer::GlobeRadarRenderer;
pub use gpu_renderer::RadarGpuRenderer;
pub use national_mosaic::NationalMosaic;
pub use network_monitor::{
    is_cross_origin_isolated, NetworkAggregate, NetworkMonitor, NetworkRequest,
};
pub use persistence_manager::PersistenceManager;
pub use realtime::{ChunkProjectionInfo, RealtimeChannel, RealtimeResult};
pub use render_coordinator::RenderCoordinator;
pub use render_request::RenderRequest;
pub use streaming_manager::{StreamingEvent, StreamingManager};
pub use types::{DownloadResult, ScanMetadata};
pub use volume_ray_renderer::VolumeRayRenderer;

/// Standard NEXRAD coverage range in km.
pub const RADAR_COVERAGE_RANGE_KM: f64 = 300.0;
