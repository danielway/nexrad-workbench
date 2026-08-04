//! Shared layout and typography constants for the timeline.
//!
//! The strip is one fixed-height widget across every zoom tier (no panel
//! reflow). Its vertical budget is split into named bands so later phases can
//! insert a minimap sliver above and loop handles below without re-deriving
//! every rect:
//!
//! ```text
//!   [ minimap sliver ]   MINIMAP_SLIVER_H  ← whole-session navigator (Phase 3)
//!   [ tick rail      ]   TICK_LANE_H
//!   [ main track     ]   MAIN_TRACK_H   ← scan containers + frame cells
//!   [ loop handles   ]   LOOP_HANDLE_H  ← draggable loop start/end (Phase 5)
//! ```
//!
//! Total height is `TIMELINE_TOTAL_H = MINIMAP_SLIVER_H + TICK_LANE_H +
//! MAIN_TRACK_H + LOOP_HANDLE_H` and is constant across Micro / Macro / Archive
//! so the bottom panel never reflows.

use eframe::egui::FontId;

/// Height of the tick-label rail above the main track (spec §6: ~14px).
pub(crate) const TICK_LANE_H: f32 = 14.0;

/// Height of the single main track that holds scan containers and their
/// frame cells (spec §6 "full strip ~56px" = 14 tick + 42 main).
pub(crate) const MAIN_TRACK_H: f32 = 42.0;

/// Whole-session minimap sliver above the tick rail (spec §5/§13): a thin
/// navigator that doubles as the Level-0 "where am I" anchor. ~6px of painted
/// content plus [`MINIMAP_PAD_Y`] above and below for a comfortable drag
/// target.
pub(crate) const MINIMAP_SLIVER_H: f32 = 10.0;

/// Vertical padding inside the minimap sliver between the hit area and the
/// painted coverage bar (so the bar reads slim while the target stays easy).
pub(crate) const MINIMAP_PAD_Y: f32 = 2.0;

/// Band below the main track that holds the draggable loop start/end handles
/// (spec §8/§12 row 6). Holds the visible handle glyphs; the *hit* targets are
/// padded up into the strip to a comfortable size ([`LOOP_HANDLE_HIT_H`] /
/// [`LOOP_HANDLE_HIT_W`]) without growing the panel.
pub(crate) const LOOP_HANDLE_H: f32 = 14.0;

/// Total height of a loop handle's hit target (spec §12: ≥44pt). The visual
/// glyph lives in the [`LOOP_HANDLE_H`] band; the hit rect extends upward into
/// the strip so the target reaches this height even though the glyph is thin.
pub(crate) const LOOP_HANDLE_HIT_H: f32 = 44.0;

/// Width of a loop handle's hit target (spec §12: ≥44pt touch target), centered
/// on the handle's x. Wider than the thin painted glyph.
pub(crate) const LOOP_HANDLE_HIT_W: f32 = 44.0;

/// Width of a handle's painted glyph (the thin draggable tab).
pub(crate) const LOOP_HANDLE_GLYPH_W: f32 = 10.0;

/// Within the snap threshold (px) of the live edge, dragging the RIGHT handle
/// snaps it to now and pins the loop (spec §8 snap-to-live fusion). A generous
/// target so the fuse-with-now-dot gesture is easy.
pub(crate) const LOOP_SNAP_LIVE_PX: f32 = 14.0;

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
        // Documented invariant: total = minimap + tick + main + loop.
        let expected = MINIMAP_SLIVER_H + TICK_LANE_H + MAIN_TRACK_H + LOOP_HANDLE_H;
        assert!((TIMELINE_TOTAL_H - expected).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn timeline_total_h_is_eighty_px() {
        // Hand-computed: 10 + 14 + 42 + 14 = 80.
        assert!((TIMELINE_TOTAL_H - 80.0).abs() < 1e-6);
    }

    // ---- loop-handle hit/touch sizing -----------------------------------

    // Spec §12: touch targets must be >= 44px in both axes, and the hit rect
    // must be wider than the thin painted glyph. These are constants, so the
    // rule is enforced at compile time — a violating edit fails the build
    // rather than a test run.
    const _: () = assert!(LOOP_HANDLE_HIT_H >= 44.0);
    const _: () = assert!(LOOP_HANDLE_HIT_W >= 44.0);
    const _: () = assert!(LOOP_HANDLE_HIT_W > LOOP_HANDLE_GLYPH_W);
}
