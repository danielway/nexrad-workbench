//! Shared layout and typography constants for the timeline.
//!
//! The strip is one fixed-height widget across every zoom tier (no panel
//! reflow). Its vertical budget is split into named bands so later phases can
//! insert a minimap sliver above and loop handles below without re-deriving
//! every rect:
//!
//! ```text
//!   [ minimap sliver ]   ← reserved (Phase 3), 0px today
//!   [ tick rail      ]   TICK_LANE_H
//!   [ main track     ]   MAIN_TRACK_H   ← scan containers + frame cells
//!   [ loop handles   ]   ← reserved (Phase 3), 0px today
//! ```
//!
//! Total height is `TIMELINE_TOTAL_H = TICK_LANE_H + MAIN_TRACK_H` and is
//! constant across Micro / Macro / Archive so the bottom panel never reflows.

use eframe::egui::FontId;

/// Height of the tick-label rail above the main track (spec §6: ~14px).
pub(crate) const TICK_LANE_H: f32 = 14.0;

/// Height of the single main track that holds scan containers and their
/// frame cells (spec §6 "full strip ~56px" = 14 tick + 42 main).
pub(crate) const MAIN_TRACK_H: f32 = 42.0;

/// Reserved sliver above the tick rail for the Phase-3 minimap. Zero today;
/// kept named so [`TIMELINE_TOTAL_H`] and the rect math already account for it.
pub(crate) const MINIMAP_SLIVER_H: f32 = 0.0;

/// Reserved band below the main track for Phase-3 loop handles. Zero today.
pub(crate) const LOOP_HANDLE_H: f32 = 0.0;

/// Total timeline height, constant across detail levels.
pub(crate) const TIMELINE_TOTAL_H: f32 =
    MINIMAP_SLIVER_H + TICK_LANE_H + MAIN_TRACK_H + LOOP_HANDLE_H;

/// Vertical inset of frame cells / sub-texture inside their scan container, so
/// the container's bounding box reads as a frame around its contents.
pub(crate) const CELL_INSET_Y: f32 = 4.0;

/// Vertical inset of the scan-container box inside the main track.
pub(crate) const CONTAINER_INSET_Y: f32 = 2.0;

/// Font for text inside scan/frame cells (labels, counts, countdowns).
pub(crate) fn block_font() -> FontId {
    FontId::monospace(9.0)
}

/// Font for time tick labels in the tick lane.
pub(crate) fn tick_font() -> FontId {
    FontId::monospace(9.0)
}
