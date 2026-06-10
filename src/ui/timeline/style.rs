//! Shared layout and typography constants for the timeline.
//!
//! All track heights live here so the bottom panel and the timeline
//! renderer agree on the panel budget, and so zooming between detail
//! levels never changes the total height (no panel reflow).

use eframe::egui::FontId;

/// Height of the tick-label lane above the scan track.
pub(crate) const TICK_LANE_H: f32 = 14.0;
/// Height of the scan (volume) track when the sweep track is visible.
pub(crate) const SCAN_TRACK_H: f32 = 27.0;
/// Separator between the scan and sweep tracks.
pub(crate) const TRACK_SEPARATOR_H: f32 = 1.0;
/// Height of the sweep (tilt) track.
pub(crate) const SWEEP_TRACK_H: f32 = 22.0;
/// Scan track height when the sweep track is hidden — absorbs the sweep
/// track's space so the total stays constant across detail levels.
pub(crate) const EXPANDED_SCAN_TRACK_H: f32 = SCAN_TRACK_H + TRACK_SEPARATOR_H + SWEEP_TRACK_H;
/// Total timeline height, constant across detail levels.
pub(crate) const TIMELINE_TOTAL_H: f32 = TICK_LANE_H + EXPANDED_SCAN_TRACK_H;

/// Font for text inside scan/sweep blocks (labels, counts, countdowns).
pub(crate) fn block_font() -> FontId {
    FontId::monospace(9.0)
}

/// Font for time tick labels in the tick lane.
pub(crate) fn tick_font() -> FontId {
    FontId::monospace(9.0)
}
