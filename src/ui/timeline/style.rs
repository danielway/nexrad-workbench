//! Shared layout and typography constants for the timeline.
//!
//! The strip is one fixed-height widget across every zoom tier (no panel
//! reflow). Its vertical budget is split into named bands:
//!
//! ```text
//!   [ overview        ]   OVERVIEW_H           ← 48 h context + view box
//!   [ tick rail       ]   TICK_LANE_H
//!   [ selection strip ]   SELECTION_STRIP_H    ← where ranges are made
//!   [ main track      ]   MAIN_TRACK_H         ← scan containers + frame cells
//! ```
//!
//! Total height is `TIMELINE_TOTAL_H = OVERVIEW_H + TICK_LANE_H +
//! SELECTION_STRIP_H + MAIN_TRACK_H` and is constant across Micro / Macro
//! so the bottom panel never reflows.

use eframe::egui::FontId;

/// Height of the tick-label rail above the main track (spec §6: ~14px).
pub(crate) const TICK_LANE_H: f32 = 14.0;

/// Height of the single main track that holds scan containers and their
/// frame cells (spec §6 "full strip ~56px" = 14 tick + 42 main).
pub(crate) const MAIN_TRACK_H: f32 = 42.0;

/// The overview above the tick rail: [`OVERVIEW_PAD_TOP`], the lane itself
/// ([`OVERVIEW_LANE_H`]), and below it the funnel joining the view box to
/// the strip.
pub(crate) const OVERVIEW_H: f32 = 30.0;

/// Gap above the overview lane.
pub(crate) const OVERVIEW_PAD_TOP: f32 = 2.0;

/// Height of the overview lane (coverage, time labels and the view box).
pub(crate) const OVERVIEW_LANE_H: f32 = 20.0;

/// The selection strip between the tick rail and the main track: drag along
/// it to make a range, drag its handles to resize it.
pub(crate) const SELECTION_STRIP_H: f32 = 14.0;

/// Total timeline height, constant across detail levels.
pub(crate) const TIMELINE_TOTAL_H: f32 =
    OVERVIEW_H + TICK_LANE_H + SELECTION_STRIP_H + MAIN_TRACK_H;

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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---- block_font() / tick_font() -------------------------------------
    // Both pure constructors return a plain FontId value (no Ui/Painter),
    // so size + family are directly assertable.

    #[wasm_bindgen_test]
    fn block_font_is_monospace_size_9() {
        let f = block_font();
        // size is f32; 9.0 is exactly representable.
        assert!((f.size - 9.0).abs() < 1e-6);
        assert!(f.family == eframe::egui::FontFamily::Monospace);
    }

    #[wasm_bindgen_test]
    fn tick_font_is_monospace_size_9() {
        let f = tick_font();
        assert!((f.size - 9.0).abs() < 1e-6);
        assert!(f.family == eframe::egui::FontFamily::Monospace);
    }

    #[wasm_bindgen_test]
    fn block_and_tick_fonts_are_equal() {
        // Both currently resolve to the same monospace 9.0 spec; FontId derives
        // PartialEq so this pins that they stay in lockstep.
        assert!(block_font() == tick_font());
    }

    // ---- TIMELINE_TOTAL_H derived constant ------------------------------

    #[wasm_bindgen_test]
    fn timeline_total_h_equals_sum_of_bands() {
        // Documented invariant: total = overview + tick + selection + main.
        let expected = OVERVIEW_H + TICK_LANE_H + SELECTION_STRIP_H + MAIN_TRACK_H;
        assert!((TIMELINE_TOTAL_H - expected).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn timeline_total_h_is_one_hundred_px() {
        // Hand-computed: 30 + 14 + 14 + 42 = 100.
        assert!((TIMELINE_TOTAL_H - 100.0).abs() < 1e-6);
    }
}
