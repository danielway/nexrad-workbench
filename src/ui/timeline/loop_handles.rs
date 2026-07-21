//! Draggable loop handles hanging below the main strip (spec §8, §12 row 6).
//!
//! When a loop exists, two handles (loop start / loop end) sit at the selection
//! edges in the reserved [`super::style::LOOP_HANDLE_H`] band below the strip.
//! Dragging a handle adjusts that bound. The band is its OWN allocated widget
//! (a distinct response id) so its hit layer never shares the strip's
//! press-seek/scrub — the strip's seek additionally ignores presses inside the
//! handle hit rects (threaded through `suppress_rects`), so the two never fight.
//!
//! Hit targets are padded to ≥44pt ([`LOOP_HANDLE_HIT_W`] × [`LOOP_HANDLE_HIT_H`])
//! by extending the interact rect upward into the strip, even though the painted
//! glyph is thin.
//!
//! Snap-to-live fusion (spec §8): dragging the RIGHT handle within
//! [`LOOP_SNAP_LIVE_PX`] of the live edge snaps it to now and *pins* the loop
//! (the window then slides forward as sweeps arrive). The handle fuses with the
//! now dot (shared red treatment). Dragging it back off now un-pins the loop
//! (it becomes a fixed range).

use super::style::{LOOP_HANDLE_GLYPH_W, LOOP_HANDLE_HIT_H, LOOP_HANDLE_HIT_W, LOOP_SNAP_LIVE_PX};
use crate::state::AppState;
use crate::ui::colors::timeline::{self as tl_colors, LIVE_ACTIVE};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind};

/// The two loop-handle hit rects (start, end) for the current selection, or
/// `None` when no loop exists or it's fully off-screen. Computed from the strip
/// geometry alone (no `&mut`), so the strip-interaction layer can take these as
/// `suppress_rects` BEFORE the band widget is allocated.
pub(super) fn handle_hit_rects(
    frame: &super::TimelineFrame<'_>,
    range: Option<(f64, f64)>,
    band_bottom: f32,
) -> Option<[Rect; 2]> {
    let (start, end) = range?;
    let overlay = &frame.rects.overlay;
    let start_x = frame.ts_to_x(start);
    let end_x = frame.ts_to_x(end);
    // Reject when both edges are off-screen (nothing draggable in view).
    if end_x < overlay.left() || start_x > overlay.right() {
        return None;
    }
    Some([hit_rect(start_x, band_bottom), hit_rect(end_x, band_bottom)])
}

/// Pure snap-to-live decision (spec §8): would dragging the right handle to
/// screen-x `handle_x` snap it to the live edge at `now_x`? True only while
/// `streaming` and within [`LOOP_SNAP_LIVE_PX`] of the now-line. Extracted so
/// the threshold logic is unit-testable without an egui context.
pub(super) fn snaps_to_live(handle_x: f32, now_x: f32, streaming: bool) -> bool {
    streaming && (handle_x - now_x).abs() <= LOOP_SNAP_LIVE_PX
}

/// A single handle's hit rect: [`LOOP_HANDLE_HIT_W`] wide, centered on `x`, and
/// [`LOOP_HANDLE_HIT_H`] tall, extending upward from `band_bottom` into the
/// strip so the target reaches ≥44pt without growing the panel.
fn hit_rect(x: f32, band_bottom: f32) -> Rect {
    Rect::from_min_max(
        Pos2::new(x - LOOP_HANDLE_HIT_W / 2.0, band_bottom - LOOP_HANDLE_HIT_H),
        Pos2::new(x + LOOP_HANDLE_HIT_W / 2.0, band_bottom),
    )
}

/// Render + handle the draggable loop handles in `band_rect` (its own widget).
/// Must run AFTER the strip so its interact rects win for the handle positions
/// (the strip also suppresses seeks there). No-op when no loop exists.
pub(super) fn render_loop_handles(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    frame: &super::TimelineFrame<'_>,
    band_rect: Rect,
) {
    let Some((start, end)) = playback.state.selection_range() else {
        return;
    };
    let overlay = &frame.rects.overlay;
    let now_x = frame.ts_to_x(frame.now_secs);
    let pinned = playback.state.loop_window.is_some_and(|w| w.pinned);

    // The painter for the band (and the thin connector lines up into the strip).
    let painter = ui.painter_at(Rect::from_min_max(
        Pos2::new(band_rect.left(), overlay.top()),
        band_rect.max,
    ));

    // -- Handle drag interaction (two separate interact rects) ----------------
    // start edge = index 0, end edge = index 1.
    let start_x = frame.ts_to_x(start);
    let end_x = frame.ts_to_x(end);
    let band_bottom = band_rect.bottom();

    let mut new_start = start;
    let mut new_end = end;
    let mut snapped_end_to_live = false;
    let mut dragged_end_off_live = false;

    for (idx, edge_x) in [(0usize, start_x), (1usize, end_x)] {
        let rect = hit_rect(edge_x, band_bottom);
        // Only interact with handles whose x is on-screen.
        if edge_x < overlay.left() - 2.0 || edge_x > overlay.right() + 2.0 {
            continue;
        }
        let id = ui.id().with(("loop_handle", idx));
        let resp = ui.interact(rect, id, Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let ts = frame.x_to_ts(pos.x);
                if idx == 0 {
                    new_start = ts;
                } else {
                    new_end = ts;
                    // Snap-to-live fusion: within the threshold of the now-line,
                    // snap the right handle to now and remember to pin.
                    if snaps_to_live(pos.x, now_x, live.mode_state.is_active()) {
                        new_end = frame.now_secs;
                        snapped_end_to_live = true;
                    } else if pinned {
                        // Dragged the end away from now while pinned → un-pin.
                        dragged_end_off_live = true;
                    }
                }
            }
        }
    }

    let edited = new_start != start || new_end != end;
    if edited {
        apply_handle_edit(
            state,
            live,
            playback,
            new_start,
            new_end,
            snapped_end_to_live,
            dragged_end_off_live,
        );
    }

    // -- Paint: a connector line per edge up into the strip + the band glyph --
    // Re-read after the edit so the paint reflects this frame's bounds.
    if let Some((s, e)) = playback.state.selection_range() {
        let pinned_now = playback.state.loop_window.is_some_and(|w| w.pinned);
        let sx = frame.ts_to_x(s);
        let ex = frame.ts_to_x(e);
        // The end handle fuses with the now dot when pinned and at the live edge.
        let end_is_fused = pinned_now && (ex - now_x).abs() <= 1.5;

        paint_handle(&painter, sx, overlay, band_rect, false, false);
        paint_handle(&painter, ex, overlay, band_rect, pinned_now, end_is_fused);
    }
}

/// Write a handle edit through the selection/anchor API (never direct field
/// writes). Dragging pauses the pinned slide by converting to a fixed range,
/// except when the right handle snapped to live (which (re)pins).
fn apply_handle_edit(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    new_start: f64,
    new_end: f64,
    snapped_end_to_live: bool,
    dragged_end_off_live: bool,
) {
    // Editing a handle is a Free-mode range edit: if a pinned *replay* is
    // running (LookbackLoop), drop to a fixed selection first so the drag isn't
    // clobbered by tick_live. The stream keeps running.
    if playback.state.time_model.is_lookback() {
        live.detach_playhead(
            &mut playback.state,
            state.frame_now.secs(),
            state.pause_stream_while_reviewing,
        );
    }
    playback.state.set_selection(new_start, new_end);
    if snapped_end_to_live {
        playback.state.anchor_selection_to_live();
    } else if dragged_end_off_live {
        playback.state.unanchor_selection_from_live();
    }
    playback.state.apply_selection_as_bounds();
    if let Some(range) = playback.state.selection_range() {
        state.selection_just_finalized = Some(range);
    }
}

/// Paint one handle: a thin vertical connector from the strip's selection band
/// down through the loop-handle band, plus a small draggable tab glyph. A pinned
/// end handle uses the live red treatment and, when fused with the now dot, gets
/// a small merged cap.
fn paint_handle(
    painter: &egui::Painter,
    x: f32,
    overlay: &Rect,
    band_rect: Rect,
    pinned: bool,
    fused: bool,
) {
    if x < overlay.left() - 1.0 || x > overlay.right() + 1.0 {
        return;
    }
    let color = if pinned {
        LIVE_ACTIVE
    } else {
        tl_colors::selection_edge()
    };

    // Connector from the bottom of the strip overlay through the band.
    painter.line_segment(
        [
            Pos2::new(x, overlay.bottom() - 1.0),
            Pos2::new(x, band_rect.bottom() - 2.0),
        ],
        Stroke::new(1.5_f32, color),
    );

    // The tab glyph: a small rounded rect centered on x within the band.
    let glyph = Rect::from_min_max(
        Pos2::new(x - LOOP_HANDLE_GLYPH_W / 2.0, band_rect.top() + 1.0),
        Pos2::new(x + LOOP_HANDLE_GLYPH_W / 2.0, band_rect.bottom() - 1.0),
    );
    painter.rect_filled(glyph, 2.0, fill_for(color, fused));
    painter.rect_stroke(glyph, 2.0, Stroke::new(1.0_f32, color), StrokeKind::Inside);
}

/// Tab fill: the edge color washed down, or a fuller live red when fused with
/// the now dot.
fn fill_for(color: Color32, fused: bool) -> Color32 {
    if fused {
        LIVE_ACTIVE
    } else {
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 150)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn snap_to_live_only_within_threshold_while_streaming() {
        let now_x = 500.0;
        // Exactly on the now-line and within the threshold → snaps.
        assert!(snaps_to_live(now_x, now_x, true));
        assert!(snaps_to_live(now_x + LOOP_SNAP_LIVE_PX, now_x, true));
        assert!(snaps_to_live(now_x - LOOP_SNAP_LIVE_PX, now_x, true));
        // Just past the threshold → no snap.
        assert!(!snaps_to_live(now_x + LOOP_SNAP_LIVE_PX + 0.1, now_x, true));
        // Never snaps when not streaming (no live edge to fuse with).
        assert!(!snaps_to_live(now_x, now_x, false));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── snaps_to_live: complementary cases the existing `tests` mod omits ──

    #[wasm_bindgen_test]
    fn snap_negative_side_just_past_threshold_does_not_snap() {
        let now_x = 300.0;
        // Mirror of the existing +epsilon case on the LEFT side of the now-line.
        assert!(!snaps_to_live(now_x - LOOP_SNAP_LIVE_PX - 0.1, now_x, true));
    }

    #[wasm_bindgen_test]
    fn snap_far_away_while_streaming_does_not_snap() {
        let now_x = 300.0;
        // Well outside the threshold (both directions) → never snaps.
        assert!(!snaps_to_live(now_x + 1000.0, now_x, true));
        assert!(!snaps_to_live(now_x - 1000.0, now_x, true));
    }

    #[wasm_bindgen_test]
    fn snap_within_threshold_but_not_streaming_never_snaps() {
        let now_x = 300.0;
        // Even a value that WOULD snap while streaming must not snap when idle.
        assert!(snaps_to_live(now_x + 5.0, now_x, true));
        assert!(!snaps_to_live(now_x + 5.0, now_x, false));
    }

    // ── hit_rect: 44×44 target centered on x, extending UP from band_bottom ──

    #[wasm_bindgen_test]
    fn hit_rect_is_full_touch_target_size() {
        let r = hit_rect(100.0, 200.0);
        // LOOP_HANDLE_HIT_W × LOOP_HANDLE_HIT_H = 44 × 44.
        assert!((r.width() - LOOP_HANDLE_HIT_W).abs() < 1e-4);
        assert!((r.height() - LOOP_HANDLE_HIT_H).abs() < 1e-4);
        assert!((r.width() - 44.0).abs() < 1e-4);
        assert!((r.height() - 44.0).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn hit_rect_centered_on_x_and_anchored_to_band_bottom() {
        let x = 100.0_f32;
        let band_bottom = 200.0_f32;
        let r = hit_rect(x, band_bottom);
        // Horizontally centered on x: left = x - 22, right = x + 22.
        assert!((r.left() - (x - LOOP_HANDLE_HIT_W / 2.0)).abs() < 1e-4);
        assert!((r.right() - (x + LOOP_HANDLE_HIT_W / 2.0)).abs() < 1e-4);
        assert!((r.left() - 78.0).abs() < 1e-4);
        assert!((r.right() - 122.0).abs() < 1e-4);
        // Bottom sits at band_bottom; top extends UP into the strip by HIT_H.
        assert!((r.bottom() - band_bottom).abs() < 1e-4);
        assert!((r.top() - (band_bottom - LOOP_HANDLE_HIT_H)).abs() < 1e-4);
        assert!((r.top() - 156.0).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn hit_rect_center_tracks_x() {
        // The rect's center x equals the requested x (within rounding).
        let r = hit_rect(37.5, 80.0);
        assert!((r.center().x - 37.5).abs() < 1e-4);
    }

    // ── fill_for: fused → live red; otherwise the edge color at alpha 150 ──

    #[wasm_bindgen_test]
    fn fill_for_fused_is_live_active_red() {
        // When fused with the now dot, the tab fills with the bright live red,
        // independent of the passed-in edge color. LIVE_ACTIVE is fully opaque,
        // so all channels round-trip exactly through premultiplication.
        let edge = Color32::from_rgb(180, 188, 210);
        let f = fill_for(edge, true);
        assert!(f.to_array() == [255, 80, 80, 255]);
        assert!(f == LIVE_ACTIVE);
    }

    #[wasm_bindgen_test]
    fn fill_for_unfused_washes_edge_color_to_alpha_150() {
        // Not fused → from_rgba_unmultiplied(r, g, b, 150). Alpha is stored
        // verbatim; rgb is premultiplied so only assert alpha exactly.
        let edge = Color32::from_rgb(180, 188, 210);
        let f = fill_for(edge, false);
        assert!(f.a() == 150);
        // It must NOT be the opaque live red used for the fused case.
        assert!(f.a() != LIVE_ACTIVE.a());
    }
}
