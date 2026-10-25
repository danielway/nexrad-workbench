//! Render subsystem: the decode worker pool every site renders through.
//!
//! What each site is rendering (its render track, crossfade cache and scrub
//! cache) lives on its [`crate::subsystem::sites::SiteState`]; the pool is
//! shared, so it stays here.
//!
//! GPU resources: the GL context, the globe sphere and the geographic line
//! renderers are shared ([`crate::GpuResources`]); the radar renderers are
//! per site.

use crate::nexrad::RenderCoordinator;

/// Owner of the worker render pipeline.
pub(crate) struct Render {
    /// Worker pool and the request methods, deduplicated per site.
    pub coordinator: RenderCoordinator,
}

impl Render {
    pub(crate) fn new(coordinator: RenderCoordinator) -> Self {
        Self { coordinator }
    }
}
