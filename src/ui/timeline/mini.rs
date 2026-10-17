//! The phone's mini timeline (one-instrument §13, phone timeline §1): a
//! small timeline with the desktop strip's real cells, marks, playhead and
//! now cap, at one of three zoom stops.
//!
//! Its window is [`crate::core::phone_strip::strip_window`]: at Cuts the
//! last three volumes with the ring on the frame on screen, the now line
//! with its cap and the ghost of the next volume; at Volumes three hours
//! with one cell per volume ([`crate::core::phone_strip::volume_cell`]); at
//! Day 24 hours of storm bars sized from the day's listing
//! ([`crate::core::storm_activity::ScanSizes::bars`]) over the on-device
//! underline. The ruler under it is
//! [`crate::core::phone_strip::ruler_ticks`]. The zoom pill at the left edge
//! names the stop and taps to the next.
//!
//! A tap seeks (`Intent::Seek`, Press), a drag scrubs (the same reducer,
//! synchronously, like the desktop strip), both snapped to the stop's unit
//! ([`crate::core::phone_strip::snap_scrub`]); a long-press opens the scan
//! inspector; a pinch moves one stop on release
//! ([`crate::core::phone_strip::StripTouch`]). The strip and its gestures
//! sit in an area that reaches [`STRIP_REACH_PX`] above it into the map, so
//! a thumb that lands just short still gets the strip, and the map neither
//! pans on that press nor zooms on the strip's pinch.

use super::frame_cells::{paint_container, CellAnim};
use super::marks::paint_strip_marks;
use super::now_edge::render_now_affordance;
use super::ruler::render_playback_cursor;
use super::{reduced_motion, style, TimelineFrame, TrackRects};
use crate::core::phone_strip::{
    next_stop, ruler_ticks, snap_scrub, strip_window, volume_cell, zoom_pill_label, PinchInput,
    StripTouch, StripTouchInput, StripUnits, StripZoom, STRIP_REACH_PX,
};
use crate::core::storm_activity::ScanSizes;
use crate::core::transport::SeekGesture;
use crate::core::{FrameJoinInputs, Intent, TimelineTier};
use crate::data::SiteId;
use crate::state::AppState;
use crate::ui::theme;
use crate::ui::theme::timeline as tl_colors;
use eframe::egui::{self, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

/// The mini timeline's height: the cells (or bars) over the ruler.
pub(crate) const MINI_TIMELINE_H: f32 = 52.0;

/// The cells' track, from the strip's top; the ruler sits under it.
const TRACK_TOP: f32 = 2.0;
const TRACK_H: f32 = 36.0;

/// The zoom pill's column at the strip's left edge (the whole column is its
/// target), and the pill drawn in it.
const PILL_COL_W: f32 = 48.0;
const PILL_SIZE: Vec2 = Vec2::new(40.0, 18.0);

/// The gap between volumes at one cell per volume, in points.
const VOLUME_GAP_PX: f64 = 2.0;

/// The on-device underline's thickness at Day.
const UNDERLINE_H: f32 = 2.0;

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
    let lane = Rect::from_min_max(
        Pos2::new(strip.left() + PILL_COL_W, strip.top()),
        Pos2::new(strip.right() - 2.0, strip.bottom()),
    );
    let track = Rect::from_min_size(
        Pos2::new(lane.left(), strip.top() + TRACK_TOP),
        Vec2::new(lane.width(), TRACK_H),
    );
    // The press area: the lane and the reach above it. Allocated first so
    // the pill and the cap, drawn after, sit on top of it.
    let press_rect = Rect::from_min_max(
        Pos2::new(lane.left(), strip.top() - STRIP_REACH_PX),
        lane.max,
    );
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
    let window = strip_window(stop, now, playhead, pinned, &starts);
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
    let anim = CellAnim {
        pulse: (0.5 + 0.5 * (ui.ctx().input(|i| i.time) * 3.0).sin()) as f32,
        reduced_motion: reduced,
        morph: 1.0,
    };
    let clipped = painter.with_clip_rect(track.intersect(ui.clip_rect()));
    let site = SiteId::new(state.viz_state.site_id.clone());
    let mut sizes = ScanSizes::default();
    match stop {
        StripZoom::Cuts => {
            let countdown = live.countdown_remaining_secs(now);
            let countdown_idx = crate::core::timeline_view::countdown_container(&containers, now);
            for (i, c) in containers.iter().enumerate() {
                paint_container(
                    &clipped,
                    &frame,
                    c,
                    anim,
                    countdown,
                    countdown_idx == Some(i),
                );
            }
        }
        StripZoom::Volumes => {
            let gap = VOLUME_GAP_PX / zoom.max(f64::MIN_POSITIVE);
            for c in &containers {
                paint_container(&clipped, &frame, &volume_cell(c, gap), anim, None, false);
            }
        }
        StripZoom::Day => {
            sizes = timeline.activity.scans(&site, window.start, window.end);
            paint_day(
                &clipped,
                &frame,
                timeline,
                &sizes,
                &containers,
                now,
                playhead,
            );
        }
    }
    let tier = if stop.per_volume() {
        TimelineTier::Macro
    } else {
        TimelineTier::Micro
    };
    let marks = crate::core::timeline_view::strip_marks(&containers, zoom, tier);
    let failed_ticks = paint_strip_marks(&clipped, &frame, &marks, None);
    if !reduced && (state.download_progress.is_active() || frame.view.live_volume().is_some()) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }

    // The ruler under the track, in the chosen zone.
    let offset = super::tz_offset_secs(window.start + window.span() / 2.0, state.use_local_time);
    let in_window: Vec<f64> = containers
        .iter()
        .filter(|c| !c.is_projected)
        .map(|c| c.start_secs)
        .collect();
    let ruler_painter = painter.with_clip_rect(lane.intersect(ui.clip_rect()));
    for tick in ruler_ticks(stop, window, &in_window, offset) {
        let x = frame.ts_to_x(tick.ts);
        if x < lane.left() || x > lane.right() - 20.0 {
            continue;
        }
        ruler_painter.line_segment(
            [
                Pos2::new(x, track.bottom()),
                Pos2::new(x, track.bottom() + 3.0),
            ],
            Stroke::new(1.0_f32, tl_colors::TICK_MINOR),
        );
        ruler_painter.text(
            Pos2::new(x + 2.0, track.bottom() + 1.0),
            egui::Align2::LEFT_TOP,
            tick.label,
            style::tick_font(),
            tl_colors::TICK_LABEL,
        );
    }

    // The playhead (white) and the now line with its cap (LIVE, REJOIN or
    // GO LIVE, doing what the cap does), or their edge marks off-window.
    let playhead_at_now = live.mode_state.is_active() && playback.state.is_pinned();
    let edges = crate::core::timeline_view::edge_markers(
        window.start,
        window.end,
        (!playhead_at_now).then_some(playhead),
        now,
    );
    render_playback_cursor(&clipped, &frame, playhead, edges.playhead);
    let cap = render_now_affordance(ui, &clipped, state, live, &frame, edges.now);

    // The zoom pill: names the stop, a tap goes to the next one.
    let pill_col = Rect::from_min_max(strip.min, Pos2::new(lane.left(), strip.bottom()));
    let pill = Rect::from_min_size(Pos2::new(strip.left() + 4.0, track.top() + 6.0), PILL_SIZE);
    let pill_resp = ui
        .interact(pill_col, ui.id().with("mini_zoom_pill"), Sense::click())
        .on_hover_text("Zoom: cuts, volumes or the day");
    painter.rect_filled(pill, 3.0, theme::map::scrim());
    painter.rect_stroke(
        pill,
        3.0,
        Stroke::new(1.0_f32, tl_colors::BORDER),
        StrokeKind::Inside,
    );
    painter.text(
        pill.center(),
        egui::Align2::CENTER_CENTER,
        zoom_pill_label(stop),
        style::tick_font(),
        if pill_resp.is_pointer_button_down_on() {
            theme::SELECTED
        } else {
            theme::TEXT
        },
    );
    if pill_resp.clicked() {
        state.push_command(Intent::SetPhoneStripZoom(next_stop(stop)));
    }

    // Gestures. A press on the cap or a failure mark belongs to it.
    let owned = |p: Pos2| {
        cap.is_some_and(|r| r.contains(p)) || failed_ticks.iter().any(|t| t.rect.contains(p))
    };
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
    });
    ui.ctx().memory_mut(|m| m.data.insert_temp(key, touch));
    chrome.phone_strip_claims_touch |= out.claimed;
    if let Some(next) = out.settle {
        state.push_command(Intent::SetPhoneStripZoom(next));
    }
    if out.press.waiting || out.claimed {
        ui.ctx().request_repaint();
    }
    if let Some((x, _)) = out.press.long_press {
        if let Some(scan_start) = frame.view.scan_start_at(frame.x_to_ts(x)) {
            chrome.scan_inspector = Some(scan_start);
        }
        return;
    }
    state.pointer_scrub_active = response.dragged() && out.may_seek;
    if !out.may_seek {
        return;
    }
    if let Some(p) = pos.filter(|p| !owned(*p)) {
        let units = StripUnits::from_containers(&containers).with_listed(&sizes.scans);
        let ts = snap_scrub(stop, frame.x_to_ts(p.x), &units, now);
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

/// The Day stop: one storm bar per volume, as tall as its size on the scale
/// of the day in view, standing on the on-device underline; days not listed
/// yet show their coverage as a faint wash. The ring marks the volume of the
/// frame on screen.
fn paint_day(
    painter: &egui::Painter,
    frame: &TimelineFrame<'_>,
    timeline: &crate::subsystem::Timeline,
    sizes: &ScanSizes,
    containers: &[crate::core::ScanContainer],
    now: f64,
    playhead: f64,
) {
    let track = frame.rects.scan;
    let bars = Rect::from_min_max(
        Pos2::new(track.left(), track.top() + 4.0),
        Pos2::new(track.right(), track.bottom() - UNDERLINE_H - 1.0),
    );
    let column = |start: f64, end: f64, height: f32, color: egui::Color32| {
        let x0 = frame.ts_to_x(start);
        let x1 = frame.ts_to_x(end).max(x0 + 1.0);
        if x1 < bars.left() || x0 > bars.right() {
            return;
        }
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, bars.bottom() - height),
                Pos2::new(x1, bars.bottom()),
            ),
            0.0,
            color,
        );
    };
    let listed = containers
        .iter()
        .filter(|c| !c.is_projected)
        .map(|c| (c.start_secs, c.end_secs));
    for (start, end) in sizes.unknown_pieces(listed) {
        column(start, end, bars.height(), tl_colors::available_fill());
    }
    for (scan, frac) in sizes.bars(frame.view_start, frame.view_end) {
        column(
            scan.start,
            scan.end,
            bars.height() * frac,
            tl_colors::activity_fill(),
        );
    }

    // What is held on this device, the overview's underline.
    let held: Vec<(f64, f64)> = timeline
        .scans
        .time_ranges()
        .iter()
        .map(|r| (r.start, r.end))
        .collect();
    let marks =
        crate::core::overview::OverviewMarks::build(&crate::core::overview::OverviewMarkInputs {
            start: frame.view_start,
            end: frame.view_end,
            px_per_sec: frame.zoom,
            held: &held,
            events: &[],
            outage_days: &[],
            now,
            streaming: false,
            playhead,
            playhead_at_now: false,
            range: None,
            range_pinned: false,
        });
    for (s, e) in marks.held {
        let x0 = frame.ts_to_x(s).max(track.left());
        let x1 = frame.ts_to_x(e).min(track.right()).max(x0 + 1.0);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, track.bottom() - UNDERLINE_H),
                Pos2::new(x1, track.bottom()),
            ),
            0.0,
            tl_colors::HELD,
        );
    }

    // The ring on the volume of the frame on screen.
    if let Some(c) = containers
        .iter()
        .find(|c| c.cells.iter().any(|cell| cell.is_active))
    {
        let x0 = frame.ts_to_x(c.start_secs);
        let x1 = frame.ts_to_x(c.end_secs).max(x0 + 3.0);
        painter.rect_stroke(
            Rect::from_min_max(Pos2::new(x0, bars.top()), Pos2::new(x1, bars.bottom())),
            1.0,
            Stroke::new(2.0_f32, tl_colors::ACTIVE_SWEEP),
            StrokeKind::Outside,
        );
    }
}
