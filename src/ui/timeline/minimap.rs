//! Whole-session minimap sliver (spec §5, §12 row 3, §13).
//!
//! A thin strip ABOVE the main track that shows the *entire* session at a
//! fixed scale, so the user always has a "where am I" anchor regardless of how
//! far the main strip is zoomed in. Visual grammar is lifted from the mobile
//! scrubber ([`crate::ui::mobile::scrubber`]):
//!   - faint slate segments = available on the server (archive shadow)
//!   - solid steel-blue segments = cached on this device
//!   - a red marker at the right = the live edge ("now") while streaming
//!   - a translucent NEUTRAL rectangle = the main strip's visible window
//!   - a subtle neutral playhead tick = the playback position
//!
//! The accent budget (playhead / live edge / active ring) is already spent on
//! the main strip, so the minimap's window indicator stays neutral.
//!
//! Interaction (desktop only this phase):
//!   - drag anywhere = pan/fast navigation: the visible window recenters on the
//!     dragged position, clamped to the session extent ± a small margin.
//!   - click = jump the view window there. The minimap navigates the *view*; it
//!     never moves the playhead (the main strip owns the playhead seek).
//!
//! The minimap has its own response/id so its drag never fights the main
//! strip's scrub.

use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use super::style;

/// Fraction of the session span allowed as scroll margin past either end, so a
/// view window can be recentered slightly beyond the data extent (matching the
/// "± small margin" in the spec). Also used to pad a zero/degenerate extent.
const EXTENT_MARGIN_FRAC: f64 = 0.02;

/// A degenerate session (no data, no stream) falls back to this half-window
/// (seconds) centered on `now`, so the minimap never has zero width.
const FALLBACK_HALF_SPAN_SECS: f64 = 1800.0;

/// The whole-session extent the minimap maps across, in Unix seconds.
///
/// Pure value object so the coordinate mapping and the clamp math are unit
/// testable without egui.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SessionExtent {
    pub start: f64,
    pub end: f64,
}

impl SessionExtent {
    /// Union of cached time ranges, archive shadow boundaries, and — while
    /// streaming — `now` (so the live edge stays at the right). Falls back to a
    /// fixed window around `now` when there is no data at all.
    ///
    /// `cached` and `shadows` are `(start, end)` pairs in Unix seconds.
    pub(super) fn compute(
        cached: impl IntoIterator<Item = (f64, f64)>,
        shadows: impl IntoIterator<Item = (f64, f64)>,
        now: f64,
        streaming: bool,
    ) -> Self {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for (s, e) in cached.into_iter().chain(shadows) {
            lo = lo.min(s);
            hi = hi.max(e);
        }
        if streaming {
            // The live edge must always be reachable at the right.
            hi = hi.max(now);
            // Keep some history visible even before any scan lands.
            lo = lo.min(now - FALLBACK_HALF_SPAN_SECS);
        }
        if !lo.is_finite() || !hi.is_finite() || hi <= lo {
            return Self {
                start: now - FALLBACK_HALF_SPAN_SECS,
                end: now + FALLBACK_HALF_SPAN_SECS,
            };
        }
        Self { start: lo, end: hi }
    }

    /// Span in seconds (always > 0 by construction).
    pub(super) fn span(&self) -> f64 {
        (self.end - self.start).max(1.0)
    }

    /// Map a timestamp to a fraction in `0..=1` across the extent (clamped).
    pub(super) fn frac_of(&self, ts: f64) -> f64 {
        ((ts - self.start) / self.span()).clamp(0.0, 1.0)
    }

    /// Map a fraction in `0..=1` back to a timestamp.
    pub(super) fn ts_of(&self, frac: f64) -> f64 {
        self.start + frac.clamp(0.0, 1.0) * self.span()
    }

    /// Clamp a desired view *start* so the window `[start, start + win_secs)`
    /// stays within the session extent, allowing a small margin past either
    /// end. When the window is wider than the extent, it is centered on the
    /// extent instead of pinned to one side.
    pub(super) fn clamp_view_start(&self, desired_start: f64, win_secs: f64) -> f64 {
        let margin = self.span() * EXTENT_MARGIN_FRAC;
        let lo = self.start - margin;
        let hi = self.end + margin;
        if win_secs >= hi - lo {
            // Window can't fit — center it on the extent.
            (self.start + self.end) / 2.0 - win_secs / 2.0
        } else {
            desired_start.clamp(lo, hi - win_secs)
        }
    }

    /// View start that recenters the window of width `win_secs` on `center_ts`,
    /// clamped to the extent (± margin). The navigation primitive for both a
    /// minimap click (jump) and a minimap drag (pan).
    pub(super) fn recentered_view_start(&self, center_ts: f64, win_secs: f64) -> f64 {
        self.clamp_view_start(center_ts - win_secs / 2.0, win_secs)
    }
}

/// Render the minimap sliver and handle its drag/click navigation.
///
/// Drawn above the tick rail, in its own allocated rect with its own response
/// id, so it never shares a hit-test layer with the main strip. Returns
/// nothing — it only mutates the view position on `playback`.
pub(super) fn render_minimap(
    ui: &mut egui::Ui,
    state: &mut crate::state::AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
    let available_w = ui.available_width();
    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_w, style::MINIMAP_SLIVER_H),
        Sense::click_and_drag(),
    );
    let full_rect = response.rect;
    let dark = state.is_dark;

    // Content track — a thin bar with a couple px of vertical padding so the
    // hit area is comfortable while the painted bar stays slim.
    let pad_x = 4.0;
    let track_rect = Rect::from_min_max(
        Pos2::new(
            full_rect.left() + pad_x,
            full_rect.top() + style::MINIMAP_PAD_Y,
        ),
        Pos2::new(
            full_rect.right() - pad_x,
            full_rect.bottom() - style::MINIMAP_PAD_Y,
        ),
    );
    if track_rect.width() <= 0.0 {
        return;
    }

    let now = state.frame_now.secs();
    let streaming = live.mode_state.is_active();

    // Build the session extent from the same sources the strip reads, plus the
    // live edge when streaming.
    let cached: Vec<(f64, f64)> = timeline
        .scans
        .time_ranges()
        .iter()
        .map(|r| (r.start, r.end))
        .collect();
    let shadows: Vec<(f64, f64)> = timeline
        .shadow_scan_boundaries
        .iter()
        .map(|b| (b.start as f64, b.end as f64))
        .collect();
    let extent = SessionExtent::compute(
        cached.iter().copied(),
        shadows.iter().copied(),
        now,
        streaming,
    );

    let ts_to_x =
        |ts: f64| -> f32 { track_rect.left() + extent.frac_of(ts) as f32 * track_rect.width() };
    let x_to_ts = |x: f32| -> f64 {
        let frac = ((x - track_rect.left()) / track_rect.width()) as f64;
        extent.ts_of(frac)
    };

    // Background bar.
    painter.rect_filled(track_rect, 1.5, tl_colors::background(dark));

    let draw_segment = |start: f64, end: f64, color: Color32| {
        let x0 = ts_to_x(start);
        let x1 = ts_to_x(end).max(x0 + 1.0);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, track_rect.top()),
                Pos2::new(x1.min(track_rect.right()), track_rect.bottom()),
            ),
            1.0,
            color,
        );
    };

    // Available (server archive, not downloaded) — faint slate, drawn first so
    // cached paints over any overlap. Suppress shadows already covered by
    // cached data (same rule the strip and mobile scrubber use).
    {
        let view = crate::state::TimelineView::build(
            &timeline.scans,
            &timeline.shadow_scan_boundaries,
            None,
            None,
        );
        for b in view.shadow_boundaries() {
            if view.is_covered_by_cached(b.start) {
                continue;
            }
            draw_segment(
                b.start as f64,
                b.end as f64,
                tl_colors::available_border(dark),
            );
        }
    }

    // Cached (on device) — solid steel blue.
    for (s, e) in &cached {
        draw_segment(*s, *e, tl_colors::cached_fill(dark, false));
    }

    // The visible-window indicator: a translucent NEUTRAL rectangle over the
    // span the main strip currently shows. Neutral on purpose — the accent
    // budget is spent on the main strip.
    let win_start = playback.state.timeline_view_start;
    let win_secs = playback.state.view_width_secs();
    let win_end = win_start + win_secs;
    let wx0 = ts_to_x(win_start);
    let wx1 = ts_to_x(win_end).max(wx0 + 2.0);
    let window_rect = Rect::from_min_max(
        Pos2::new(wx0, track_rect.top() - 1.0),
        Pos2::new(wx1.min(track_rect.right()), track_rect.bottom() + 1.0),
    );
    painter.rect_filled(window_rect, 1.0, minimap_window_fill());
    painter.rect_stroke(
        window_rect,
        1.0,
        Stroke::new(1.0_f32, minimap_window_edge()),
        StrokeKind::Inside,
    );

    // Live-edge marker — red, only while streaming (the one accent the minimap
    // is allowed: it doubles as the right-edge "now" landmark).
    if streaming {
        let x = ts_to_x(now);
        painter.line_segment(
            [
                Pos2::new(x, full_rect.top()),
                Pos2::new(x, full_rect.bottom()),
            ],
            Stroke::new(1.5_f32, tl_colors::LIVE_ACTIVE),
        );
    }

    // Subtle playhead tick — neutral, so it reads as "position" not "now".
    let playhead_ts = playback.state.playback_position();
    if playhead_ts >= extent.start && playhead_ts <= extent.end {
        let x = ts_to_x(playhead_ts);
        painter.line_segment(
            [
                Pos2::new(x, full_rect.top() + 1.0),
                Pos2::new(x, full_rect.bottom() - 1.0),
            ],
            Stroke::new(1.0_f32, tl_colors::selection(dark)),
        );
    }

    // Hover affordance — the whole sliver is draggable.
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }

    // Interaction: drag (pan) or click (jump). Both recenter the *view* window
    // on the pointer position; neither touches the playhead. Filtered to the
    // primary button so a right-drag here doesn't navigate.
    let primary_drag = response.dragged_by(egui::PointerButton::Primary);
    let acted = (primary_drag || response.clicked())
        .then(|| response.interact_pointer_pos())
        .flatten();
    if let Some(pos) = acted {
        let center_ts = x_to_ts(pos.x);
        let new_start = extent.recentered_view_start(center_ts, win_secs);
        playback.state.timeline_view_start = new_start;
    }
}

/// Translucent neutral fill for the visible-window indicator. Light enough to
/// see the coverage segments through it.
fn minimap_window_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(200, 206, 222, 46)
}

/// Neutral outline for the visible-window indicator.
fn minimap_window_edge() -> Color32 {
    Color32::from_rgba_unmultiplied(200, 206, 222, 150)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[wasm_bindgen_test]
    fn extent_unions_cached_and_shadows() {
        let e = SessionExtent::compute(
            [(100.0, 200.0)],
            [(50.0, 80.0), (300.0, 400.0)],
            1000.0,
            false,
        );
        assert!(approx(e.start, 50.0));
        assert!(approx(e.end, 400.0));
    }

    #[wasm_bindgen_test]
    fn streaming_extends_to_now_on_the_right() {
        let e = SessionExtent::compute([(100.0, 200.0)], [], 5000.0, true);
        assert!(approx(
            e.start,
            200.0_f64.min(100.0).min(5000.0 - FALLBACK_HALF_SPAN_SECS)
        ));
        // now is later than the cached end, so the right edge is `now`.
        assert!(approx(e.end, 5000.0));
    }

    #[wasm_bindgen_test]
    fn streaming_does_not_shrink_below_cached_end() {
        // now is BEFORE the cached end (clock skew / replay) — the extent must
        // still cover the cached data.
        let e = SessionExtent::compute([(100.0, 9000.0)], [], 5000.0, true);
        assert!(approx(e.end, 9000.0));
    }

    #[wasm_bindgen_test]
    fn empty_session_falls_back_to_window_around_now() {
        let e = SessionExtent::compute([], [], 1000.0, false);
        assert!(approx(e.start, 1000.0 - FALLBACK_HALF_SPAN_SECS));
        assert!(approx(e.end, 1000.0 + FALLBACK_HALF_SPAN_SECS));
    }

    #[wasm_bindgen_test]
    fn frac_and_ts_roundtrip() {
        let e = SessionExtent {
            start: 1000.0,
            end: 2000.0,
        };
        assert!(approx(e.frac_of(1000.0), 0.0));
        assert!(approx(e.frac_of(1500.0), 0.5));
        assert!(approx(e.frac_of(2000.0), 1.0));
        // Out of range clamps.
        assert!(approx(e.frac_of(500.0), 0.0));
        assert!(approx(e.frac_of(2500.0), 1.0));
        assert!(approx(e.ts_of(0.25), 1250.0));
        assert!(approx(e.ts_of(e.frac_of(1750.0)), 1750.0));
    }

    #[wasm_bindgen_test]
    fn clamp_keeps_window_inside_extent_with_margin() {
        let e = SessionExtent {
            start: 0.0,
            end: 1000.0,
        };
        let win = 200.0;
        let margin = 1000.0 * EXTENT_MARGIN_FRAC;
        // Far-left desired start clamps to lo.
        assert!(approx(e.clamp_view_start(-9999.0, win), -margin));
        // Far-right desired start clamps so the window end reaches hi.
        assert!(approx(
            e.clamp_view_start(9999.0, win),
            1000.0 + margin - win
        ));
        // A start already inside is untouched.
        assert!(approx(e.clamp_view_start(400.0, win), 400.0));
    }

    #[wasm_bindgen_test]
    fn clamp_centers_when_window_wider_than_extent() {
        let e = SessionExtent {
            start: 0.0,
            end: 1000.0,
        };
        // Window wider than extent+margins → centered on the extent.
        let win = 5000.0;
        let start = e.clamp_view_start(123.0, win);
        assert!(approx(start, 500.0 - win / 2.0));
    }

    #[wasm_bindgen_test]
    fn recenter_centers_window_on_target_when_room() {
        let e = SessionExtent {
            start: 0.0,
            end: 10_000.0,
        };
        let win = 1000.0;
        // Centering on 5000 leaves room on both sides → exact center.
        assert!(approx(
            e.recentered_view_start(5000.0, win),
            5000.0 - win / 2.0
        ));
    }

    #[wasm_bindgen_test]
    fn recenter_clamps_near_the_edges() {
        let e = SessionExtent {
            start: 0.0,
            end: 10_000.0,
        };
        let win = 1000.0;
        let margin = 10_000.0 * EXTENT_MARGIN_FRAC;
        // Centering at the far left can't go below lo.
        assert!(approx(e.recentered_view_start(-500.0, win), -margin));
        // Centering at the far right can't push the window end past hi.
        assert!(approx(
            e.recentered_view_start(10_500.0, win),
            10_000.0 + margin - win
        ));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[wasm_bindgen_test]
    fn span_is_end_minus_start() {
        let e = SessionExtent {
            start: 1000.0,
            end: 3500.0,
        };
        assert!(close(e.span(), 2500.0));
    }

    #[wasm_bindgen_test]
    fn span_floors_at_one_for_degenerate_extent() {
        // start == end → raw span 0, floored to 1.0 so mapping never divides by 0.
        let e = SessionExtent {
            start: 500.0,
            end: 500.0,
        };
        assert!(close(e.span(), 1.0));
        // A negative (inverted) raw span also floors to 1.0.
        let inverted = SessionExtent {
            start: 800.0,
            end: 600.0,
        };
        assert!(close(inverted.span(), 1.0));
    }

    #[wasm_bindgen_test]
    fn frac_of_on_degenerate_extent_is_finite_and_clamped() {
        // span() floors to 1.0, so frac_of stays finite (no div-by-zero / NaN).
        let e = SessionExtent {
            start: 500.0,
            end: 500.0,
        };
        let f = e.frac_of(500.0);
        assert!(f.is_finite());
        // 0 / 1 = 0, clamped within 0..=1.
        assert!(close(f, 0.0));
        // A timestamp one second past start maps to fraction 1.0 (1/1, clamped).
        assert!(close(e.frac_of(501.0), 1.0));
        // Anything below start clamps to 0.
        assert!(close(e.frac_of(400.0), 0.0));
    }

    #[wasm_bindgen_test]
    fn compute_streaming_only_no_data_uses_now_and_history() {
        // No cached/shadow ranges, but streaming → extent spans
        // [now - FALLBACK_HALF_SPAN_SECS, now], not the symmetric fallback window.
        let now = 10_000.0;
        let e = SessionExtent::compute([], [], now, true);
        assert!(close(e.start, now - FALLBACK_HALF_SPAN_SECS));
        assert!(close(e.end, now));
    }

    #[wasm_bindgen_test]
    fn compute_inverted_single_pair_falls_back_to_window_around_now() {
        // Union of an inverted (end < start) pair yields hi <= lo, which is
        // treated as degenerate → symmetric fallback window around `now`.
        let now = 2000.0;
        let e = SessionExtent::compute([(900.0, 100.0)], [], now, false);
        assert!(close(e.start, now - FALLBACK_HALF_SPAN_SECS));
        assert!(close(e.end, now + FALLBACK_HALF_SPAN_SECS));
    }

    #[wasm_bindgen_test]
    fn compute_shadows_only_unions_their_bounds() {
        // Only archive-shadow ranges, not streaming → extent is their union.
        let e = SessionExtent::compute([], [(300.0, 350.0), (100.0, 120.0)], 9999.0, false);
        assert!(close(e.start, 100.0));
        assert!(close(e.end, 350.0));
    }

    #[wasm_bindgen_test]
    fn ts_of_clamps_fraction_argument() {
        let e = SessionExtent {
            start: 1000.0,
            end: 2000.0,
        };
        // Out-of-range fractions clamp before mapping.
        assert!(close(e.ts_of(-1.0), 1000.0));
        assert!(close(e.ts_of(2.0), 2000.0));
    }

    #[wasm_bindgen_test]
    fn window_indicator_colors_are_translucent_and_distinct() {
        // Only the alpha channel survives from_rgba_unmultiplied unchanged
        // (r/g/b are premultiplied), so assert alpha exactly.
        let fill = minimap_window_fill();
        let edge = minimap_window_edge();
        assert!(fill.a() == 46);
        assert!(edge.a() == 150);
        // Both are translucent (not fully opaque) and the edge is more opaque
        // than the fill so the outline reads over the body.
        assert!(fill.a() < 255);
        assert!(edge.a() < 255);
        assert!(edge.a() > fill.a());
    }
}
