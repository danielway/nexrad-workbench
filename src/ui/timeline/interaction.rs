//! Timeline interaction: click, shift+click, drag-to-pan, scroll-to-zoom.

use crate::state::AppState;
use eframe::egui::{self, Rect};

/// Handle mouse interaction on the timeline: click, shift+click, drag-to-pan, scroll-to-zoom.
pub(super) fn handle_timeline_interaction(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    response: &egui::Response,
    frame: &super::TimelineFrame<'_>,
    now_rect: Option<Rect>,
) {
    let zoom = frame.zoom;
    let shift_held = ui.input(|i| i.modifiers.shift);

    if shift_held && response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let clicked_ts = frame.x_to_ts(pos.x);
            let current_pos = playback.state.playback_position();
            playback.state.selection_start = Some(current_pos);
            playback.state.selection_end = Some(clicked_ts);
            playback.state.apply_selection_as_bounds();
            let duration_mins = (clicked_ts - current_pos).abs() / 60.0;
            log::debug!("Shift+click range: {:.0} minutes", duration_mins);
        }
    }

    if shift_held && response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let drag_start_ts = frame.x_to_ts(pos.x);
            playback.state.selection_start = Some(drag_start_ts);
            playback.state.selection_end = Some(drag_start_ts);
            playback.state.selection_in_progress = true;
        }
    }

    if shift_held && response.dragged() && playback.state.selection_in_progress {
        if let Some(pos) = response.interact_pointer_pos() {
            let current_ts = frame.x_to_ts(pos.x);
            playback.state.selection_end = Some(current_ts);
        }
    }

    if response.drag_stopped() && playback.state.selection_in_progress {
        playback.state.selection_in_progress = false;
        if let Some((start, end)) = playback.state.selection_range() {
            let duration_mins = (end - start) / 60.0;
            log::debug!("Selected time range: {:.0} minutes", duration_mins);
            playback.state.apply_selection_as_bounds();
        }
    }

    if response.clicked() && !shift_held {
        if let Some(pos) = response.interact_pointer_pos() {
            // Clicks on the now affordance (the live-edge cap or the off-screen
            // chip) are owned by `now_edge` — never also treat them as a seek.
            if now_rect.is_some_and(|r| r.contains(pos)) {
                return;
            }

            let clicked_ts = frame.x_to_ts(pos.x);

            // Seeking while live detaches the playhead but keeps the stream
            // ingesting — the now-cap flips to "return to live" and the
            // timeline keeps growing at the right edge.
            live.detach_playhead(&mut playback.state, state.frame_now.secs());

            playback.state.set_playback_position(clicked_ts);
            playback.state.clear_selection();
        }
    }

    // Drag to pan
    if response.dragged() && !shift_held && !playback.state.selection_in_progress {
        let delta_secs = -response.drag_delta().x as f64 / zoom;
        playback.state.timeline_view_start += delta_secs;
    }

    // Scroll wheel zoom
    if response.hovered() {
        let scroll_delta = ui.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = 1.0 + scroll_delta.y as f64 * 0.002;
            let old_zoom = playback.state.timeline_zoom;

            // While the playhead is attached to the live edge (pinned or
            // replaying), don't let the user zoom out into macro mode — the
            // live stream is about individual sweeps/chunks, which only make
            // sense at micro zoom. A detached playhead zooms freely.
            let realtime =
                playback.state.time_model.is_pinned() || playback.state.time_model.is_lookback();
            let min_zoom = if realtime {
                crate::state::MICRO_ZOOM_THRESHOLD
            } else {
                0.000001
            };
            let new_zoom = (old_zoom * zoom_factor).clamp(min_zoom, 1000.0);

            if let Some(cursor_pos) = response.hover_pos() {
                let cursor_ts = frame.x_to_ts(cursor_pos.x);
                let new_view_start =
                    cursor_ts - (cursor_pos.x - frame.rects.scan.left()) as f64 / new_zoom;
                playback.state.timeline_view_start = new_view_start;
            }

            playback.state.timeline_zoom = new_zoom;
        }
    }
}
