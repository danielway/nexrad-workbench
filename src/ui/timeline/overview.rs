//! The overview lane above the strip (round-4 design): 48 hours of context
//! by default, zooming out to the whole archive, with a box marking what the
//! strip below shows.
//!
//! Two channels, two glyphs (one-instrument §5 "Overview"):
//!   - bar height = how much echo, from the archive's file sizes
//!     ([`crate::core::storm_activity`]), stretched so the view's typical
//!     day sits low and its standouts reach the top: zoomed out, a bar's
//!     biggest day (a faint stub while its days are still listing); zoomed
//!     in, each volume (full height until its day's sizes are known). One
//!     bar color, so nothing competes with the height.
//!   - a slate underline = what is held on this device, at its real extent
//!   - amber pennants on the underline = saved events
//!   - a neutral tick on the baseline = a known outage
//!   - an amber line along the top = the selected range
//!   - the red dot = now (full strength while streaming)
//!   - a white tick = the playhead, when it isn't at now
//!   - the view box, joined to the strip below by a faint funnel
//!
//! Labels and hover text are in the chosen zone, like the strip.
//!
//! Gestures: drag the box to move the view, drag its sides to zoom the view,
//! drag the rest of the lane (or scroll sideways) to slide the overview,
//! scroll to zoom it, click the lane to look there (zoomed out to bars, to
//! go to that bar's stormiest day). None of them touch playback.
//! When the box is out of sight, a chip at the lane's edge brings it back.
//!
//! A thin projection of [`crate::core::overview`]: the window, the grabs, the
//! resize, every mark ([`OverviewMarks`]) and the hover words are decided
//! there. Desktop only; the mobile scrubber plays this role on phones.

use crate::core::overview::{
    bar_shape, bar_tip, detail_for, label_step, label_times, resized_view, scan_tip, BarShape,
    BarTipInputs, LabelStep, Overview, OverviewDetail, OverviewEvent, OverviewGrab,
    OverviewMarkInputs, OverviewMarks, LANE_HINT, PENNANT_HIT_PX,
};
use crate::core::storm_activity::{ScanSizes, SpanActivity, ViewScale};
use crate::data::SiteId;
use crate::state::TimeBucket;
use crate::ui::theme::timeline as tl_colors;
use crate::ui::time_format::ZoneClock;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use super::style;

/// Horizontal inset of the lane inside the widget, matching the strip below.
const PAD_X: f32 = 4.0;

/// A press that moved less than this (px) is a click.
const CLICK_SLOP_PX: f32 = 3.0;

/// No tick label within this many px of the lane's right edge (it would be
/// clipped).
const LABEL_RIGHT_GUARD_PX: f32 = 34.0;

/// Labels sit at least this far apart.
const LABEL_MIN_PX: f64 = 64.0;

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
    let px_per_sec = lane.width() as f64 / ov.span;
    let detail = detail_for(px_per_sec);

    painter.rect_filled(lane, 1.5, tl_colors::BACKGROUND);
    let lane_painter = painter.with_clip_rect(lane);
    // Bars stand on the underline band, which carries what is held and the
    // saved events.
    let bars_rect = Rect::from_min_max(
        lane.min,
        Pos2::new(lane.right(), lane.bottom() - style::OVERVIEW_UNDERLINE_H),
    );
    let clock = ZoneClock {
        use_local: state.use_local_time,
    };
    let site = SiteId::new(state.viz_state.site_id.clone());
    let mut sizes = ScanSizes::default();
    let (bars, activity) = match detail {
        OverviewDetail::Scans => {
            sizes = timeline.activity.scans(&site, ov.start, ov.end());
            paint_scans(&lane_painter, timeline, &sizes, bars_rect, &ts_to_x);
            (Vec::new(), Vec::new())
        }
        OverviewDetail::Bars(granularity) => {
            let bars = crate::state::aggregate_buckets(
                &timeline.scans,
                &timeline.shadow_scan_boundaries,
                &state.saved_events,
                &state.viz_state.site_id,
                ov.start.max(era),
                ov.end().min(now),
                granularity,
            );
            let activity: Vec<SpanActivity> = bars
                .iter()
                .map(|b| timeline.activity.span(&site, b.start, b.end))
                .collect();
            let scale = ViewScale::from_levels(activity.iter().filter_map(|a| a.peak));
            paint_bars(&lane_painter, &bars, &activity, &scale, bars_rect, &ts_to_x);
            (bars, activity)
        }
    };
    // Zoomed out, a click goes to the clicked bar's stormiest known day.
    let bar_day = |ts: f64| -> Option<f64> {
        let i = bars.iter().position(|b| ts >= b.start && ts < b.end)?;
        let clicked = crate::state::calendar::utc_day_start(ts.clamp(era, now)) as i64;
        Some(activity[i].go_to_day(clicked) as f64)
    };
    let now_x = ts_to_x(now);
    if now_x < lane.right() {
        lane_painter.rect_filled(
            Rect::from_min_max(Pos2::new(now_x.max(lane.left()), lane.top()), lane.max),
            0.0,
            tl_colors::future_wash(),
        );
    }
    let use_local = state.use_local_time;
    match label_step(px_per_sec, LABEL_MIN_PX) {
        LabelStep::Clock => paint_ticks(&lane_painter, lane, &ov, use_local, &ts_to_x),
        step => paint_calendar_labels(&lane_painter, lane, &ov, step, use_local, &ts_to_x),
    }

    // Every other mark is placed by the core.
    let held: Vec<(f64, f64)> = timeline
        .scans
        .time_ranges()
        .iter()
        .map(|r| (r.start, r.end))
        .collect();
    let events: Vec<OverviewEvent<'_>> = state
        .saved_events
        .events
        .iter()
        .filter(|e| e.site_id == state.viz_state.site_id)
        .map(|e| OverviewEvent {
            start: e.start_time,
            end: e.end_time,
            name: &e.name,
        })
        .collect();
    let outage_days = timeline.activity.outage_days(&site, ov.start, ov.end());
    let marks = OverviewMarks::build(&OverviewMarkInputs {
        start: ov.start,
        end: ov.end(),
        px_per_sec,
        held: &held,
        events: &events,
        outage_days: &outage_days,
        now,
        streaming,
        playhead: playback.state.playback_position(),
        playhead_at_now: streaming && playback.state.is_pinned(),
        range: playback.state.selection_range(),
        range_pinned: playback.state.is_live_range() && !playback.state.selection_in_progress(),
    });
    paint_marks(&painter, &marks, lane, bars_rect, &ts_to_x);

    // The view box, or a chip pointing at it when it's out of sight.
    let view_end = view_start + view_span;
    let box_px = (ts_to_x(view_start), ts_to_x(view_end));
    let chip = paint_view_box(&painter, full_rect, lane, box_px);
    if let Some(ts) = marks.playhead_tick {
        let px = ts_to_x(ts);
        painter.line_segment(
            [Pos2::new(px, lane.top()), Pos2::new(px, lane.bottom())],
            Stroke::new(1.5_f32, tl_colors::PLAYHEAD),
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
                let ts = x_to_ts(d.press_x);
                look_at(playback, ts, bar_day(ts), now, detail);
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
                let ts = x_to_ts(p.x);
                look_at(playback, ts, bar_day(ts), now, detail);
            }
        }
    }

    // Scroll zooms the overview at the pointer; sideways scroll slides it
    // (content follows, as on the strip).
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta);
        let ov = playback.state.overview;
        if scroll.x != 0.0 {
            let shift = scroll.x as f64 / lane.width() as f64 * ov.span;
            playback.state.overview.slide_to(ov.start - shift, now, era);
        }
        if scroll.y != 0.0 {
            let anchor = response
                .hover_pos()
                .map_or(ov.start + ov.span / 2.0, |p| x_to_ts(p.x));
            let factor = (-(scroll.y as f64) * 0.004).exp();
            playback
                .state
                .overview
                .zoom(anchor, factor, view_span, now, era);
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
            Some(OverviewGrab::View) => Some(
                "What the timeline below shows. Drag to move it, drag its sides to zoom."
                    .to_string(),
            ),
            Some(OverviewGrab::ViewStart | OverviewGrab::ViewEnd) => {
                Some("Drag to zoom the view.".to_string())
            }
            Some(OverviewGrab::Lane) => None,
            None => Some("Bring the view back into the overview.".to_string()),
        };
        let tip = tip.or_else(|| {
            let ts = response.hover_pos().map(|p| x_to_ts(p.x))?;
            if let Some(p) = marks.pennant_at(ts, PENNANT_HIT_PX / px_per_sec) {
                return Some(format!("Saved event: {}", p.name));
            }
            let i = bars.iter().position(|b| ts >= b.start && ts < b.end);
            let Some(i) = i else {
                return Some(match sizes.at(ts) {
                    Some(scan) => scan_tip(&clock, scan),
                    None => LANE_HINT.to_string(),
                });
            };
            let day_mean_mb = timeline
                .activity
                .day(&site, bars[i].start as i64)
                .filter(|_| bars[i].end - bars[i].start <= 86_400.0)
                .map(|d| d.mean_bytes as f64 / 1e6);
            Some(bar_tip(
                &clock,
                &BarTipInputs {
                    start: bars[i].start,
                    end: bars[i].end,
                    activity: activity[i],
                    day_mean_mb,
                    cached_secs: bars[i].cached_secs,
                    go_to: bar_day(ts).unwrap_or(ts),
                },
            ))
        });
        if let Some(tip) = tip {
            response.on_hover_text(tip);
        }
    }
}

/// What a click on the lane does: center the strip's view on `ts` (no later
/// than now) at its zoom, or, zoomed out to bars, go to `bar_day` (the bar's
/// stormiest known day, else the day clicked).
fn look_at(
    playback: &mut crate::subsystem::Playback,
    ts: f64,
    bar_day: Option<f64>,
    now: f64,
    detail: OverviewDetail,
) {
    let era = crate::core::NEXRAD_ARCHIVE_START_SECS;
    if let OverviewDetail::Bars(_) = detail {
        let day =
            bar_day.unwrap_or_else(|| crate::state::calendar::utc_day_start(ts.clamp(era, now)));
        playback.state.overview.go_to_day(day, now, era);
        playback.state.show_view(day, day + 86_400.0, false, now);
        return;
    }
    let span = playback.state.view_width_secs();
    let start = crate::core::clamp_view_start(
        ts.min(now) - span / 2.0,
        span,
        crate::core::NEXRAD_ARCHIVE_START_SECS,
        now,
    );
    playback.state.show_view(start, start + span, false, now);
}

/// Zoomed out: one bar per day / week / month / quarter, as tall as its
/// stormiest known day on the view's `scale`, in one color. A bar whose
/// days are still listing draws a faint stub; an outage draws nothing here
/// (its tick is a mark).
fn paint_bars(
    painter: &egui::Painter,
    bars: &[TimeBucket],
    activity: &[SpanActivity],
    scale: &ViewScale,
    area: Rect,
    ts_to_x: &dyn Fn(f64) -> f32,
) {
    for (b, act) in bars.iter().zip(activity) {
        let (height, color) = match bar_shape(act.bar(), scale) {
            BarShape::Height(frac) => (area.height() * frac, tl_colors::activity_fill()),
            BarShape::Pending => (2.0, tl_colors::activity_pending()),
            BarShape::Empty => continue,
        };
        let (x0, x1) = (ts_to_x(b.start), ts_to_x(b.end));
        let x1 = if x1 - x0 > 3.0 {
            x1 - 1.0
        } else {
            x1.max(x0 + 1.0)
        };
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, area.bottom() - height),
                Pos2::new(x1, area.bottom()),
            ),
            0.0,
            color,
        );
    }
}

/// The core-placed marks: the held underline, pennants, outage ticks, the
/// range line and the now dot.
fn paint_marks(
    painter: &egui::Painter,
    marks: &OverviewMarks,
    lane: Rect,
    bars: Rect,
    ts_to_x: &dyn Fn(f64) -> f32,
) {
    let clip = |x: f32| x.clamp(lane.left(), lane.right());
    let underline_y = lane.bottom() - 1.0;
    let span = |s: f64, e: f64| {
        let x0 = clip(ts_to_x(s));
        (x0, clip(ts_to_x(e)).max(x0 + 1.0))
    };
    for &(s, e) in &marks.held {
        let (x0, x1) = span(s, e);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, underline_y - 1.0),
                Pos2::new(x1, lane.bottom()),
            ),
            0.0,
            tl_colors::HELD,
        );
    }
    for &(s, e) in &marks.outages {
        let (x0, x1) = span(s, e);
        let y = bars.bottom();
        painter.line_segment(
            [Pos2::new(x0, y - 0.5), Pos2::new(x1, y - 0.5)],
            Stroke::new(1.0_f32, tl_colors::OUTAGE),
        );
        painter.line_segment(
            [Pos2::new(x0, y - 6.0), Pos2::new(x0, y)],
            Stroke::new(1.5_f32, tl_colors::OUTAGE),
        );
    }
    for p in &marks.pennants {
        let (x0, x1) = span(p.start, p.end);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, underline_y - 1.0),
                Pos2::new(x1, lane.bottom()),
            ),
            0.0,
            tl_colors::PENNANT,
        );
        let px = ts_to_x(p.start);
        if px >= lane.left() && px <= lane.right() {
            let top = lane.top() + 4.0;
            painter.line_segment(
                [Pos2::new(px, top), Pos2::new(px, lane.bottom())],
                Stroke::new(1.0_f32, tl_colors::PENNANT),
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(px, top),
                    Pos2::new(px + 6.0, top + 2.5),
                    Pos2::new(px, top + 5.0),
                ],
                tl_colors::PENNANT,
                Stroke::NONE,
            ));
        }
    }
    if let Some((s, e)) = marks.range {
        let (x0, x1) = span(s, e);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, lane.top()),
                Pos2::new(x1.max(x0 + 2.0), lane.top() + 3.0),
            ),
            0.0,
            tl_colors::RANGE,
        );
    }
    if let Some(dot) = marks.now_dot {
        let x = ts_to_x(dot.ts).min(lane.right() - 4.0);
        let color = if dot.streaming {
            tl_colors::LIVE_ACTIVE
        } else {
            tl_colors::NOW_IDLE
        };
        painter.circle_filled(Pos2::new(x, bars.center().y), 3.5, color);
    }
}

/// Day, month or year labels for a zoomed-out overview, on midnights of the
/// chosen zone.
fn paint_calendar_labels(
    painter: &egui::Painter,
    lane: Rect,
    ov: &Overview,
    step: LabelStep,
    use_local: bool,
    ts_to_x: &dyn Fn(f64) -> f32,
) {
    let offset = super::tz_offset_secs(ov.start + ov.span / 2.0, use_local);
    for t in label_times(ov.start, ov.end(), step, offset) {
        let x = ts_to_x(t).round() + 0.5;
        if x < lane.left() || x > lane.right() {
            continue;
        }
        let dt = super::DateTimeComponents::from_timestamp(t as i64, use_local);
        let (label, major) = match step {
            LabelStep::Years(_) => (dt.year.to_string(), true),
            LabelStep::Months(_) if dt.month == 1 => (dt.year.to_string(), true),
            LabelStep::Months(_) => (dt.month_abbrev().to_string(), false),
            _ if dt.day == 1 && dt.month == 1 => (dt.year.to_string(), true),
            _ => (format!("{} {}", dt.month_abbrev(), dt.day), dt.day == 1),
        };
        painter.line_segment(
            [Pos2::new(x, lane.top()), Pos2::new(x, lane.bottom())],
            Stroke::new(
                1.0_f32,
                if major {
                    tl_colors::TICK_MAJOR
                } else {
                    tl_colors::TICK_MINOR
                },
            ),
        );
        if x <= lane.right() - LABEL_RIGHT_GUARD_PX {
            painter.text(
                Pos2::new(x + 3.0, lane.top() + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                style::tick_font(),
                if major {
                    tl_colors::PLAYHEAD
                } else {
                    tl_colors::TICK_LABEL
                },
            );
        }
    }
}

/// Zoomed in: each volume of a day whose sizes are known, as tall as its
/// size on the view's scale, in the one bar color. Days not known yet show
/// their listed and on-device coverage as a faint full-height wash (what is
/// held is the underline), skipping listed volumes already covered by
/// cached data, the same rule the strip uses.
fn paint_scans(
    painter: &egui::Painter,
    timeline: &crate::subsystem::Timeline,
    sizes: &ScanSizes,
    lane: Rect,
    ts_to_x: &dyn Fn(f64) -> f32,
) {
    let column = |start: f64, end: f64, height: f32, color: Color32| {
        let x0 = ts_to_x(start);
        let x1 = ts_to_x(end).max(x0 + 1.0);
        if x1 < lane.left() || x0 > lane.right() {
            return;
        }
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, lane.bottom() - height),
                Pos2::new(x1, lane.bottom()),
            ),
            0.0,
            color,
        );
    };
    // Full-height coverage, only across days whose sizes aren't known.
    let segment = |start: f64, end: f64, color: Color32| {
        let mut t = start;
        while t < end {
            let piece_end = end.min((crate::core::storm_activity::day_start_of(t) + 86_400) as f64);
            if !sizes.knows(t) {
                column(t, piece_end, lane.height(), color);
            }
            t = piece_end;
        }
    };
    let view = crate::core::TimelineView::build(
        &timeline.scans,
        &timeline.shadow_scan_boundaries,
        None,
        None,
    );
    for b in view.shadow_boundaries() {
        if !view.is_covered_by_cached(b.start) {
            segment(b.start as f64, b.end as f64, tl_colors::available_fill());
        }
    }
    // Held time is the underline, so cached coverage draws like any other.
    for r in timeline.scans.time_ranges() {
        segment(r.start, r.end, tl_colors::available_fill());
    }

    let (lo, hi) = (
        sizes
            .scans
            .partition_point(|s| ts_to_x(s.end) < lane.left()),
        sizes
            .scans
            .partition_point(|s| ts_to_x(s.start) <= lane.right()),
    );
    let visible = &sizes.scans[lo..hi.max(lo)];
    let scale = ViewScale::from_levels(visible.iter().map(|s| s.level));
    for scan in visible {
        column(
            scan.start,
            scan.end,
            lane.height() * scale.height_frac(scan.level),
            tl_colors::activity_fill(),
        );
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
    ts_to_x: &dyn Fn(f64) -> f32,
) {
    let config = super::select_tick_config(lane.width() as f64 / ov.span);
    let interval = config.major_interval.max(1);
    let tz_offset_secs = super::tz_offset_secs(ov.start, use_local);
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
                    tl_colors::TICK_MAJOR,
                    format!("{} {}", dt.month_abbrev(), dt.day),
                )
            } else {
                (
                    tl_colors::TICK_MINOR,
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
                        tl_colors::PLAYHEAD
                    } else {
                        tl_colors::TICK_LABEL
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
) -> Option<Rect> {
    let edge = tl_colors::view_box_edge();
    let fill = tl_colors::view_box_fill();
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
        painter.rect_filled(chip, 3.0, tl_colors::BACKGROUND);
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
        tl_colors::view_box_fill().gamma_multiply(0.5),
        Stroke::NONE,
    ));
    let line = Stroke::new(1.0, tl_colors::TICK_MINOR);
    painter.line_segment([Pos2::new(a, top), Pos2::new(l, bottom)], line);
    painter.line_segment([Pos2::new(b, top), Pos2::new(r, bottom)], line);
    None
}
