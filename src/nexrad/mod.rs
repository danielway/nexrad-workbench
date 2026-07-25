//! NEXRAD data integration module.
//!
//! This module provides the full data pipeline from network to pixels:
//! - **Acquisition** ([`acquisition`]): Archive downloads from AWS S3, the
//!   download queue, archive indexing, and IndexedDB cache-load channels
//! - **Live** ([`live`]): Real-time chunk streaming, its lifecycle state
//!   machine, and per-volume streaming plans
//! - **Decode** ([`decode`]): Record splitting, bzip2 decompression, VCP
//!   extraction, and pre-computed sweep storage in IndexedDB (runs in Web
//!   Worker), plus the main-thread worker pool
//! - **Render** ([`render`]): GPU-based radar rendering via WebGL2 shaders
//!   with polar-to-Cartesian conversion, OKLab color interpolation, and 3D
//!   globe/volume ray-marching
//! - **Timing/projection** ([`timing`], [`projection`]): chunk-arrival
//!   physics and the live-scan projection engine

pub(crate) mod acquisition;
pub(crate) mod decode;
pub(crate) mod detection;
pub(crate) mod live;
pub(crate) mod projection;
mod projector;
pub(crate) mod render;
pub(crate) mod timing;

// Module aliases preserving pre-regroup `crate::nexrad::<module>` paths used
// outside this module.
pub(crate) use acquisition::download_queue;
pub(crate) use render::color_table;

pub(crate) use acquisition::acquisition_coordinator::AcquisitionCoordinator;
pub(crate) use acquisition::archive_index::ScanBoundary;
pub(crate) use acquisition::cache_channel::CacheLoadResult;
pub(crate) use acquisition::download::{ListingResult, NetworkStats};
pub(crate) use acquisition::types::DownloadResult;
pub(crate) use decode::decode_worker::{
    default_pool_size, ChunkIngestResult, DecodeResult, IngestResult, VolumeData, VolumeSweepMeta,
    WorkerOutcome, WorkerPool,
};
pub(crate) use live::realtime::{
    ChunkProjectedTimes, ChunkProjectionInfo, RealtimeChannel, RealtimeResult,
};
pub(crate) use live::streaming_plan::StreamingPlan;
pub(crate) use projector::ProjectorObservation;
pub(crate) use render::globe_radar_renderer::GlobeRadarRenderer;
pub(crate) use render::gpu_renderer::RadarGpuRenderer;
pub(crate) use render::national_mosaic::NationalMosaic;
pub(crate) use render::render_coordinator::RenderCoordinator;
pub(crate) use render::volume_ray_renderer::VolumeRayRenderer;

/// Standard NEXRAD coverage range in km.
pub(crate) const RADAR_COVERAGE_RANGE_KM: f64 = 300.0;
