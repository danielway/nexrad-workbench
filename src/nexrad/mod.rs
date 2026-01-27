//! NEXRAD data integration module.
//!
//! This module provides functionality for:
//! - Downloading archival NEXRAD data from AWS
//! - Caching downloaded data in IndexedDB via v4 record-based storage
//! - High-performance radar rendering via nexrad-render with texture caching
//! - Dynamic sweep rendering across multiple volumes

mod archive_index;
mod cache_channel;
mod download;
mod realtime;
mod render_sweep;
mod sweep_animator;
mod texture_cache;
mod texture_render;
mod types;
mod volume_ring;

pub use archive_index::ArchiveIndex;
pub use cache_channel::{CacheLoadChannel, CacheLoadResult, ScrubLoadChannel, ScrubLoadResult};
pub use download::{DownloadChannel, ListingResult, NetworkStats};
pub use realtime::{RealtimeChannel, RealtimeResult};
pub use render_sweep::RenderSweep;
pub use sweep_animator::{AnimationState, SweepAnimator};
pub use texture_cache::{RadarCacheKey, RadarTextureCache};
pub use texture_render::{radar_coverage_range_km, render_radials_to_image};
pub use types::{CachedScan, DownloadResult, ScanKey, ScanMetadata};
pub use volume_ring::VolumeRing;
