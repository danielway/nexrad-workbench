//! NEXRAD data integration module.
//!
//! This module provides functionality for:
//! - Downloading archival NEXRAD data from AWS
//! - Caching downloaded data in IndexedDB
//! - High-performance radar rendering via nexrad-render with texture caching

mod cache;
mod download;
mod texture_cache;
mod texture_render;
mod types;

pub use cache::NexradCache;
pub use download::DownloadChannel;
pub use texture_cache::{RadarCacheKey, RadarTextureCache};
pub use texture_render::{radar_coverage_range_km, render_sweep_to_image};
pub use types::{CachedScan, DownloadResult};
