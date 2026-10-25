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
    /// Why live-only overlays (warnings, the national mosaic) are off this
    /// frame, `None` while they apply: the playhead is too far behind now,
    /// or a playing loop reaches further back (see
    /// [`crate::core::recency`]). It picks the Layers note's wording, and
    /// [`Self::data_is_live`] reads it. See
    /// [`crate::state::recency::live_overlays_off`].
    pub live_overlays_off: Option<crate::core::recency::LiveOverlaysOff>,
    /// Whether sweep animation is effectively enabled this frame: Rays mode,
    /// and — while attached to live with a fixed elevation filter — the
    /// antenna is on that cut.
    pub effective_sweep_animation: bool,
    /// Whether the canvas follows the live stream rather than the playhead.
    /// See [`crate::subsystem::Live::canvas_follows_live`].
    pub canvas_follows_live: bool,
}

impl Derived {
    /// Populate the snapshot. Called once near the top of `update()`,
    /// before any subsystem tick or panel render, so every consumer
    /// downstream reads the same values. `follows_live` is
    /// [`crate::subsystem::Live::canvas_follows_live`].
    /// `live` is the frame's live radar snapshot (the in-progress cut).
    pub(crate) fn for_frame(
        state: &AppState,
        playback: &Playback,
        follows_live: bool,
        live: &crate::core::LiveRadarModel,
    ) -> Self {
        Self {
            frame_now_secs: state.frame_now.secs(),
            visible_bounds: state.viz_state.last_visible_bounds,
            live_overlays_off: crate::state::recency::live_overlays_off(&playback.state),
            effective_sweep_animation: state.effective_sweep_animation(
                &playback.state,
                follows_live,
                live,
            ),
            canvas_follows_live: follows_live,
        }
    }

    /// Whether live-only overlays apply this frame (nothing in
    /// `live_overlays_off`): the one answer the mosaic fetch, alert
    /// polling, canvas and legends gate on.
    pub(crate) fn data_is_live(&self) -> bool {
        self.live_overlays_off.is_none()
    }
}
