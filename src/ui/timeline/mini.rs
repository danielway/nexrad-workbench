//! The phone's mini timeline (one-instrument §13, phone timeline §1): a
//! small timeline at one of three zoom stops, a thin projection of
//! [`crate::core::phone_strip`].
//!
//! Its window is [`crate::core::phone_strip::strip_window`]. At 15 MIN it
//! draws the desktop strip's real cells (the last three volumes and the
//! ghost of the next); at 3 H and DAY the storm-activity bars of
//! [`crate::core::phone_strip::activity_bars`] (one per volume, one per
//! half hour) in the storm blue, the slate on-device underline under them
//! and a quiet wash where no sizes are known yet. Every stop draws the
//! periwinkle ring on the frame on screen, the white playhead line when
//! detached, the red now line and the small now tab under it
//! ([`crate::core::phone_strip::strip_cap`]); the ruler
//! ([`crate::core::phone_strip::ruler_ticks`]) sits on the baseline and its
//! labels give way to the tab. The zoom pill at the left edge says what the
//! strip spans and taps to the next stop.
//!
//! Rows, from the strip's top: the now line from 4 to 52; cells and bars
//! from 10 to 34 (the ring centred at 22); the underline at 36; ruler ticks
//! at 40 and labels down to the baseline; the tab from 37 to 53, below the
//! cells, so it never hides a cell, the ring or the now line.
//!
//! A selected range ([`crate::core::phone_range::range_marks`]) draws as an
//! amber band from 8 to 36 over the cells, an amber edge line at each fixed
//! end, and round handles centred at 45 in the tab's row: start and end for
//! a fixed range, only the start for one pinned to now, a lone tick and
//! handle for a parked start. The tab steps aside from the handles'
//! targets ([`crate::core::phone_range::cap_clear_of_handles`]) and the
//! ruler labels give way to both.
//!
//! A tap seeks (`Intent::Seek`, Press), a drag scrubs (the same reducer,
//! synchronously, like the desktop strip), both snapped to the stop's unit
//! ([`crate::core::phone_strip::snap_scrub`]); a long-press opens the action
//! sheet ([`Intent::OpenPressSheet`]); a pinch moves one stop on release
//! ([`crate::core::phone_strip::StripTouch`]). A press inside a handle's
//! 24 px target moves that end, snapped like a scrub, with the desktop's
//! cost label above the strip while it moves; the drop selects the range
//! ([`crate::core::phone_range::HandleDrag`]). The strip and its gestures
//! sit in an area that reaches [`STRIP_REACH_PX`] above it into the map, so
//! a thumb that lands just short still gets the strip, and the map neither
//! pans on that press nor zooms on the strip's pinch.

use super::frame_cells::{paint_container, CellAnim};
use super::marks::paint_strip_marks;
use super::{reduced_motion, style, TimelineFrame, TrackRects};
use crate::core::phone_range::{
    cap_clear_of_handles, handle_at, range_marks, HandleDrag, HandleDrop, PressSheet, RangeFacts,
    RangeMarks, BAND_BOTTOM, BAND_TOP, HANDLE_CY, HANDLE_R, HANDLE_TARGET_PX,
};
use crate::core::phone_strip::{
    activity_bars, cap_intents, next_stop, on_device_spans, quiet_spans, ring_center,
    ruler_labels_shown, ruler_ticks, snap_scrub, strip_cap, strip_window, zoom_pill_label,
    PinchInput, StripTouch, StripTouchInput, StripUnits, StripZoom, STRIP_REACH_PX,
};
use crate::core::storm_activity::ScanSizes;
use crate::core::transport::SeekGesture;
use crate::core::{FrameJoinInputs, Intent, TimelineTier};
use crate::data::SiteId;
use crate::state::AppState;
use crate::ui::theme;
use crate::ui::theme::timeline as tl_colors;
use eframe::egui::{self, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

/// The mini timeline's height.
pub(crate) const MINI_TIMELINE_H: f32 = 56.0;

/// The cells' and bars' row, from the strip's top.
const TRACK_TOP: f32 = 10.0;
const TRACK_H: f32 = 24.0;

/// The on-device underline's row and thickness.
const UNDERLINE_TOP: f32 = 36.0;
const UNDERLINE_H: f32 = 3.0;

/// The ruler's tick row and the top of its labels.
const TICK_TOP: f32 = 40.0;
const TICK_H: f32 = 3.0;
const LABEL_TOP: f32 = 42.0;

/// The now and playhead lines' extent.
const LINE_TOP: f32 = 4.0;
const LINE_BOTTOM: f32 = 52.0;

/// The now tab's row.
const CAP_TOP: f32 = 37.0;
const CAP_H: f32 = 16.0;

/// The ring's radius and centre row.
const RING_R: f32 = 12.0;
const RING_CY: f32 = 22.0;

/// The zoom pill's column at the strip's left edge (the whole column is its
/// target), and the pill drawn in it.
const PILL_COL_W: f32 = 58.0;
const PILL_MIN: Vec2 = Vec2::new(8.0, 6.0);
const PILL_SIZE: Vec2 = Vec2::new(44.0, 16.0);

/// Room kept at the lane's right end.
const LANE_RIGHT_GAP: f32 = 8.0;

/// The gap between neighbouring bars and underline pieces at 3 H, in points.
const BAR_GAP_PX: f32 = 1.0;

/// How far back the window's volumes are looked for: past the widest
/// stop's half-span either side of the playhead.
fn lookback_secs(stop: StripZoom) -> f64 {
    match stop {
        StripZoom::Cuts | StripZoom::Volumes => 6.0 * 3600.0,
        StripZoom::Day => 26.0 * 3600.0,
    }
}

/// Draw the mini timeline at the available width and apply its gestures.
#[allow(clippy::too_many_arguments)]
pub(in crate::ui) fn render_mini_timeline(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    acquisition: &crate::subsystem::Acquisition,
    chrome: &mut crate::subsystem::Chrome,
) {
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(Vec2::new(width, MINI_TIMELINE_H), Sense::hover());
    // The strip draws in an area above the map's layer so its reach into
    // the map takes the press, clipped like the panel (it slides with it).
    let panel_clip = ui.clip_rect();
    let reach = Rect::from_min_max(
        Pos2::new(strip.left(), strip.top() - STRIP_REACH_PX),
        strip.max,
    );
    let clip = Rect::from_min_max(
        Pos2::new(panel_clip.left(), panel_clip.top() - STRIP_REACH_PX),
        panel_clip.max,
    );
    egui::Area::new(ui.id().with("mini_strip"))
        .order(egui::Order::Middle)
        .fixed_pos(reach.min)
        .constrain(false)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(clip);
            draw_strip(
                ui,
                strip,
                state,
                timeline,
                live,
                playback,
                acquisition,
                chrome,
            );
        });
}

#[allow(clippy::too_many_arguments)]
fn draw_strip(
    ui: &mut egui::Ui,
    strip: Rect,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    acquisition: &crate::subsystem::Acquisition,
    chrome: &mut crate::subsystem::Chrome,
) {
    let stop = chrome.phone_strip_zoom;
    let top = strip.top();
    let lane = Rect::from_min_max(
        Pos2::new(strip.left() + PILL_COL_W, top),
        Pos2::new(strip.right() - LANE_RIGHT_GAP, strip.bottom()),
    );
    let track = Rect::from_min_size(
        Pos2::new(lane.left(), top + TRACK_TOP),
        Vec2::new(lane.width(), TRACK_H),
    );
    // The press area: the lane and the reach above it. Allocated first so
    // the pill and the tab, drawn after, sit on top of it.
    let press_rect = Rect::from_min_max(Pos2::new(lane.left(), top - STRIP_REACH_PX), lane.max);
    let response = ui.allocate_rect(press_rect, Sense::click_and_drag());
    let painter = ui.painter().clone();
    painter.rect_filled(strip, 2.0, tl_colors::BACKGROUND);
    painter.rect_stroke(
        strip,
        2.0,
        Stroke::new(1.0_f32, tl_colors::BORDER),
        StrokeKind::Inside,
    );

    let now = state.frame_now.secs();
    let view = crate::core::TimelineView::build(
        &timeline.scans,
        &timeline.shadow_scan_boundaries,
        Some(&live.mode_state),
        live.radar_model.position.as_ref(),
    );

    // The same join the desktop strip makes: queued, in flight, failed, the
    // selected product and tilt, and the frame on screen for the ring.
    let failed = acquisition.state.failed_scan_starts();
    let mut in_flight = state.download_progress.in_flight_scans.clone();
    in_flight.extend_from_slice(&state.download_progress.active_scans);
    let active = state.viz_state.displayed.as_ref().map(|d| {
        (
            d.identity.scan_timestamp_secs(),
            d.identity.elevation_number,
        )
    });
    let join = FrameJoinInputs {
        queued: &state.download_progress.pending_scans,
        in_flight: &in_flight,
        failed: &failed,
        product: state.viz_state.product.to_worker_string(),
        tilt: state.viz_state.elevation_selection.elevation_number(),
        active,
        prev_active: None,
    };

    let playhead = playback.state.playback_position();
    let pinned = playback.state.is_pinned() || playback.state.is_live_range();
    let look_from = now.min(playhead) - lookback_secs(stop);
    let all = view.frame_containers_in_range(look_from, now.max(playhead) + 3600.0, join);
    let starts: Vec<f64> = all.iter().map(|c| c.start_secs).collect();
    let prev = chrome.phone_strip_view.map(|(w, _)| w);
    let window = strip_window(stop, now, playhead, pinned, &starts, prev);
    chrome.phone_strip_view = Some((window, lane.width()));
    let zoom = track.width() as f64 / window.span().max(1.0);

    let frame = TimelineFrame {
        view,
        now_secs: now,
        view_start: window.start,
        view_end: window.end,
        zoom,
        rects: TrackRects {
            tick: Rect::from_min_size(track.min, Vec2::new(track.width(), 0.0)),
            scan: track,
            overlay: track,
        },
        tier: TimelineTier::Micro,
        use_local: state.use_local_time,
    };

    let containers: Vec<_> = all
        .into_iter()
        .filter(|c| c.end_secs >= window.start && c.start_secs <= window.end)
        .collect();
    let reduced = reduced_motion(ui.ctx());
    let lane_painter = painter.with_clip_rect(lane.intersect(ui.clip_rect()));
    let site = SiteId::new(state.viz_state.site_id.clone());
    let sizes = if stop.per_volume() {
        timeline.activity.scans(&site, window.start, window.end)
    } else {
        ScanSizes::default()
    };
    let bars = activity_bars(stop, window, &sizes);
    let units = StripUnits::from_containers(&containers).with_listed(&sizes.scans);
    let snapped = |x: f32| snap_scrub(stop, frame.x_to_ts(x), &units, now);

    // The touch, stepped before the range draws so a moving handle's band
    // never trails the finger. A press that begins in a handle's target is
    // the handle's.
    let lane_x = (lane.left(), lane.right());
    let to_x = |t: f64| frame.ts_to_x(t);
    let facts = RangeFacts::of(&playback.state, now);
    let drag_key = response.id.with("mini_handle");
    let mut drag: Option<HandleDrag> = ui.ctx().memory(|m| m.data.get_temp(drag_key));
    let held = range_marks(&facts, drag.as_ref(), &to_x, lane_x);
    let key = response.id.with("mini_touch");
    let mut touch: StripTouch = ui
        .ctx()
        .memory(|m| m.data.get_temp(key))
        .unwrap_or_default();
    let gesture_area = Rect::from_min_max(
        Pos2::new(strip.left(), strip.top() - STRIP_REACH_PX),
        strip.max,
    );
    let pinch = ui.input(|i| i.multi_touch()).map(|t| PinchInput {
        zoom_delta: t.zoom_delta,
        began_on_strip: gesture_area.contains(t.start_pos),
    });
    let pos = response.interact_pointer_pos();
    let out = touch.step(StripTouchInput {
        down: response.is_pointer_button_down_on(),
        pos: pos.map(|p| (p.x, p.y)),
        now: ui.ctx().input(|i| i.time),
        pinch,
        stop,
        handle: pos.and_then(|p| handle_at(&held.handles, p.x, p.y - top)),
    });
    ui.ctx().memory_mut(|m| m.data.insert_temp(key, touch));

    // A moving handle previews its range; the drop selects it.
    if let Some(end) = out.moving {
        if drag.is_none() {
            drag = HandleDrag::grab(end, &facts);
        }
        if let (Some(d), Some(p)) = (drag.as_mut(), pos) {
            d.drag_to(snapped(p.x), now);
        }
    }
    let dropped = out
        .dropped
        .and_then(|_| drag.take())
        .map(|d| d.drop_at(&facts));
    if touch.press.is_none() {
        drag = None;
    }
    ui.ctx().memory_mut(|m| match drag {
        Some(d) => {
            m.data.insert_temp(drag_key, d);
        }
        None => m.data.remove::<HandleDrag>(drag_key),
    });
    let marks = range_marks(&facts, drag.as_ref(), &to_x, lane_x);

    let span_x = |s: f64, e: f64, gap: f32| {
        let x0 = frame.ts_to_x(s);
        let x1 = frame.ts_to_x(e);
        let g = if x1 - x0 > 2.0 * gap + 1.0 { gap } else { 0.0 };
        (x0 + g, (x1 - g).max(x0 + g + 1.0))
    };
    match stop {
        StripZoom::Cuts => {
            let anim = CellAnim {
                pulse: (0.5 + 0.5 * (ui.ctx().input(|i| i.time) * 3.0).sin()) as f32,
                reduced_motion: reduced,
                morph: 1.0,
            };
            let countdown = live.countdown_remaining_secs(now);
            let countdown_idx = crate::core::timeline_view::countdown_container(&containers, now);
            for (i, c) in containers.iter().enumerate() {
                // The ring is the strip's own circle, drawn below.
                let mut c = c.clone();
                for cell in &mut c.cells {
                    cell.is_active = false;
                    cell.is_prev_active = false;
                }
                paint_container(
                    &lane_painter,
                    &frame,
                    &c,
                    anim,
                    countdown,
                    countdown_idx == Some(i),
                );
            }
        }
        StripZoom::Volumes | StripZoom::Day => {
            let gap = if stop == StripZoom::Volumes {
                BAR_GAP_PX / 2.0
            } else {
                0.0
            };
            // Volumes with no sizes yet: the desktop's quiet wash.
            for (s, e) in quiet_spans(&containers, &sizes) {
                let (x0, x1) = span_x(s, e, 0.0);
                lane_painter.rect_filled(
                    Rect::from_min_max(Pos2::new(x0, track.top()), Pos2::new(x1, track.bottom())),
                    0.0,
                    tl_colors::available_fill(),
                );
            }
            for bar in &bars {
                let (x0, x1) = span_x(bar.start, bar.end, gap);
                let h = track.height() * bar.frac;
                lane_painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, track.bottom() - h),
                        Pos2::new(x1, track.bottom()),
                    ),
                    0.0,
                    tl_colors::storm_fill(),
                );
            }
            let join_secs = if stop == StripZoom::Day {
                2.0 / zoom.max(f64::MIN_POSITIVE)
            } else {
                0.0
            };
            for (s, e) in on_device_spans(&containers, join_secs) {
                let (x0, x1) = span_x(s, e, gap);
                lane_painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, top + UNDERLINE_TOP),
                        Pos2::new(x1, top + UNDERLINE_TOP + UNDERLINE_H),
                    ),
                    0.0,
                    tl_colors::HELD,
                );
            }
        }
    }
    let tier = if stop.per_volume() {
        TimelineTier::Macro
    } else {
        TimelineTier::Micro
    };
    let strip_marks = crate::core::timeline_view::strip_marks(&containers, zoom, tier);
    let failed_ticks = paint_strip_marks(&lane_painter, &frame, &strip_marks, None);
    paint_band(&lane_painter, top, &marks);
    if !reduced && (state.download_progress.is_active() || frame.view.live_volume().is_some()) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }

    // The now tab: its place first, so the ruler's labels can give way.
    let session = &live.session;
    let running = session.stream.is_running();
    let now_x = frame.ts_to_x(now);
    let now_in_lane = now >= window.start && now_x <= lane.right();
    let font = style::tick_font();
    let measure = |s: &str| {
        painter
            .layout_no_wrap(s.to_string(), font.clone(), theme::TEXT)
            .size()
            .x
    };
    let mut cap = strip_cap(
        session.now_cap,
        now_in_lane.then_some(now_x),
        lane.right(),
        &measure,
    );
    // The tab shares its row with the range handles: it steps aside so both
    // stay tappable.
    cap.x0 = cap_clear_of_handles(cap.x0, cap.width, &marks.handle_xs(), lane_x);

    // The ruler on the baseline, in the chosen zone.
    let offset = super::tz_offset_secs(window.start + window.span() / 2.0, state.use_local_time);
    let in_window: Vec<f64> = containers
        .iter()
        .filter(|c| !c.is_projected)
        .map(|c| c.start_secs)
        .collect();
    let ticks: Vec<_> = ruler_ticks(stop, window, &in_window, offset)
        .into_iter()
        .map(|t| (frame.ts_to_x(t.ts), t.label))
        .filter(|(x, _)| *x >= lane.left() && *x <= lane.right())
        .collect();
    let labelled: Vec<(f32, f32)> = ticks
        .iter()
        .filter(|(_, l)| !l.is_empty())
        .map(|(x, l)| (x + 2.0, measure(l)))
        .collect();
    let mut avoid = vec![(cap.x0, cap.x0 + cap.width)];
    avoid.extend(
        marks
            .handles
            .iter()
            .map(|h| (h.x - HANDLE_R, h.x + HANDLE_R)),
    );
    let mut shown = ruler_labels_shown(&labelled, lane_x, &avoid).into_iter();
    for (x, label) in ticks {
        lane_painter.line_segment(
            [
                Pos2::new(x, top + TICK_TOP),
                Pos2::new(x, top + TICK_TOP + TICK_H),
            ],
            Stroke::new(1.0_f32, tl_colors::TICK_MINOR),
        );
        if label.is_empty() || !shown.next().unwrap_or(false) {
            continue;
        }
        lane_painter.text(
            Pos2::new(x + 2.0, top + LABEL_TOP),
            egui::Align2::LEFT_TOP,
            label,
            font.clone(),
            tl_colors::TICK_LABEL,
        );
    }

    // The ring on the frame on screen, the playhead line when detached and
    // the now line.
    let active_ts = active.map(|(ts, _)| ts);
    if let Some(c) = ring_center(stop, active_ts, &containers, &bars) {
        let x = frame.ts_to_x(c);
        if x >= lane.left() && x <= lane.right() + RING_R {
            // On the strip's painter, so a ring on the newest volume is not
            // cut off where the lane ends.
            painter.circle_stroke(
                Pos2::new(x, top + RING_CY),
                RING_R,
                Stroke::new(1.5_f32, theme::SELECTED),
            );
        }
    }
    let playhead_at_now = live.mode_state.is_active() && playback.state.is_pinned();
    let red = if running {
        tl_colors::LIVE_ACTIVE
    } else {
        tl_colors::NOW_IDLE
    };
    let vline = |x: f32, color: egui::Color32| {
        lane_painter.line_segment(
            [
                Pos2::new(x, top + LINE_TOP),
                Pos2::new(x, top + LINE_BOTTOM),
            ],
            Stroke::new(1.5_f32, color),
        );
    };
    if !playhead_at_now {
        let x = frame.ts_to_x(playhead);
        if x >= lane.left() && x <= lane.right() {
            vline(x, tl_colors::PLAYHEAD);
        }
    }
    if now_in_lane {
        vline(now_x, red);
    }

    // The tab: filled red for LIVE, outlined for REJOIN and GO LIVE.
    let cap_rect = Rect::from_min_size(
        Pos2::new(cap.x0, top + CAP_TOP),
        Vec2::new(cap.width, CAP_H),
    );
    let cap_resp = ui
        .interact(cap_rect, ui.id().with("mini_now_tab"), Sense::click())
        .on_hover_text(match session.now_cap {
            crate::core::session::NowCap::Live => "Live",
            crate::core::session::NowCap::Rejoin => "Rejoin live",
            crate::core::session::NowCap::GoLive => "Go live",
        });
    if cap.filled {
        painter.rect_filled(cap_rect, 4.0, red);
    } else {
        painter.rect_filled(cap_rect, 4.0, tl_colors::BACKGROUND);
        painter.rect_stroke(cap_rect, 4.0, Stroke::new(1.2_f32, red), StrokeKind::Inside);
    }
    painter.text(
        cap_rect.center(),
        egui::Align2::CENTER_CENTER,
        &cap.label,
        font.clone(),
        if cap.filled { theme::TEXT } else { red },
    );
    if cap_resp.clicked() {
        for intent in cap_intents(session.now_cap, cap.edge) {
            state.push_command(intent);
        }
    }

    // The range handles, in the tab's row; the moving one shows its target.
    let moving = drag.filter(|d| d.moved).map(|d| d.end);
    paint_handles(&lane_painter, top, &marks, moving);
    // While a handle moves, the desktop's cost line above the strip.
    let preview = crate::core::phone_range::strip_range(&facts, drag.as_ref());
    if let (Some(d), Some(r)) = (drag.filter(|d| d.moved && !d.parked), preview) {
        let sizes = timeline.activity.scans(&site, r.start, r.end);
        let label = frame.view.range_cost_sized(r.start, r.end, &sizes).label();
        let x = frame.ts_to_x(if d.at <= d.anchor { r.start } else { r.end });
        paint_cost_label(ui, strip, x, &label);
        ui.ctx().request_repaint();
    }

    // The zoom pill: says what the strip spans, a tap goes to the next stop.
    let pill_col = Rect::from_min_max(strip.min, Pos2::new(lane.left(), strip.bottom()));
    let pill = Rect::from_min_size(strip.min + PILL_MIN, PILL_SIZE);
    let pill_resp = ui
        .interact(pill_col, ui.id().with("mini_zoom_pill"), Sense::click())
        .on_hover_text("Zoom: 15 minutes, 3 hours or the day");
    painter.rect_filled(pill, PILL_SIZE.y / 2.0, theme::PANEL);
    painter.rect_stroke(
        pill,
        PILL_SIZE.y / 2.0,
        Stroke::new(1.0_f32, tl_colors::BORDER),
        StrokeKind::Inside,
    );
    painter.text(
        pill.center(),
        egui::Align2::CENTER_CENTER,
        zoom_pill_label(stop),
        font,
        if pill_resp.is_pointer_button_down_on() {
            theme::SELECTED
        } else {
            theme::TEXT_DIM
        },
    );
    if pill_resp.clicked() {
        state.push_command(Intent::SetPhoneStripZoom(next_stop(stop)));
    }

    // Gestures. A press on the tab or a failure mark belongs to it.
    let owned = |p: Pos2| cap_rect.contains(p) || failed_ticks.iter().any(|t| t.rect.contains(p));
    chrome.phone_strip_claims_touch |= out.claimed;
    if let Some(next) = out.settle {
        state.push_command(Intent::SetPhoneStripZoom(next));
    }
    if out.press.waiting || out.claimed {
        ui.ctx().request_repaint();
    }
    match dropped {
        Some(HandleDrop::Select(intent)) => state.push_command(intent),
        Some(HandleDrop::TooLong) => state.toasts.notice(format!(
            "Range too long \u{2014} select at most {} days",
            (crate::core::MAX_SELECTION_SPAN_SECS / 86_400.0) as i64
        )),
        Some(HandleDrop::Nothing) | None => {}
    }
    if let Some((x, _)) = out.press.long_press {
        state.push_command(Intent::OpenPressSheet(PressSheet {
            ts: snapped(x),
            scan_start: frame.view.scan_start_at(frame.x_to_ts(x)),
        }));
        return;
    }
    state.pointer_scrub_active = response.dragged() && out.may_seek;
    if !out.may_seek {
        return;
    }
    if let Some(p) = pos.filter(|p| !owned(*p)) {
        let ts = snapped(p.x);
        if response.dragged() {
            // A scrub runs the seek reducer synchronously so the playhead
            // keeps up with the finger.
            live.seek(
                &mut playback.state,
                now,
                state.pause_stream_while_reviewing,
                ts,
                SeekGesture::Scrub,
            );
        } else if response.clicked() {
            state.push_command(Intent::Seek {
                ts,
                gesture: SeekGesture::Press,
            });
        }
    }

    // A tap on a failure mark retries the scans it stands for.
    if response.clicked() {
        if let Some(p) = pos {
            if let Some(tick) = failed_ticks.iter().find(|t| t.rect.contains(p)) {
                for key in &tick.keys {
                    if let Some(op) = acquisition.state.failed_operation_for_scan_start(
                        key.round() as i64,
                        crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
                    ) {
                        state.push_command(Intent::RetryFailed(op));
                    }
                }
            }
        }
    }
}

/// The range's band: a translucent wash over the cells from [`BAND_TOP`] to
/// [`BAND_BOTTOM`], with an amber edge line at each fixed end; or a parked
/// start's amber tick.
fn paint_band(painter: &egui::Painter, top: f32, marks: &RangeMarks) {
    let edge = |x: f32, w: f32| {
        painter.line_segment(
            [
                Pos2::new(x, top + BAND_TOP - 1.0),
                Pos2::new(x, top + BAND_BOTTOM + 1.0),
            ],
            Stroke::new(w, tl_colors::RANGE),
        );
    };
    if let Some((sx, ex)) = marks.band {
        let band = Rect::from_min_max(
            Pos2::new(sx, top + BAND_TOP),
            Pos2::new(ex, top + BAND_BOTTOM),
        );
        if band.width() > 0.0 {
            painter.rect_filled(band, 3.0, tl_colors::selection_fill());
        }
        edge(sx, 1.5);
        if !marks.pinned {
            edge(ex, 1.5);
        }
    }
    if let Some(x) = marks.parked_tick {
        edge(x, 2.0);
    }
}

/// The round amber handles below the band; the moving one shows its
/// [`HANDLE_TARGET_PX`] target as a faint ring.
fn paint_handles(
    painter: &egui::Painter,
    top: f32,
    marks: &RangeMarks,
    moving: Option<crate::core::phone_range::RangeEnd>,
) {
    for h in &marks.handles {
        let c = Pos2::new(h.x, top + HANDLE_CY);
        painter.circle_filled(c, HANDLE_R, tl_colors::RANGE);
        if moving == Some(h.end) {
            painter.circle_stroke(
                c,
                HANDLE_TARGET_PX / 2.0,
                Stroke::new(1.0_f32, tl_colors::RANGE.gamma_multiply(0.6)),
            );
        }
    }
}

/// The cost line (`1:30:00 · 18 scans · 71 MB`) on a scrim above the strip,
/// over the moving handle and kept inside the strip's width.
fn paint_cost_label(ui: &egui::Ui, strip: Rect, x: f32, label: &str) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        ui.id().with("mini_cost_label"),
    ));
    let galley = painter.layout_no_wrap(label.to_string(), style::tick_font(), theme::TEXT);
    let size = galley.size() + Vec2::new(12.0, 6.0);
    let left = (x - size.x / 2.0)
        .min(strip.right() - size.x)
        .max(strip.left());
    let bg = Rect::from_min_size(
        Pos2::new(left, strip.top() - STRIP_REACH_PX - size.y - 4.0),
        size,
    );
    painter.rect_filled(bg, 3.0, theme::map::scrim());
    painter.rect_stroke(
        bg,
        3.0,
        Stroke::new(1.0_f32, tl_colors::RANGE),
        StrokeKind::Inside,
    );
    painter.galley(bg.min + Vec2::new(6.0, 3.0), galley, theme::TEXT);
}
