//! Bottom panel UI: orchestrates the timeline, playback controls, and session statistics.

use super::acquisition_drawer::render_acquisition_drawer;
use crate::state::{AppState, LiveExitReason, PlaybackMode, PlaybackSpeed};
use crate::subsystem::Acquisition;
use eframe::egui;

use super::playback_controls::render_playback_controls;
use super::timeline::render_timeline;

/// Desktop bottom panel: timeline + playback controls + session stats.
///
/// Only rendered when not in mobile layout — the mobile chrome owns the
/// bottom region. The per-frame pulse-animation tick is hoisted into the
/// main update loop so it runs in both layouts without this function
/// needing to be called as a side-effect carrier on mobile.
pub fn render_bottom_panel(
    ctx: &egui::Context,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    acquisition: &mut Acquisition,
    chrome: &mut crate::subsystem::Chrome,
) {
    let dt = ctx.input(|i| i.stable_dt);

    // Handle spacebar to toggle playback (only when no text input is focused)
    let space_pressed = ctx.input(|i| i.key_pressed(egui::Key::Space) && !i.modifiers.any());
    let has_focus = ctx.memory(|m| m.focused().is_some());
    if space_pressed && !has_focus {
        if playback.state.playing {
            // Stop - also exits live mode if active
            if live.mode_state.is_active() {
                live.mode_state.stop(LiveExitReason::UserStopped);
                playback.state.time_model.disable_realtime_lock();
                state.status_message = live
                    .mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }
            playback.state.playing = false;
        } else {
            // Only allow playback if zoom permits
            if playback.state.is_playback_allowed() {
                playback.state.playing = true;
            }
        }
    }

    // Advance playback position when playing
    // The time_model handles real-time lock mode internally
    if playback.state.playing {
        let mode = playback.state.playback_mode();
        let was_macro = playback.state.macro_playback.was_macro;

        // Detect mode transitions and auto-adjust speed
        if mode == PlaybackMode::Macro && !was_macro {
            // Entering macro: promote disallowed speeds to nearest macro speed
            if playback.state.speed.macro_frames_per_second().is_none() {
                playback.state.speed = PlaybackSpeed::Quarter;
            }
            // Reset accumulator on transition
            playback.state.macro_playback.frame_accumulator = 0.0;
        } else if mode == PlaybackMode::Micro && was_macro {
            // Entering micro: map fps-based speed to a reasonable timeline speed.
            // Keep the same PlaybackSpeed variant — the labels just change.
        }
        playback.state.macro_playback.was_macro = mode == PlaybackMode::Macro;

        match mode {
            PlaybackMode::Micro => playback.state.advance(dt as f64),
            PlaybackMode::Macro => playback.state.advance_macro(dt as f64),
        }

        // In real-time streaming mode, keep the playhead on-screen. Pan/zoom is
        // otherwise free; this only fires when "now" would fall outside the
        // visible range, scrolling the view minimally to put it at the edge.
        if live.mode_state.is_active() {
            let view_width = playback.state.view_width_secs();
            if view_width > 0.0 {
                let now = playback.state.playback_position();
                let view_start = playback.state.timeline_view_start;
                let view_end = view_start + view_width;
                if now > view_end {
                    playback.state.timeline_view_start = now - view_width;
                } else if now < view_start {
                    playback.state.timeline_view_start = now;
                }
            }
        }

        // Repaint at 30 FPS while playing — smooth for continuous micro-mode
        // advances and well above the 1–15 FPS frame cadence macro mode emits.
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    // The acquisition drawer is reachable only via the dev-mode-only network
    // metric label. Force it closed when dev mode is off so a previously
    // expanded drawer doesn't linger after the user toggles dev off.
    let drawer_expanded = state.dev_mode && acquisition.state.drawer_expanded;
    // Sized to the new content layout (timeline 53 + spacing + controls
    // ~24 + frame margins). Stays constant across macro/micro because
    // the timeline locks its own height in render_timeline.
    let controls_height = 88.0;
    let top_bar_height = 36.0;
    let min_central_height = 100.0;
    let max_panel_height =
        ctx.input(|i| i.viewport_rect().height()) - top_bar_height - min_central_height;

    // When the drawer is expanded, a resize handle, separator, and inter-widget
    // spacing are rendered between the drawer and the controls. Account for that
    // overhead so the controls aren't pushed below the window edge.
    let drawer_spacing_overhead = 14.0;
    let drawer_height = if drawer_expanded {
        let max_drawer = (max_panel_height - controls_height - drawer_spacing_overhead).max(0.0);
        acquisition.state.drawer_height.min(max_drawer)
    } else {
        0.0
    };
    let total_height = if drawer_expanded {
        controls_height + drawer_spacing_overhead + drawer_height
    } else {
        controls_height
    };

    egui::TopBottomPanel::bottom("bottom_panel")
        .exact_height(total_height)
        .show(ctx, |ui| {
            // Render acquisition drawer above normal controls when expanded
            if drawer_expanded {
                // Resize handle: thin draggable strip
                let resize_response = ui.allocate_response(
                    egui::Vec2::new(ui.available_width(), 4.0),
                    egui::Sense::drag(),
                );
                if resize_response.dragged() {
                    // Dragging up increases height, dragging down decreases
                    let delta = -resize_response.drag_delta().y;
                    acquisition.state.drawer_height =
                        (acquisition.state.drawer_height + delta).clamp(100.0, 600.0);
                }
                if resize_response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }

                render_acquisition_drawer(
                    ui,
                    state,
                    live,
                    acquisition,
                    drawer_height - 4.0,
                    chrome,
                );
                ui.separator();
            }

            ui.vertical(|ui| {
                // Timeline row
                render_timeline(ui, state, timeline, live, playback, chrome);

                ui.add_space(2.0);

                // Playback controls row
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    render_playback_controls(
                        ui,
                        state,
                        timeline,
                        live,
                        playback,
                        acquisition,
                        chrome,
                    );
                });
            });
        });

    // Stats detail is now a proper modal rendered from main.rs via render_stats_modal.
}
