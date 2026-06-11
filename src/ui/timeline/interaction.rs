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
            playback.state.set_selection(current_pos, clicked_ts);
            maybe_anchor_to_live(live, playback, frame.now_secs);
            playback.state.apply_selection_as_bounds();
            if let Some(range) = playback.state.selection_range() {
                state.selection_just_finalized = Some(range);
            }
            let duration_mins = (clicked_ts - current_pos).abs() / 60.0;
            log::debug!("Shift+click range: {:.0} minutes", duration_mins);
        }
    }

    if shift_held && response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            playback.state.begin_selection_drag(frame.x_to_ts(pos.x));
        }
    }

    if shift_held && response.dragged() && playback.state.selection_in_progress() {
        if let Some(pos) = response.interact_pointer_pos() {
            playback.state.update_selection_drag(frame.x_to_ts(pos.x));
        }
    }

    if response.drag_stopped() && playback.state.end_selection_drag() {
        maybe_anchor_to_live(live, playback, frame.now_secs);
        playback.state.apply_selection_as_bounds();
        if let Some((start, end)) = playback.state.selection_range() {
            state.selection_just_finalized = Some((start, end));
            log::debug!("Selected time range: {:.0} minutes", (end - start) / 60.0);
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

            // Selection survival: a click *inside* the selection seeks within
            // it (loop bounds intact); a click outside clears it. Pan/zoom
            // never clear.
            let inside = playback.state.selection_contains(clicked_ts);
            playback.state.set_playback_position(clicked_ts);
            if !inside {
                playback.state.clear_selection();
            }
        }
    }

    // Drag to pan
    if response.dragged() && !shift_held && !playback.state.selection_in_progress() {
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

            // Zooming out past the micro threshold while attached to the
            // live edge is a browsing gesture: detach (the stream keeps
            // running) instead of silently clamping the zoom. No hidden
            // zoom floor remains.
            let attached =
                playback.state.time_model.is_pinned() || playback.state.time_model.is_lookback();
            if attached && new_zoom < crate::state::MICRO_ZOOM_THRESHOLD {
                live.detach_playhead(&mut playback.state, state.frame_now.secs());
            }

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

/// Implicit live-anchoring: a fresh selection whose later edge lands within
/// one volume of "now" while streaming reads as "from then up to now" — make
/// it follow the live edge as new data arrives. No extra chrome needed.
fn maybe_anchor_to_live(
    live: &crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    now_secs: f64,
) {
    if !live.mode_state.is_active() {
        return;
    }
    let near_now = playback
        .state
        .selection_range()
        .is_some_and(|(_, end)| (now_secs - end).abs() < crate::FALLBACK_SCAN_DURATION_SECS as f64);
    if near_now {
        playback.state.anchor_selection_to_live();
    }
}
