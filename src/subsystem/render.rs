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

/// Owner of the worker render pipeline + the sweep-animation cache.
pub struct Render {
    /// Worker pool, active scan tracking, render-request dedup.
    pub coordinator: RenderCoordinator,
    /// Previous-sweep cache + resolution for sweep crossfade animation.
    pub playback_manager: PlaybackManager,
}

impl Render {
    pub fn new(coordinator: RenderCoordinator) -> Self {
        Self {
            coordinator,
            playback_manager: PlaybackManager::new(),
        }
    }
}
