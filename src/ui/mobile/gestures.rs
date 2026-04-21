//! Multi-touch gesture digestion.
//!
//! egui surfaces multi-touch state via `InputState::multi_touch()` which
//! returns `Some(MultiTouchInfo)` whenever two or more fingers are on the
//! surface. This module reads that state and produces `TouchDeltas` already
//! mapped into the units the canvas code wants (pan in screen points, zoom
//! as a multiplicative factor, focus in screen coords).
//!
//! When a pinch is in progress, callers should skip the single-finger
//! `drag_delta`/scroll-wheel paths to avoid double-applying motion.

use eframe::egui::{self, Pos2, Vec2};

/// Digested touch gesture for this frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TouchDeltas {
    /// Pan in screen points (add directly to `pan_offset`).
    pub pan: Vec2,
    /// Multiplicative zoom (1.0 = no change, >1 = zoom in, <1 = zoom out).
    pub zoom: f32,
    /// Screen-space focus point for the zoom (keep this point fixed while
    /// scaling so pinch feels anchored to the fingers).
    pub focus: Pos2,
}

/// Read the current multi-touch state, if any. Returns `None` when fewer
/// than two fingers are on the surface.
pub(crate) fn consume(ctx: &egui::Context) -> Option<TouchDeltas> {
    ctx.input(|i| i.multi_touch()).map(|info| TouchDeltas {
        pan: info.translation_delta,
        zoom: info.zoom_delta,
        focus: info.center_pos,
    })
}
