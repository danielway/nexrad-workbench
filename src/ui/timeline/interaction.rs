//! Timeline interaction: click, shift+click, drag-to-pan, scroll-to-zoom.

use crate::state::{AppState, LiveExitReason, MICRO_ZOOM_THRESHOLD};
use eframe::egui::{self, Rect};

/// Handle mouse interaction on the timeline: click, shift+click, drag-to-pan, scroll-to-zoom.
pub(super) fn handle_timeline_interaction(
    ui: &mut egui::Ui,
    state: &mut AppState,
    response: &egui::Response,
    full_rect: &Rect,
    view_start: f64,
    zoom: f64,
) {
    let shift_held = ui.input(|i| i.modifiers.shift);

    if shift_held && response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let clicked_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
            let current_pos = state.playback_state.playback_position();
            state.playback_state.selection_start = Some(current_pos);
            state.playback_state.selection_end = Some(clicked_ts);
            state.playback_state.apply_selection_as_bounds();
            let duration_mins = (clicked_ts - current_pos).abs() / 60.0;
            log::debug!("Shift+click range: {:.0} minutes", duration_mins);
        }
    }

    if shift_held && response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let drag_start_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
            state.playback_state.selection_start = Some(drag_start_ts);
            state.playback_state.selection_end = Some(drag_start_ts);
            state.playback_state.selection_in_progress = true;
        }
    }

    if shift_held && response.dragged() && state.playback_state.selection_in_progress {
        if let Some(pos) = response.interact_pointer_pos() {
            let current_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
            state.playback_state.selection_end = Some(current_ts);
        }
    }

    if response.drag_stopped() && state.playback_state.selection_in_progress {
        state.playback_state.selection_in_progress = false;
        if let Some((start, end)) = state.playback_state.selection_range() {
            let duration_mins = (end - start) / 60.0;
            log::debug!("Selected time range: {:.0} minutes", duration_mins);
            state.playback_state.apply_selection_as_bounds();
        }
    }

    if response.clicked() && !shift_held {
        if let Some(pos) = response.interact_pointer_pos() {
            if state.live_mode_state.is_active() {
                state.live_mode_state.stop(LiveExitReason::UserSeeked);
                state.playback_state.time_model.disable_realtime_lock();
                state.status_message = state
                    .live_mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }

            let clicked_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;

            state.playback_state.set_playback_position(clicked_ts);
            state.playback_state.clear_selection();

            if let Some(frame) = state.playback_state.timestamp_to_frame(clicked_ts as i64) {
                state.playback_state.current_frame = frame;
            }
        }
    }

    // Drag to pan
    if response.dragged() && !shift_held && !state.playback_state.selection_in_progress {
        let delta_secs = -response.drag_delta().x as f64 / zoom;
        state.playback_state.timeline_view_start += delta_secs;
    }

    // Scroll wheel zoom
    if response.hovered() {
        let scroll_delta = ui.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = 1.0 + scroll_delta.y as f64 * 0.002;
            let old_zoom = state.playback_state.timeline_zoom;
            // In live mode, never let the user zoom out past micro-mode — they
            // must be able to see individual sweeps and chunks.
            let min_zoom = if state.live_mode_state.is_active() {
                MICRO_ZOOM_THRESHOLD
            } else {
                0.000001
            };
            let new_zoom = (old_zoom * zoom_factor).clamp(min_zoom, 1000.0);

            if let Some(cursor_pos) = response.hover_pos() {
                let cursor_ts = view_start + (cursor_pos.x - full_rect.left()) as f64 / old_zoom;
                let new_view_start =
                    cursor_ts - (cursor_pos.x - full_rect.left()) as f64 / new_zoom;
                state.playback_state.timeline_view_start = new_view_start;
            }

            state.playback_state.timeline_zoom = new_zoom;
        }
    }
}
