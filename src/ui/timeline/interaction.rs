//! Timeline interaction: click, shift+click, drag-to-pan, scroll-to-zoom.

use crate::state::{AppState, LiveExitReason};
use eframe::egui::{self, Rect};

/// Handle mouse interaction on the timeline: click, shift+click, drag-to-pan, scroll-to-zoom.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_timeline_interaction(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    response: &egui::Response,
    full_rect: &Rect,
    view_start: f64,
    zoom: f64,
) {
    let shift_held = ui.input(|i| i.modifiers.shift);

    if shift_held && response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let clicked_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
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
            let drag_start_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
            playback.state.selection_start = Some(drag_start_ts);
            playback.state.selection_end = Some(drag_start_ts);
            playback.state.selection_in_progress = true;
        }
    }

    if shift_held && response.dragged() && playback.state.selection_in_progress {
        if let Some(pos) = response.interact_pointer_pos() {
            let current_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
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
            if live.mode_state.is_active() {
                live.mode_state.stop(LiveExitReason::UserSeeked);
                playback.state.time_model.disable_realtime_lock();
                state.status_message = live
                    .mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }

            let clicked_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;

            playback.state.set_playback_position(clicked_ts);
            playback.state.clear_selection();

            if let Some(frame) = playback.state.timestamp_to_frame(clicked_ts as i64) {
                playback.state.current_frame = frame;
            }
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
            let new_zoom = (old_zoom * zoom_factor).clamp(0.000001, 1000.0);

            if let Some(cursor_pos) = response.hover_pos() {
                let cursor_ts = view_start + (cursor_pos.x - full_rect.left()) as f64 / old_zoom;
                let new_view_start =
                    cursor_ts - (cursor_pos.x - full_rect.left()) as f64 / new_zoom;
                playback.state.timeline_view_start = new_view_start;
            }

            playback.state.timeline_zoom = new_zoom;
        }
    }
}
