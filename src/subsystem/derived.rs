//! Per-frame derived state snapshot.
//!
//! Populated once at the top of [`crate::WorkbenchApp::update`] so every
//! UI consumer within the same frame sees a consistent picture without
//! recomputing. Replaces ~25 scattered per-frame computations of
//! `data_is_live` (6 sites), `effective_sweep_animation` (6 sites),
//! and `visible_bounds` (10 sites) with one populated struct.
//!
//! Design notes:
//! - **`visible_bounds`** carries the *previous* frame's cached value
//!   from [`crate::state::VizState::last_visible_bounds`]. The canvas
//!   updates that field when it renders this frame, so non-canvas
//!   readers (top bar's alerts chip, alerts modal, hit-tests in
//!   `canvas_interaction.rs`) always see one-frame-stale bounds.
//!   That's the same semantics as before; centralising it here gives
//!   future consumers a single seam.
//! - **`frame_now_secs`** is captured once so consumers can't drift
//!   against each other when one calls `Date::now()` later in the
//!   frame than another.
//!
//! Not included (intentionally):
//! - `current_scan_ts` / `current_elevation_list`: callers that need
//!   these compute against `Timeline` + `Live` directly; the lookup is
//!   cheap enough that pre-computing would only add coupling.

use crate::state::AppState;
use crate::subsystem::Playback;

/// Per-frame snapshot of computed values shared across UI panels.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Derived {
    /// Wall-clock "now" in seconds since epoch, captured once at the
    /// top of the frame so every consumer agrees on it.
    pub frame_now_secs: f64,
    /// 2D visible bounds (min_lon, min_lat, max_lon, max_lat). Sourced
    /// from the previous frame's cached value on
    /// [`crate::state::VizState::last_visible_bounds`]; `None` in 3D
    /// mode or on the very first frame before the canvas has rendered.
    pub visible_bounds: Option<(f64, f64, f64, f64)>,
    /// Whether the playback cursor is within the live-overlay freshness
    /// window. Same semantics as
    /// [`crate::state::recency::data_is_live`].
    pub data_is_live: bool,
    /// Whether sweep animation is effectively enabled this frame
    /// (`render_processing.sweep_animation && micro mode && advanced`).
    pub effective_sweep_animation: bool,
}

impl Derived {
    /// Populate the snapshot. Called once near the top of `update()`,
    /// before any subsystem tick or panel render, so every consumer
    /// downstream reads the same values.
    pub(crate) fn for_frame(state: &AppState, playback: &Playback) -> Self {
        Self {
            frame_now_secs: state.frame_now.secs(),
            visible_bounds: state.viz_state.last_visible_bounds,
            data_is_live: crate::state::recency::data_is_live(&playback.state),
            effective_sweep_animation: state.effective_sweep_animation(&playback.state),
        }
    }
}
