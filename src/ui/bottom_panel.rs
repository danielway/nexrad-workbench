//! Bottom panel UI: orchestrates the timeline, playback controls, and session statistics.

use super::acquisition_drawer::render_acquisition_drawer;
use super::colors::{live, ui as ui_colors};
use crate::state::{AppState, LiveExitReason};
use eframe::egui::{self, RichText};

use super::playback_controls::render_playback_controls;
use super::timeline::render_timeline;

pub fn render_bottom_panel(ctx: &egui::Context, state: &mut AppState) {
    let dt = ctx.input(|i| i.stable_dt);

    // Update live mode pulse animation
    state.live_mode_state.update_pulse(dt);

    // Handle spacebar to toggle playback (only when no text input is focused)
    let space_pressed = ctx.input(|i| i.key_pressed(egui::Key::Space) && !i.modifiers.any());
    let has_focus = ctx.memory(|m| m.focused().is_some());
    if space_pressed && !has_focus {
        if state.playback_state.playing {
            // Stop - also exits live mode if active
            if state.live_mode_state.is_active() {
                state.live_mode_state.stop(LiveExitReason::UserStopped);
                state.playback_state.time_model.disable_realtime_lock();
                state.status_message = state
                    .live_mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }
            state.playback_state.playing = false;
        } else {
            // Only allow playback if zoom permits
            if state.playback_state.is_playback_allowed() {
                state.playback_state.playing = true;
            }
        }
    }

    // Advance playback position when playing
    // The time_model handles real-time lock mode internally
    if state.playback_state.playing {
        state.playback_state.advance(dt as f64);

        // Pin playback position on the visible timeline during playback.
        // In live/real-time mode, pin at 75% from left (right quarter) so more
        // history is visible. In archive playback, pin at 25% from left.
        let view_width_secs = state.playback_state.view_width_secs();
        if view_width_secs > 0.0 {
            let pin_fraction = if state.live_mode_state.is_active() {
                0.75
            } else {
                0.25
            };
            let target_offset = view_width_secs * pin_fraction;
            let pos = state.playback_state.playback_position();
            state.playback_state.timeline_view_start = pos - target_offset;
        }

        // Request continuous repaint while playing
        ctx.request_repaint();
    }

    let drawer_expanded = state.acquisition.drawer_expanded;
    let controls_height = 104.0;
    let top_bar_height = 36.0;
    let min_central_height = 100.0;
    let max_panel_height =
        ctx.input(|i| i.viewport_rect().height()) - top_bar_height - min_central_height;

    // When the drawer is expanded, a resize handle, separator, and inter-widget
    // spacing are rendered between the drawer and the controls. Account for that
    // overhead so the controls aren't pushed below the window edge.
    let drawer_spacing_overhead = 14.0;
    let drawer_height = if drawer_expanded {
        let max_drawer =
            (max_panel_height - controls_height - drawer_spacing_overhead).max(0.0);
        state.acquisition.drawer_height.min(max_drawer)
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
                    state.acquisition.drawer_height =
                        (state.acquisition.drawer_height + delta).clamp(100.0, 600.0);
                }
                if resize_response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }

                render_acquisition_drawer(ui, state, drawer_height - 4.0);
                ui.separator();
            }

            ui.vertical(|ui| {
                // Mode and acquisition status bar
                ui.horizontal(|ui| {
                    let mode_label = if state.live_mode_state.is_active() {
                        "REAL-TIME"
                    } else {
                        "NAVIGATE"
                    };
                    let mode_color = if state.live_mode_state.is_active() {
                        live::STREAMING
                    } else {
                        ui_colors::label(state.is_dark)
                    };
                    ui.label(
                        RichText::new(mode_label)
                            .size(10.0)
                            .strong()
                            .color(mode_color),
                    );

                    // Show data staleness if available
                    if let Some(end_staleness) = state.viz_state.data_staleness_secs {
                        ui.separator();
                        let format_compact = |secs: f64| -> String {
                            if secs < 60.0 {
                                format!("{:.0}s", secs)
                            } else if secs < 3600.0 {
                                format!("{:.0}m", secs / 60.0)
                            } else if secs < 86400.0 {
                                format!("{:.1}h", secs / 3600.0)
                            } else if secs < 86400.0 * 365.0 {
                                format!("{:.0}d", secs / 86400.0)
                            } else {
                                format!("{:.1}y", secs / (86400.0 * 365.25))
                            }
                        };
                        let age_text = if end_staleness < 300.0 {
                            if let Some(start_staleness) = state.viz_state.data_staleness_start_secs
                            {
                                format!(
                                    "{}–{} old",
                                    format_compact(start_staleness),
                                    format_compact(end_staleness),
                                )
                            } else {
                                format!("{} old", format_compact(end_staleness))
                            }
                        } else {
                            format!("{} old", format_compact(end_staleness))
                        };
                        let age_color = if end_staleness < 60.0 {
                            ui_colors::SUCCESS
                        } else if end_staleness < 300.0 {
                            ui_colors::ACTIVE
                        } else {
                            egui::Color32::from_rgb(220, 80, 80)
                        };
                        ui.label(RichText::new(age_text).size(10.0).color(age_color));
                    }
                });

                // Timeline row
                ui.add_space(2.0);
                render_timeline(ui, state);

                ui.add_space(2.0);

                // Playback controls row
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    render_playback_controls(ui, state);
                });
            });
        });

    // Stats detail is now a proper modal rendered from main.rs via render_stats_modal.
}
