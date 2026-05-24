//! Render subsystem: owns the worker-driven decode pipeline + the
//! sweep-animation cache that lives in front of it.
//!
//! Before this subsystem existed:
//! - [`crate::nexrad::RenderCoordinator`] (held as `WorkbenchApp.render`)
//!   owned the worker pool, the current scan/elevation tracking, and
//!   the per-frame render dedup cache.
//! - [`crate::state::playback_manager::PlaybackManager`] (held as
//!   `WorkbenchApp.playback_manager`) owned the previous-sweep cache
//!   and the resolution logic that drives sweep-animation crossfade.
//!
//! Both run in the same per-frame render slice and both ultimately
//! decide *what bytes get on screen this frame*. Folding them together
//! keeps `WorkbenchApp` to one render field and gives the caller a
//! single typed handle for the render side of the loop.
//!
//! GPU resources (the GL context + per-shader renderers) stay in
//! [`crate::GpuResources`] for now because they have a separate
//! lifecycle — eframe wires them up at startup from the creation
//! context and they're never re-created.

use crate::nexrad::RenderCoordinator;
use crate::state::playback_manager::PlaybackManager;

/// Per-frame inputs the scrub-detection cache compares against.
///
/// `advance_playback` skips the O(scans) timeline search when none of
/// these have changed since the last frame; that's the whole point of
/// the cache. The active scan timestamp catches ingest-driven scan
/// changes that happen without playback movement.
#[derive(Default)]
pub struct ScrubCache {
    pub last_playback_ts: Option<f64>,
    pub last_elevation_selection: Option<crate::state::ElevationSelection>,
    pub last_scan_count: usize,
    /// Active scan timestamp (sub-second Unix seconds) from
    /// `RenderCoordinator::scan_key`.
    pub last_active_scan_ts: Option<f64>,
}

/// Owner of the worker render pipeline + the sweep-animation cache.
pub struct Render {
    /// Worker pool, active scan tracking, render-request dedup.
    pub coordinator: RenderCoordinator,
    /// Previous-sweep cache + resolution for sweep crossfade animation.
    pub playback_manager: PlaybackManager,
    /// Per-frame scrub-detection cache used by `advance_playback`.
    pub scrub_cache: ScrubCache,
}

impl Render {
    pub fn new(coordinator: RenderCoordinator) -> Self {
        Self {
            coordinator,
            playback_manager: PlaybackManager::new(),
            scrub_cache: ScrubCache::default(),
        }
    }
}
