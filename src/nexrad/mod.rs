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
//!
//! The live-scan projection engine lives in [`crate::core::projection`]
//! (with the pure chunk-arrival timing physics in [`crate::core::timing`]);
//! the streaming loop here feeds it observations and reads its plans.

pub(crate) mod acquisition;
pub(crate) mod decode;
pub(crate) mod detection;
pub(crate) mod live;
pub(crate) mod render;

// Module aliases preserving pre-regroup `crate::nexrad::<module>` paths used
// outside this module.
pub(crate) use acquisition::download_queue;
pub(crate) use render::color_table;

pub(crate) use acquisition::acquisition_coordinator::AcquisitionCoordinator;
pub(crate) use acquisition::download::{ListingResult, NetworkStats};
pub(crate) use acquisition::types::DownloadResult;
pub(crate) use decode::decode_worker::{default_pool_size, WorkerOutcome, WorkerPool};
pub(crate) use live::realtime::{RealtimeChannel, RealtimeResult};
pub(crate) use render::globe_radar_renderer::GlobeRadarRenderer;
pub(crate) use render::gpu_renderer::RadarGpuRenderer;
pub(crate) use render::national_mosaic::NationalMosaic;
pub(crate) use render::render_coordinator::RenderCoordinator;
pub(crate) use render::volume_ray_renderer::VolumeRayRenderer;

/// Standard NEXRAD coverage range in km.
pub(crate) const RADAR_COVERAGE_RANGE_KM: f64 = 300.0;
