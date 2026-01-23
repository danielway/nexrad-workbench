//! NEXRAD data integration module.
//!
//! This module provides functionality for:
//! - Downloading archival NEXRAD data from AWS
//! - Caching downloaded data in IndexedDB (with separate metadata store for fast queries)
//! - High-performance radar rendering via nexrad-render with texture caching

mod archive_index;
mod cache;
mod cache_channel;
mod download;
mod realtime;
mod texture_cache;
mod texture_render;
mod types;

pub use archive_index::ArchiveIndex;
pub use cache::NexradCache;
pub use cache_channel::{CacheLoadChannel, CacheLoadResult, ScrubLoadChannel, ScrubLoadResult};
pub use download::{DownloadChannel, ListingResult, NetworkStats};
pub use realtime::{RealtimeChannel, RealtimeResult};
pub use texture_cache::{RadarCacheKey, RadarTextureCache};
pub use texture_render::{radar_coverage_range_km, render_sweep_to_image};
pub use types::{CachedScan, DownloadResult, ScanKey, ScanMetadata};
