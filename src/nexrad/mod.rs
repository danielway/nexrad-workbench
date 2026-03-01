//! NEXRAD data integration module.
//!
//! This module provides functionality for:
//! - Downloading archival NEXRAD data from AWS
//! - Caching downloaded data in IndexedDB via record-based storage
//! - High-performance radar rendering via nexrad-render with texture caching

mod archive_index;
mod cache_channel;
mod decode_worker;
mod download;
mod realtime;
mod record_decode;
mod texture_cache;
mod texture_render;
#[allow(dead_code)]
mod types;

pub use archive_index::ArchiveIndex;
pub use cache_channel::{CacheLoadChannel, CacheLoadResult, ScrubLoadChannel, ScrubLoadResult};
pub use decode_worker::{DecodeWorker, WorkerOutcome};
pub use download::{DownloadChannel, ListingResult, NetworkStats};
pub use realtime::{RealtimeChannel, RealtimeResult};
pub use record_decode::{decode_record_to_radials, probe_record_elevations};
pub use texture_cache::{RadarCacheKey, RadarTextureCache};
pub use texture_render::radar_coverage_range_km;
pub use types::{CachedScan, DownloadResult, ScanMetadata};
