//! Centralized keyboard shortcut handling and help overlay.

use crate::state::{AppState, LiveExitReason, PlaybackSpeed, RadarProduct, ViewMode};
use eframe::egui::{self, RichText};

/// Shortcut definition for display in the help overlay.
struct Shortcut {
    key: &'static str,
    description: &'static str,
}

const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        key: "Space",
        description: "Play / Pause",
    },
    Shortcut {
        key: "[",
        description: "Step backward",
    },
    Shortcut {
        key: "]",
        description: "Step forward",
    },
    Shortcut {
        key: "-",
        description: "Decrease playback speed",
    },
    Shortcut {
        key: "=",
        description: "Increase playback speed",
    },
    Shortcut {
        key: "L",
        description: "Toggle live mode",
    },
    Shortcut {
        key: "1",
        description: "Toggle left sidebar",
    },
    Shortcut {
        key: "2",
        description: "Toggle right sidebar",
    },
    Shortcut {
        key: "P",
        description: "Cycle product",
    },
    Shortcut {
        key: "E",
        description: "Cycle elevation up",
    },
    Shortcut {
        key: "S",
        description: "Open site selection",
    },
    Shortcut {
        key: "G",
        description: "Toggle globe / flat map",
    },
    Shortcut {
        key: "C",
        description: "Cycle camera mode (3D)",
    },
    Shortcut {
        key: "N",
        description: "Reset view (re-center, level, North up)",
    },
    Shortcut {
        key: "?",
        description: "Toggle this help overlay",
    },
];

/// Process keyboard shortcuts. Call once per frame from the main update loop.
pub fn handle_shortcuts(ctx: &egui::Context, state: &mut AppState) {
    // Skip shortcut processing when a text field has focus
    let has_focus = ctx.memory(|m| m.focused().is_some());
    if has_focus {
        return;
    }

    ctx.input(|i| {
        // Note: Space is handled directly in bottom_panel.rs for play/pause
        // to keep the existing integration with live mode logic.

        // [ — Step backward
        if i.key_pressed(egui::Key::OpenBracket) && !i.modifiers.any() {
            // Will be applied below
        }

        // ] — Step forward
        if i.key_pressed(egui::Key::CloseBracket) && !i.modifiers.any() {
            // Will be applied below
        }
    });

    // Re-read input for actual state mutations (can't mutate state inside ctx.input closure)
    let step_back = ctx.input(|i| i.key_pressed(egui::Key::OpenBracket) && !i.modifiers.any());
    let step_fwd = ctx.input(|i| i.key_pressed(egui::Key::CloseBracket) && !i.modifiers.any());
    let speed_down = ctx.input(|i| i.key_pressed(egui::Key::Minus) && !i.modifiers.any());
    let speed_up = ctx.input(|i| i.key_pressed(egui::Key::Equals) && !i.modifiers.any());
    let toggle_live = ctx.input(|i| i.key_pressed(egui::Key::L) && !i.modifiers.any());
    let toggle_left = ctx.input(|i| i.key_pressed(egui::Key::Num1) && !i.modifiers.any());
    let toggle_right = ctx.input(|i| i.key_pressed(egui::Key::Num2) && !i.modifiers.any());
    let cycle_product = ctx.input(|i| i.key_pressed(egui::Key::P) && !i.modifiers.any());
    let cycle_elevation = ctx.input(|i| i.key_pressed(egui::Key::E) && !i.modifiers.any());
    let open_site = ctx.input(|i| i.key_pressed(egui::Key::S) && !i.modifiers.any());
    let toggle_globe = ctx.input(|i| i.key_pressed(egui::Key::G) && !i.modifiers.any());
    let cycle_camera = ctx.input(|i| i.key_pressed(egui::Key::C) && !i.modifiers.any());
    let reset_north = ctx.input(|i| i.key_pressed(egui::Key::N) && !i.modifiers.any());
    let toggle_help = ctx.input(|i| {
        // ? requires Shift on most layouts
        i.key_pressed(egui::Key::Questionmark)
            || (i.key_pressed(egui::Key::Slash) && i.modifiers.shift)
    });

    // Skip if focus is held
    if ctx.memory(|m| m.focused().is_some()) {
        return;
    }

    let current_pos = state.playback_state.playback_position();
    let target_elev = state.viz_state.target_elevation;
    let jog_fallback = state
        .playback_state
        .speed
        .timeline_seconds_per_real_second();
    const ELEV_TOLERANCE: f32 = 0.3;

    if step_back {
        exit_live_if_active(state, LiveExitReason::UserJogged);
        let new_pos = state
            .radar_timeline
            .prev_matching_sweep_end(current_pos, target_elev, ELEV_TOLERANCE)
            .unwrap_or(current_pos - jog_fallback);
        state.playback_state.set_playback_position(new_pos);
    }

    if step_fwd {
        exit_live_if_active(state, LiveExitReason::UserJogged);
        let new_pos = state
            .radar_timeline
            .next_matching_sweep_end(current_pos, target_elev, ELEV_TOLERANCE)
            .unwrap_or(current_pos + jog_fallback);
        state.playback_state.set_playback_position(new_pos);
    }

    if speed_down {
        let speeds = PlaybackSpeed::all();
        if let Some(idx) = speeds.iter().position(|s| *s == state.playback_state.speed) {
            if idx > 0 {
                state.playback_state.speed = speeds[idx - 1];
            }
        }
    }

    if speed_up {
        let speeds = PlaybackSpeed::all();
        if let Some(idx) = speeds.iter().position(|s| *s == state.playback_state.speed) {
            if idx + 1 < speeds.len() {
                state.playback_state.speed = speeds[idx + 1];
            }
        }
    }

    if toggle_live {
        if state.live_mode_state.is_active() {
            state.live_mode_state.stop(LiveExitReason::UserStopped);
            state.playback_state.time_model.disable_realtime_lock();
            state.playback_state.playing = false;
            state.status_message = state
                .live_mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        } else {
            state.start_live_requested = true;
            state.playback_state.speed = PlaybackSpeed::Realtime;
        }
    }

    if toggle_left {
        state.left_sidebar_visible = !state.left_sidebar_visible;
    }

    if toggle_right {
        state.right_sidebar_visible = !state.right_sidebar_visible;
    }

    if cycle_product {
        let products = RadarProduct::all();
        if let Some(idx) = products.iter().position(|p| *p == state.viz_state.product) {
            state.viz_state.product = products[(idx + 1) % products.len()];
        }
    }

    if cycle_elevation {
        // Increment target elevation by one step (0.5 degrees), wrapping
        let new_elev = state.viz_state.target_elevation + 0.5;
        state.viz_state.target_elevation = if new_elev > 19.5 { 0.5 } else { new_elev };
    }

    if open_site {
        state.site_modal_open = true;
    }

    if toggle_globe {
        state.viz_state.view_mode = match state.viz_state.view_mode {
            ViewMode::Flat2D => ViewMode::Globe3D,
            ViewMode::Globe3D => ViewMode::Flat2D,
        };
    }

    if cycle_camera && state.viz_state.view_mode == ViewMode::Globe3D {
        state.viz_state.camera.mode = state.viz_state.camera.mode.next();
    }

    if reset_north && state.viz_state.view_mode == ViewMode::Globe3D {
        state.viz_state.camera.recenter();
    }

    if toggle_help {
        state.shortcuts_help_visible = !state.shortcuts_help_visible;
    }
}

/// Render the keyboard shortcut help overlay.
pub fn render_shortcuts_help(ctx: &egui::Context, state: &mut AppState) {
    if !state.shortcuts_help_visible {
        return;
    }

    let popup_id = egui::Id::new("shortcuts_help_overlay");

    egui::Area::new(popup_id)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(16.0)
                .show(ui, |ui| {
                    ui.set_min_width(300.0);
                    ui.set_max_width(400.0);

                    ui.horizontal(|ui| {
                        ui.heading("Keyboard Shortcuts");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(egui_phosphor::regular::X).clicked() {
                                state.shortcuts_help_visible = false;
                            }
                        });
                    });

                    ui.separator();

                    egui::Grid::new("shortcuts_grid")
                        .num_columns(2)
                        .spacing([20.0, 6.0])
                        .show(ui, |ui| {
                            for shortcut in SHORTCUTS {
                                ui.label(RichText::new(shortcut.key).monospace().strong());
                                ui.label(shortcut.description);
                                ui.end_row();
                            }
                        });
                });
        });
}

fn exit_live_if_active(state: &mut AppState, reason: LiveExitReason) {
    if state.live_mode_state.is_active() {
        state.live_mode_state.stop(reason);
        state.playback_state.time_model.disable_realtime_lock();
        state.status_message = state
            .live_mode_state
            .last_exit_reason
            .map(|r| r.message().to_string())
            .unwrap_or_default();
    }
}
