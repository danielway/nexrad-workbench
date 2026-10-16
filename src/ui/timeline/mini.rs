//! The phone's mini timeline (one-instrument §13): a small timeline with
//! the desktop strip's real cells, marks, playhead and now cap, replacing the
//! old bare coverage scrubber.
//!
//! Its window is [`crate::core::phone::mini_window`]: pinned, the last three
//! volumes, the ring on the frame on screen, the now line with its cap and
//! the ghost of the next volume. A tap seeks (`Intent::Seek`, Press), a drag
//! scrubs (the same reducer, synchronously, like the desktop strip) and a
//! long-press opens the scan inspector ([`crate::core::phone::PressTrack`]).
//! Everything drawn comes from the same core projections the desktop strip
//! paints.

use super::frame_cells::{paint_container, CellAnim};
use super::marks::paint_strip_marks;
use super::now_edge::render_now_affordance;
use super::ruler::render_playback_cursor;
use super::{reduced_motion, style, TimelineFrame, TrackRects};
use crate::core::phone::PressTrack;
use crate::core::transport::SeekGesture;
use crate::core::{FrameJoinInputs, Intent, TimelineTier};
use crate::state::AppState;
use crate::ui::theme::timeline as tl_colors;
use eframe::egui::{self, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

/// The mini timeline's height: the desktop strip's main track.
pub(crate) const MINI_TIMELINE_H: f32 = style::MAIN_TRACK_H;

/// How far back the window's volumes are looked for.
const LOOKBACK_SECS: f64 = 6.0 * 3600.0;

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
    let (response, painter) =
        ui.allocate_painter(Vec2::new(width, MINI_TIMELINE_H), Sense::click_and_drag());
    let track = response.rect;
    painter.rect_filled(track, 2.0, tl_colors::BACKGROUND);
    painter.rect_stroke(
        track,
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
    let look_from = now.min(playhead) - LOOKBACK_SECS;
    let all = view.frame_containers_in_range(look_from, now.max(playhead) + 3600.0, join);
    let starts: Vec<f64> = all.iter().map(|c| c.start_secs).collect();
    let window = crate::core::phone::mini_window(now, playhead, pinned, &starts);
    let zoom = width as f64 / window.span().max(1.0);

    let frame = TimelineFrame {
        view,
        now_secs: now,
        view_start: window.start,
        view_end: window.end,
        zoom,
        rects: TrackRects {
            tick: Rect::from_min_size(track.min, Vec2::new(width, 0.0)),
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
    let countdown = live.countdown_remaining_secs(now);
    let countdown_idx = crate::core::timeline_view::countdown_container(&containers, now);
    let clipped = painter.with_clip_rect(track);
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
    let marks = crate::core::timeline_view::strip_marks(&containers, zoom, TimelineTier::Micro);
    let failed_ticks = paint_strip_marks(&clipped, &frame, &marks, None);
    if !reduced && (state.download_progress.is_active() || frame.view.live_volume().is_some()) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
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

    // Gestures. A press on the cap or a failure mark belongs to it.
    let owned = |p: Pos2| {
        cap.is_some_and(|r| r.contains(p)) || failed_ticks.iter().any(|t| t.rect.contains(p))
    };
    let key = response.id.with("mini_press");
    let prev: Option<PressTrack> = ui.ctx().memory(|m| m.data.get_temp(key));
    let down = response.is_pointer_button_down_on();
    let pos = response.interact_pointer_pos();
    let (track_now, out) = PressTrack::step(
        prev,
        down,
        pos.map(|p| (p.x, p.y)),
        ui.ctx().input(|i| i.time),
    );
    ui.ctx().memory_mut(|m| match track_now {
        Some(t) => {
            m.data.insert_temp(key, t);
        }
        None => m.data.remove::<PressTrack>(key),
    });
    if out.waiting {
        ui.ctx().request_repaint();
    }
    if let Some((x, _)) = out.long_press {
        if let Some(scan_start) = frame.view.scan_start_at(frame.x_to_ts(x)) {
            chrome.scan_inspector = Some(scan_start);
        }
        return;
    }
    let long_pressed = track_now.is_some_and(|t| t.fired()) || out.swallow_release;
    if long_pressed {
        return;
    }
    if let Some(p) = pos.filter(|p| !owned(*p)) {
        let ts = frame.x_to_ts(p.x);
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
    state.pointer_scrub_active = response.dragged();

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
