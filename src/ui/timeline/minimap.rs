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
//!   - **drag the window indicator** = pan, relative to where you grabbed it.
//!     This is the primary pan surface, so the grab must not teleport: a thumb
//!     drag preserves the offset between the pointer and the window's left
//!     edge for the drag's lifetime.
//!   - **click/drag outside the indicator** = jump: the window recenters on
//!     the pointer, which is the fast way to cross a long session.
//!   - The minimap navigates the *view*; it never moves the playhead (the main
//!     strip owns the playhead seek).
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

/// How many times the main strip's visible span the minimap may show.
///
/// The minimap used to map the union of *everything* touched this session, so a
/// single scan from a week ago flattened the whole sliver and the window
/// indicator became a hairline — useless at exactly the zoom levels it exists
/// to serve. Bounding the extent to a multiple of the visible span keeps the
/// indicator a readable fraction (1/8) of the track at every zoom, while still
/// showing real surrounding context. When the session is smaller than this, the
/// whole session is shown as before.
const CONTEXT_MULTIPLE: f64 = 8.0;

/// Minimum grab width (px) for the window indicator. At wide zooms the painted
/// indicator is only a few pixels; the hit target is widened to a comfortable
/// size so the thumb stays draggable without making the paint heavier.
const MIN_THUMB_GRAB_PX: f32 = 12.0;

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
    /// The span the minimap maps across: everything worth showing (cached
    /// ranges, archive shadows, and while streaming `now`), **bounded to
    /// [`CONTEXT_MULTIPLE`] times the main strip's visible span, centered on
    /// the view**.
    ///
    /// The bound is what makes the sliver useful at every zoom. Without it the
    /// extent is the whole session, so one week-old scan squashes an hour-wide
    /// view into a hairline. With it, the window indicator is always at least
    /// 1/`CONTEXT_MULTIPLE` of the track — and when the session is smaller than
    /// the context window, the whole session still shows, exactly as before.
    ///
    /// The result always fully contains `[view_start, view_start + view_span]`,
    /// so the indicator can never clip off an edge.
    ///
    /// `cached` and `shadows` are `(start, end)` pairs in Unix seconds.
    pub(super) fn compute(
        cached: impl IntoIterator<Item = (f64, f64)>,
        shadows: impl IntoIterator<Item = (f64, f64)>,
        now: f64,
        streaming: bool,
        view_start: f64,
        view_span: f64,
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
            lo = now - FALLBACK_HALF_SPAN_SECS;
            hi = now + FALLBACK_HALF_SPAN_SECS;
        }

        // A degenerate view span (before the first layout pass) leaves the
        // data extent alone — there is no view to be relative to yet.
        if !view_span.is_finite() || view_span <= 0.0 || !view_start.is_finite() {
            return Self { start: lo, end: hi };
        }

        // Everything that could be worth showing: the data plus the view
        // itself (the view can sit outside the data after a long pan).
        let view_end = view_start + view_span;
        let full_lo = lo.min(view_start);
        let full_hi = hi.max(view_end);

        // …bounded to a context window centered on the view. Both bounds sit
        // outside the view by construction (half the context is 4x the view
        // span), so the window indicator always fits.
        let half_context = view_span * CONTEXT_MULTIPLE / 2.0;
        let center = view_start + view_span / 2.0;
        Self {
            start: full_lo.max(center - half_context),
            end: full_hi.min(center + half_context),
        }
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
    /// clamped to the extent (± margin). The navigation primitive for a minimap
    /// click or drag that did NOT start on the window indicator — a deliberate
    /// jump across the session.
    pub(super) fn recentered_view_start(&self, center_ts: f64, win_secs: f64) -> f64 {
        self.clamp_view_start(center_ts - win_secs / 2.0, win_secs)
    }

    /// View start while dragging the window indicator itself.
    ///
    /// `grab_offset_secs` is how far into the window the pointer landed when
    /// the drag began; holding it constant is what makes the thumb track the
    /// pointer instead of teleporting so its center jumps under the cursor.
    /// This is the difference between a scroll thumb and a "click to recenter"
    /// target, and it is why grabbing the indicator used to feel broken.
    pub(super) fn thumb_drag_view_start(
        &self,
        pointer_ts: f64,
        grab_offset_secs: f64,
        win_secs: f64,
    ) -> f64 {
        self.clamp_view_start(pointer_ts - grab_offset_secs, win_secs)
    }
}

/// Horizontal hit span (screen x) for the window indicator, widened to at least
/// [`MIN_THUMB_GRAB_PX`] around its center so a thin indicator is still
/// grabbable. Returned as `(left, right)`.
///
/// Pure so the "is the thumb reachable at this zoom" rule is testable without a
/// painter.
pub(super) fn thumb_hit_span(painted_left: f32, painted_right: f32) -> (f32, f32) {
    let width = painted_right - painted_left;
    if width >= MIN_THUMB_GRAB_PX {
        return (painted_left, painted_right);
    }
    let center = (painted_left + painted_right) / 2.0;
    let half = MIN_THUMB_GRAB_PX / 2.0;
    (center - half, center + half)
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
    let win_start = playback.state.timeline_view_start;
    let win_secs = playback.state.view_width_secs();
    let extent = SessionExtent::compute(
        cached.iter().copied(),
        shadows.iter().copied(),
        now,
        streaming,
        win_start,
        win_secs,
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
        let view = crate::core::TimelineView::build(
            &timeline.scans,
            &timeline.shadow_scan_boundaries,
            None,
            None,
        );
        for b in view.shadow_boundaries() {
            if view.is_covered_by_cached(b.start) {
                continue;
            }
            // The faint interior wash, matching the main strip's Available
            // cells. This used to use `available_border` (alpha 120, ~3x the
            // fill) as a solid fill, which — given how close the available and
            // cached hues are — left cached data barely distinguishable from
            // the archive shadow behind it.
            draw_segment(
                b.start as f64,
                b.end as f64,
                tl_colors::available_fill(dark),
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

    // Hover affordance — the whole sliver is draggable, but grabbing the
    // indicator is a different gesture from jumping, so say so.
    let (grab_x0, grab_x1) = thumb_hit_span(window_rect.left(), window_rect.right());
    let over_thumb = response
        .hover_pos()
        .is_some_and(|p| p.x >= grab_x0 && p.x <= grab_x1);
    if response.hovered() {
        ui.ctx().set_cursor_icon(if over_thumb {
            egui::CursorIcon::Grab
        } else {
            egui::CursorIcon::ResizeHorizontal
        });
    }

    // Interaction: grab-and-drag the window indicator (relative pan), or
    // click/drag elsewhere to jump (absolute recenter). Neither touches the
    // playhead. Filtered to the primary button so a right-drag here doesn't
    // navigate.
    //
    // The grab offset must persist for the drag's lifetime — recomputing it
    // per frame would make the thumb snap to the pointer, which is the
    // teleport this replaces. `None` in memory means "this drag is a jump".
    let grab_id = response.id.with("thumb_grab_secs");
    if response.drag_started_by(egui::PointerButton::Primary) {
        let grab = response
            .interact_pointer_pos()
            .filter(|p| p.x >= grab_x0 && p.x <= grab_x1)
            .map(|p| x_to_ts(p.x) - win_start);
        ui.ctx().memory_mut(|m| m.data.insert_temp(grab_id, grab));
    }
    if response.drag_stopped() {
        ui.ctx()
            .memory_mut(|m| m.data.remove::<Option<f64>>(grab_id));
    }
    let grab_offset_secs = ui
        .ctx()
        .memory(|m| m.data.get_temp::<Option<f64>>(grab_id))
        .flatten();

    let primary_drag = response.dragged_by(egui::PointerButton::Primary);
    let acted = (primary_drag || response.clicked())
        .then(|| response.interact_pointer_pos())
        .flatten();
    if let Some(pos) = acted {
        let pointer_ts = x_to_ts(pos.x);
        let new_start = match grab_offset_secs {
            Some(offset) => extent.thumb_drag_view_start(pointer_ts, offset, win_secs),
            None => extent.recentered_view_start(pointer_ts, win_secs),
        };
        // Navigating here is a deliberate pan: release the live view-follow
        // nudge so it can't snap the view back to now on the next frame.
        playback.state.view_follows_now = false;
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

    /// The data union with no view constraint applied — a degenerate view span
    /// short-circuits the context bound, so this isolates the union logic.
    fn union_of(
        cached: impl IntoIterator<Item = (f64, f64)>,
        shadows: impl IntoIterator<Item = (f64, f64)>,
        now: f64,
        streaming: bool,
    ) -> SessionExtent {
        SessionExtent::compute(cached, shadows, now, streaming, 0.0, 0.0)
    }

    #[wasm_bindgen_test]
    fn extent_unions_cached_and_shadows() {
        let e = union_of(
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
        let e = union_of([(100.0, 200.0)], [], 5000.0, true);
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
        let e = union_of([(100.0, 9000.0)], [], 5000.0, true);
        assert!(approx(e.end, 9000.0));
    }

    #[wasm_bindgen_test]
    fn empty_session_falls_back_to_window_around_now() {
        let e = union_of([], [], 1000.0, false);
        assert!(approx(e.start, 1000.0 - FALLBACK_HALF_SPAN_SECS));
        assert!(approx(e.end, 1000.0 + FALLBACK_HALF_SPAN_SECS));
    }

    // ---- the context bound (the fix for "uselessly zoomed-out") -----------

    #[wasm_bindgen_test]
    fn a_distant_stale_range_no_longer_flattens_the_sliver() {
        // The audit case: an hour-wide view, plus one cached range from a week
        // ago. Unbounded, the extent spans a week and the window indicator is a
        // 0.6% hairline. Bounded, the stale range is clipped out entirely and
        // the indicator stays readable.
        let view_start = 1_000_000.0;
        let view_span = 3600.0;
        let week_ago = view_start - 7.0 * 86_400.0;
        let e = SessionExtent::compute(
            [(week_ago, week_ago + 300.0)],
            [],
            view_start,
            false,
            view_start,
            view_span,
        );
        // The week-old range is outside the context window, so it is clipped.
        assert!(e.start > week_ago);
        // Never wider than the context bound…
        assert!(e.span() <= view_span * CONTEXT_MULTIPLE + 1e-6);
        // …so the indicator is at least 1/CONTEXT_MULTIPLE of the track.
        assert!(view_span / e.span() >= 1.0 / CONTEXT_MULTIPLE - 1e-9);
        // Unbounded, it would have been under 1%.
        assert!(view_span / (view_start + view_span - week_ago) < 0.01);
    }

    #[wasm_bindgen_test]
    fn a_session_smaller_than_the_context_window_is_shown_whole() {
        // Nothing is clipped when the data already fits: this preserves the
        // original whole-session behavior for short sessions.
        let view_start = 1_000_000.0;
        let view_span = 3600.0;
        let e = SessionExtent::compute(
            [(view_start - 600.0, view_start + view_span + 600.0)],
            [],
            view_start,
            false,
            view_start,
            view_span,
        );
        assert!(approx(e.start, view_start - 600.0));
        assert!(approx(e.end, view_start + view_span + 600.0));
    }

    #[wasm_bindgen_test]
    fn the_extent_always_contains_the_view_window() {
        // The window indicator must never clip off an edge, whatever the data
        // looks like — including when the view has been panned far away from
        // every cached range.
        let view_span = 3600.0;
        for view_start in [1_000_000.0_f64, -5_000_000.0, 9_000_000.0] {
            let e = SessionExtent::compute(
                [(0.0, 1000.0)],
                [(2_000_000.0, 2_000_100.0)],
                500_000.0,
                true,
                view_start,
                view_span,
            );
            assert!(e.start <= view_start);
            assert!(e.end >= view_start + view_span);
        }
    }

    #[wasm_bindgen_test]
    fn one_sided_data_clips_to_context_but_hugs_the_view_on_the_empty_side() {
        // Data lies entirely to the LEFT of the view. Scrollbar semantics: the
        // far side of the data is clipped to the context bound, while the empty
        // side stops at the view — so a thumb flush against an edge honestly
        // means "you are at the edge of the content".
        let view_start = 1_000_000.0;
        let view_span = 3600.0;
        let center = view_start + view_span / 2.0;
        let e = SessionExtent::compute(
            [(view_start - 100_000.0, view_start - 90_000.0)],
            [],
            view_start,
            false,
            view_start,
            view_span,
        );
        assert!(approx(e.start, center - view_span * CONTEXT_MULTIPLE / 2.0));
        assert!(approx(e.end, view_start + view_span));
    }

    // ---- thumb drag --------------------------------------------------------

    #[wasm_bindgen_test]
    fn thumb_drag_preserves_the_grab_offset() {
        // Grabbing the thumb 1/4 in and moving the pointer must move the view
        // by exactly the pointer delta — no teleport that recenters under the
        // cursor.
        let e = SessionExtent {
            start: 0.0,
            end: 100_000.0,
        };
        let win = 1000.0;
        let grab_offset = win * 0.25;
        let start_a = e.thumb_drag_view_start(50_000.0, grab_offset, win);
        let start_b = e.thumb_drag_view_start(53_000.0, grab_offset, win);
        assert!(approx(start_a, 50_000.0 - grab_offset));
        assert!(approx(start_b - start_a, 3000.0));
    }

    #[wasm_bindgen_test]
    fn thumb_drag_with_zero_offset_puts_the_left_edge_under_the_pointer() {
        let e = SessionExtent {
            start: 0.0,
            end: 100_000.0,
        };
        assert!(approx(
            e.thumb_drag_view_start(42_000.0, 0.0, 1000.0),
            42_000.0
        ));
    }

    #[wasm_bindgen_test]
    fn thumb_drag_is_clamped_to_the_extent() {
        let e = SessionExtent {
            start: 0.0,
            end: 10_000.0,
        };
        let win = 1000.0;
        let margin = 10_000.0 * EXTENT_MARGIN_FRAC;
        assert!(approx(e.thumb_drag_view_start(-9e9, 0.0, win), -margin));
        assert!(approx(
            e.thumb_drag_view_start(9e9, 0.0, win),
            10_000.0 + margin - win
        ));
    }

    #[wasm_bindgen_test]
    fn thin_thumbs_get_a_minimum_grab_width() {
        // A 2px indicator is unhittable; widen symmetrically about its center.
        let (lo, hi) = thumb_hit_span(100.0, 102.0);
        assert!((hi - lo - MIN_THUMB_GRAB_PX).abs() < 1e-4);
        assert!(((lo + hi) / 2.0 - 101.0).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn wide_thumbs_keep_their_painted_span() {
        // Already comfortable — do not inflate, or clicks just outside the
        // indicator would wrongly read as grabs.
        let (lo, hi) = thumb_hit_span(100.0, 160.0);
        assert!((lo - 100.0).abs() < 1e-4);
        assert!((hi - 160.0).abs() < 1e-4);
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
        let e = SessionExtent::compute([], [], now, true, 0.0, 0.0);
        assert!(close(e.start, now - FALLBACK_HALF_SPAN_SECS));
        assert!(close(e.end, now));
    }

    #[wasm_bindgen_test]
    fn compute_inverted_single_pair_falls_back_to_window_around_now() {
        // Union of an inverted (end < start) pair yields hi <= lo, which is
        // treated as degenerate → symmetric fallback window around `now`.
        let now = 2000.0;
        let e = SessionExtent::compute([(900.0, 100.0)], [], now, false, 0.0, 0.0);
        assert!(close(e.start, now - FALLBACK_HALF_SPAN_SECS));
        assert!(close(e.end, now + FALLBACK_HALF_SPAN_SECS));
    }

    #[wasm_bindgen_test]
    fn compute_shadows_only_unions_their_bounds() {
        // Only archive-shadow ranges, not streaming → extent is their union.
        let e = SessionExtent::compute(
            [],
            [(300.0, 350.0), (100.0, 120.0)],
            9999.0,
            false,
            0.0,
            0.0,
        );
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
