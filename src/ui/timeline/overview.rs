//! The overview lane above the strip (round-4 design): 48 hours of context
//! with a box marking what the strip below shows.
//!
//!   - faint slate = available on the server; steel blue = on this device
//!   - an amber line along the top = the selected range
//!   - a red dot = now, while streaming
//!   - a neutral line = the playhead, when it isn't at now
//!   - the view box, joined to the strip below by a faint funnel
//!
//! Gestures: drag the box to move the view, drag its sides to zoom the view,
//! drag the rest of the lane to slide the overview, click the lane to look
//! there, scroll sideways to slide the overview. None of them touch playback.
//! When the box is out of sight, a chip at the lane's edge brings it back.
//!
//! A thin projection of [`crate::core::overview`]: the window, the grabs and
//! the resize are decided there. Desktop only; the mobile scrubber plays this
//! role on phones.

use crate::core::overview::{resized_view, Overview, OverviewGrab};
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use super::style;

/// Horizontal inset of the lane inside the widget, matching the strip below.
const PAD_X: f32 = 4.0;

/// A press that moved less than this (px) is a click.
const CLICK_SLOP_PX: f32 = 3.0;

/// No tick label within this many px of the lane's right edge (it would be
/// clipped).
const LABEL_RIGHT_GUARD_PX: f32 = 34.0;

/// The press that started the current overview drag, kept in egui memory
/// for the drag's lifetime.
#[derive(Clone, Copy)]
struct OverviewDrag {
    grab: OverviewGrab,
    press_x: f32,
    /// Pointer time minus the view's start at the press (a box move keeps
    /// it, so the box never jumps under the pointer).
    offset_secs: f64,
    /// The overview's start at the press (a lane drag slides from here).
    overview_start: f64,
    moved: bool,
}

/// Render the overview and run its gestures.
pub(super) fn render_overview(
    ui: &mut egui::Ui,
    state: &mut crate::state::AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
    let available_w = ui.available_width();
    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_w, style::OVERVIEW_H),
        Sense::click_and_drag(),
    );
    let full_rect = response.rect;
    let lane = Rect::from_min_max(
        Pos2::new(
            full_rect.left() + PAD_X,
            full_rect.top() + style::OVERVIEW_PAD_TOP,
        ),
        Pos2::new(
            full_rect.right() - PAD_X,
            full_rect.top() + style::OVERVIEW_PAD_TOP + style::OVERVIEW_LANE_H,
        ),
    );
    if lane.width() <= 0.0 {
        return;
    }
    let dark = state.is_dark;
    let now = state.frame_now.secs();
    let era = crate::core::NEXRAD_ARCHIVE_START_SECS;
    let streaming = live.mode_state.is_active();
    let drag_id = response.id.with("overview_drag");
    let drag: Option<OverviewDrag> = ui.ctx().memory(|m| m.data.get_temp(drag_id));

    // Keep the overview on the view, except while the box itself is being
    // dragged: sliding the lane under the pointer then would run away.
    let view_start = playback.state.timeline_view_start;
    let view_span = playback.state.view_width_secs();
    let box_drag = drag.is_some_and(|d| d.grab != OverviewGrab::Lane);
    if !box_drag {
        playback
            .state
            .overview
            .reconcile(view_start, view_span, now, era);
    }
    let ov = playback.state.overview;
    let ts_to_x = |ts: f64| lane.left() + ((ts - ov.start) / ov.span) as f32 * lane.width();
    let x_to_ts = |x: f32| ov.start + ((x - lane.left()) / lane.width()) as f64 * ov.span;

    painter.rect_filled(lane, 1.5, tl_colors::background(dark));
    let lane_painter = painter.with_clip_rect(lane);
    paint_coverage(&lane_painter, timeline, lane, dark, &ts_to_x);
    let now_x = ts_to_x(now);
    if now_x < lane.right() {
        lane_painter.rect_filled(
            Rect::from_min_max(Pos2::new(now_x.max(lane.left()), lane.top()), lane.max),
            0.0,
            tl_colors::future_wash(dark),
        );
    }
    paint_ticks(
        &lane_painter,
        lane,
        &ov,
        state.use_local_time,
        dark,
        &ts_to_x,
    );

    // The selected range, in the strip's amber, along the lane's top.
    if let Some((s, e)) = playback.state.selection_range() {
        let (x0, x1) = (ts_to_x(s).max(lane.left()), ts_to_x(e).min(lane.right()));
        if x1 > lane.left() && x0 < lane.right() {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x0, lane.top()),
                    Pos2::new(x1.max(x0 + 2.0), lane.top() + 3.0),
                ),
                0.0,
                tl_colors::RANGE,
            );
        }
    }
    if streaming && now_x >= lane.left() - 1.0 && now_x <= lane.right() + 1.0 {
        painter.circle_filled(
            Pos2::new(now_x.min(lane.right() - 4.0), lane.center().y),
            3.5,
            tl_colors::LIVE_ACTIVE,
        );
    }

    // The view box, or a chip pointing at it when it's out of sight.
    let view_end = view_start + view_span;
    let box_px = (ts_to_x(view_start), ts_to_x(view_end));
    let chip = paint_view_box(&painter, full_rect, lane, box_px, dark);

    let playhead = playback.state.playback_position();
    let following_live = streaming && playback.state.is_pinned();
    let px = ts_to_x(playhead);
    if !following_live && px >= lane.left() && px <= lane.right() {
        painter.line_segment(
            [Pos2::new(px, lane.top()), Pos2::new(px, lane.bottom())],
            Stroke::new(1.5_f32, tl_colors::selection(dark)),
        );
    }

    // -- Gestures -----------------------------------------------------------
    let hover_grab = response
        .hover_pos()
        .filter(|p| chip.is_none_or(|c| !c.contains(*p)))
        .map(|p| Overview::grab_at(box_px, p.x));

    if response.drag_started_by(egui::PointerButton::Primary) {
        let press_x = ui
            .input(|i| i.pointer.press_origin())
            .map_or(lane.left(), |p| p.x);
        let on_chip = ui
            .input(|i| i.pointer.press_origin())
            .is_some_and(|p| chip.is_some_and(|c| c.contains(p)));
        if !on_chip {
            let d = OverviewDrag {
                grab: Overview::grab_at(box_px, press_x),
                press_x,
                offset_secs: x_to_ts(press_x) - view_start,
                overview_start: ov.start,
                moved: false,
            };
            ui.ctx().memory_mut(|m| m.data.insert_temp(drag_id, d));
        }
    }
    let drag: Option<OverviewDrag> = ui.ctx().memory(|m| m.data.get_temp(drag_id));

    if let (Some(mut d), Some(pos)) = (drag, response.interact_pointer_pos()) {
        if response.dragged_by(egui::PointerButton::Primary) {
            d.moved |= (pos.x - d.press_x).abs() > CLICK_SLOP_PX;
            ui.ctx().memory_mut(|m| m.data.insert_temp(drag_id, d));
            let ts = x_to_ts(pos.x);
            match d.grab {
                OverviewGrab::Lane => {
                    // Content follows the pointer, like sliding paper.
                    let shift = (pos.x - d.press_x) as f64 / lane.width() as f64 * ov.span;
                    playback
                        .state
                        .overview
                        .slide_to(d.overview_start - shift, now, era);
                }
                OverviewGrab::View => {
                    let start =
                        crate::core::clamp_view_start(ts - d.offset_secs, view_span, era, now);
                    playback
                        .state
                        .show_view(start, start + view_span, false, now);
                }
                side => {
                    if let Some((s, e)) = resized_view(side, ts, view_start, view_end) {
                        let keep_end = side == OverviewGrab::ViewStart;
                        playback.state.show_view(s, e, keep_end, now);
                    }
                }
            }
        }
    }

    if response.drag_stopped() {
        if let Some(d) = drag {
            ui.ctx()
                .memory_mut(|m| m.data.remove::<OverviewDrag>(drag_id));
            if d.grab == OverviewGrab::Lane && !d.moved {
                look_at(playback, x_to_ts(d.press_x), now);
            }
        }
    } else if response.clicked() {
        let pos = response.interact_pointer_pos();
        if pos.is_some_and(|p| chip.is_some_and(|c| c.contains(p))) {
            playback
                .state
                .overview
                .center_on(view_start + view_span / 2.0, now, era);
        } else if let Some(p) = pos {
            if Overview::grab_at(box_px, p.x) == OverviewGrab::Lane {
                look_at(playback, x_to_ts(p.x), now);
            }
        }
    }

    // Sideways scroll slides the overview (content follows, as on the strip).
    if response.hovered() {
        let scroll_x = ui.input(|i| i.smooth_scroll_delta.x);
        if scroll_x != 0.0 {
            let ov = playback.state.overview;
            let shift = scroll_x as f64 / lane.width() as f64 * ov.span;
            playback.state.overview.slide_to(ov.start - shift, now, era);
        }
    }

    let dragging = response.dragged();
    let cursor = match (drag.map(|d| d.grab), hover_grab) {
        (Some(OverviewGrab::View), _) if dragging => Some(egui::CursorIcon::Grabbing),
        (Some(OverviewGrab::Lane), _) if dragging => Some(egui::CursorIcon::Grabbing),
        (Some(_), _) if dragging => Some(egui::CursorIcon::ResizeHorizontal),
        (_, Some(OverviewGrab::ViewStart | OverviewGrab::ViewEnd)) => {
            Some(egui::CursorIcon::ResizeHorizontal)
        }
        (_, Some(OverviewGrab::View)) => Some(egui::CursorIcon::Grab),
        (_, None) if response.hovered() => Some(egui::CursorIcon::PointingHand),
        (_, Some(OverviewGrab::Lane)) => Some(egui::CursorIcon::PointingHand),
        _ => None,
    };
    if let Some(c) = cursor {
        ui.ctx().set_cursor_icon(c);
    }
    if !dragging {
        let tip = match hover_grab {
            Some(OverviewGrab::View) => {
                Some("What the timeline below shows. Drag to move it, drag its sides to zoom.")
            }
            Some(OverviewGrab::ViewStart | OverviewGrab::ViewEnd) => Some("Drag to zoom the view."),
            Some(OverviewGrab::Lane) => {
                Some("Click to look here. Drag to slide the overview. Neither moves playback.")
            }
            None => Some("Bring the view back into the overview."),
        };
        if let Some(tip) = tip {
            response.on_hover_text(tip);
        }
    }
}

/// Center the strip's view on `ts` (no later than now), keeping its zoom.
fn look_at(playback: &mut crate::subsystem::Playback, ts: f64, now: f64) {
    let span = playback.state.view_width_secs();
    let start = crate::core::clamp_view_start(
        ts.min(now) - span / 2.0,
        span,
        crate::core::NEXRAD_ARCHIVE_START_SECS,
        now,
    );
    playback.state.show_view(start, start + span, false, now);
}

/// Server-available (faint) and on-device (solid) coverage, full lane
/// height. Available volumes already covered by cached data are skipped, the
/// same rule the strip uses.
fn paint_coverage(
    painter: &egui::Painter,
    timeline: &crate::subsystem::Timeline,
    lane: Rect,
    dark: bool,
    ts_to_x: &dyn Fn(f64) -> f32,
) {
    let segment = |start: f64, end: f64, color: Color32| {
        let x0 = ts_to_x(start);
        let x1 = ts_to_x(end).max(x0 + 1.0);
        if x1 < lane.left() || x0 > lane.right() {
            return;
        }
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, lane.top()), Pos2::new(x1, lane.bottom())),
            0.0,
            color,
        );
    };
    let view = crate::core::TimelineView::build(
        &timeline.scans,
        &timeline.shadow_scan_boundaries,
        None,
        None,
    );
    for b in view.shadow_boundaries() {
        if !view.is_covered_by_cached(b.start) {
            segment(
                b.start as f64,
                b.end as f64,
                tl_colors::available_fill(dark),
            );
        }
    }
    for r in timeline.scans.time_ranges() {
        segment(r.start, r.end, tl_colors::cached_fill(dark, false));
    }
}

/// Time labels inside the lane, at the strip's tick spacing for the
/// overview's scale. Day boundaries align to local midnight when showing
/// local time, as on the strip's ruler.
fn paint_ticks(
    painter: &egui::Painter,
    lane: Rect,
    ov: &Overview,
    use_local: bool,
    dark: bool,
    ts_to_x: &dyn Fn(f64) -> f32,
) {
    let config = super::select_tick_config(lane.width() as f64 / ov.span);
    let interval = config.major_interval.max(1);
    let tz_offset_secs: i64 = if use_local {
        let d = js_sys::Date::new_0();
        d.set_time(ov.start * 1000.0);
        -(d.get_timezone_offset() as i64) * 60
    } else {
        0
    };
    let local_start = ov.start as i64 + tz_offset_secs;
    let mut tick = (local_start / interval) * interval - tz_offset_secs;
    let end = ov.end() as i64;
    while tick <= end {
        let x = ts_to_x(tick as f64).round() + 0.5;
        if x >= lane.left() && x <= lane.right() {
            // Midnight names the day when the ticks are finer than days.
            let midnight = interval < 86_400 && (tick + tz_offset_secs) % 86_400 == 0;
            let (line, label) = if midnight {
                let dt = super::DateTimeComponents::from_timestamp(tick, use_local);
                (
                    tl_colors::tick_major(dark),
                    format!("{} {}", dt.month_abbrev(), dt.day),
                )
            } else {
                (
                    tl_colors::tick_minor(dark),
                    super::format_timestamp(tick, config, use_local),
                )
            };
            painter.line_segment(
                [Pos2::new(x, lane.top()), Pos2::new(x, lane.bottom())],
                Stroke::new(1.0_f32, line),
            );
            if x <= lane.right() - LABEL_RIGHT_GUARD_PX {
                painter.text(
                    Pos2::new(x + 3.0, lane.top() + 2.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    style::tick_font(),
                    if midnight {
                        tl_colors::selection(dark)
                    } else {
                        tl_colors::tick_label(dark)
                    },
                );
            }
        }
        tick += interval;
    }
}

/// Paint the view box (or, when it's out of sight, a chip at the lane edge
/// pointing at it) and the funnel joining it to the strip below. Returns the
/// chip's rect, which brings the box back when clicked.
fn paint_view_box(
    painter: &egui::Painter,
    full_rect: Rect,
    lane: Rect,
    (x0, x1): (f32, f32),
    dark: bool,
) -> Option<Rect> {
    let edge = tl_colors::view_box_edge(dark);
    let fill = tl_colors::view_box_fill(dark);
    if x1 < lane.left() || x0 > lane.right() {
        let left = x1 < lane.left();
        let label = if left {
            format!("{} view", egui_phosphor::regular::CARET_LEFT)
        } else {
            format!("view {}", egui_phosphor::regular::CARET_RIGHT)
        };
        let galley = painter.layout_no_wrap(label, FontId::proportional(10.0), edge);
        let size = galley.size() + Vec2::new(10.0, 2.0);
        let min_x = if left {
            lane.left() + 2.0
        } else {
            lane.right() - 2.0 - size.x
        };
        let chip = Rect::from_min_size(Pos2::new(min_x, lane.center().y - size.y / 2.0), size);
        painter.rect_filled(chip, 3.0, tl_colors::background(dark));
        painter.rect_stroke(chip, 3.0, Stroke::new(1.0, edge), StrokeKind::Inside);
        painter.galley(chip.min + Vec2::new(5.0, 1.0), galley, edge);
        return Some(chip);
    }
    if x1 - x0 < 5.0 {
        let x = (x0 + x1) / 2.0;
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x - 1.0, lane.top()),
                Pos2::new(x + 1.0, lane.bottom()),
            ),
            0.0,
            edge,
        );
    } else {
        let r = Rect::from_min_max(Pos2::new(x0, lane.top()), Pos2::new(x1, lane.bottom()));
        painter.rect_filled(r, 1.0, fill);
        painter.rect_stroke(r, 1.0, Stroke::new(1.2, edge), StrokeKind::Inside);
    }
    // The funnel: from the box's bottom corners out to the strip's edges.
    let (a, b) = (
        x0.clamp(lane.left(), lane.right()),
        x1.clamp(lane.left(), lane.right()),
    );
    let (top, bottom) = (lane.bottom(), full_rect.bottom());
    let (l, r) = (full_rect.left(), full_rect.right());
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(a, top),
            Pos2::new(b, top),
            Pos2::new(r, bottom),
            Pos2::new(l, bottom),
        ],
        tl_colors::view_box_fill(dark).gamma_multiply(0.5),
        Stroke::NONE,
    ));
    let line = Stroke::new(1.0, tl_colors::tick_minor(dark));
    painter.line_segment([Pos2::new(a, top), Pos2::new(l, bottom)], line);
    painter.line_segment([Pos2::new(b, top), Pos2::new(r, bottom)], line);
    None
}
