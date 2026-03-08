//! Centralized keyboard shortcut handling and help overlay.

use crate::geo::camera::CameraMode;
use crate::state::{AppState, LiveExitReason, PlaybackSpeed, RadarProduct, ViewMode};
use eframe::egui::{self, RichText};

/// Shortcut definition for display in the help overlay.
struct Shortcut {
    key: &'static str,
    description: &'static str,
}

/// Section header in the shortcuts help overlay.
struct ShortcutSection {
    title: &'static str,
    shortcuts: &'static [Shortcut],
}

const PLAYBACK_SHORTCUTS: &[Shortcut] = &[
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
        key: "Ctrl+L",
        description: "Toggle live mode",
    },
    Shortcut {
        key: "P",
        description: "Cycle product",
    },
    Shortcut {
        key: "E",
        description: "Cycle elevation up (2D)",
    },
    Shortcut {
        key: "S",
        description: "Open site selection (2D)",
    },
];

const VIEW_SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        key: "1",
        description: "2D top-down mode",
    },
    Shortcut {
        key: "2",
        description: "3D planet orbit mode",
    },
    Shortcut {
        key: "3",
        description: "3D site orbit mode",
    },
    Shortcut {
        key: "4",
        description: "Free look mode",
    },
    Shortcut {
        key: "T",
        description: "Toggle last 2D / 3D mode",
    },
];

const CAMERA_SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        key: "WASD / Arrows",
        description: "Move / pan camera",
    },
    Shortcut {
        key: "Q / E",
        description: "Move down / up (3D)",
    },
    Shortcut {
        key: "Shift",
        description: "2× camera speed",
    },
    Shortcut {
        key: "Ctrl",
        description: "¼× camera speed",
    },
    Shortcut {
        key: "R",
        description: "Reset camera to default",
    },
    Shortcut {
        key: "F",
        description: "Focus on radar site",
    },
    Shortcut {
        key: "N",
        description: "Align North up (3D)",
    },
    Shortcut {
        key: "Home",
        description: "Reset pivot to default (3D)",
    },
];

const GENERAL_SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        key: "?",
        description: "Toggle this help overlay",
    },
    Shortcut {
        key: "Esc",
        description: "Close open modal / overlay",
    },
];

const SHORTCUT_SECTIONS: &[ShortcutSection] = &[
    ShortcutSection {
        title: "Playback",
        shortcuts: PLAYBACK_SHORTCUTS,
    },
    ShortcutSection {
        title: "View Modes",
        shortcuts: VIEW_SHORTCUTS,
    },
    ShortcutSection {
        title: "Camera",
        shortcuts: CAMERA_SHORTCUTS,
    },
    ShortcutSection {
        title: "General",
        shortcuts: GENERAL_SHORTCUTS,
    },
];

/// Process keyboard shortcuts. Call once per frame from the main update loop.
pub fn handle_shortcuts(ctx: &egui::Context, state: &mut AppState) {
    // Skip shortcut processing when a text field has focus
    let has_focus = ctx.memory(|m| m.focused().is_some());
    if has_focus {
        return;
    }

    // Re-read input for actual state mutations (can't mutate state inside ctx.input closure)
    let step_back = ctx.input(|i| i.key_pressed(egui::Key::OpenBracket) && !i.modifiers.any());
    let step_fwd = ctx.input(|i| i.key_pressed(egui::Key::CloseBracket) && !i.modifiers.any());
    let speed_down = ctx.input(|i| i.key_pressed(egui::Key::Minus) && !i.modifiers.any());
    let speed_up = ctx.input(|i| i.key_pressed(egui::Key::Equals) && !i.modifiers.any());
    let toggle_live = ctx.input(|i| i.key_pressed(egui::Key::L) && i.modifiers.command);
    let cycle_product = ctx.input(|i| i.key_pressed(egui::Key::P) && !i.modifiers.any());
    let cycle_elevation = ctx.input(|i| i.key_pressed(egui::Key::E) && !i.modifiers.any());
    let open_site = ctx.input(|i| i.key_pressed(egui::Key::S) && !i.modifiers.any());
    let toggle_help = ctx.input(|i| {
        // ? requires Shift on most layouts
        i.key_pressed(egui::Key::Questionmark)
            || (i.key_pressed(egui::Key::Slash) && i.modifiers.shift)
    });

    // View mode switching (1-4 keys)
    let mode_1 = ctx.input(|i| i.key_pressed(egui::Key::Num1) && !i.modifiers.any());
    let mode_2 = ctx.input(|i| i.key_pressed(egui::Key::Num2) && !i.modifiers.any());
    let mode_3 = ctx.input(|i| i.key_pressed(egui::Key::Num3) && !i.modifiers.any());
    let mode_4 = ctx.input(|i| i.key_pressed(egui::Key::Num4) && !i.modifiers.any());
    let toggle_2d_3d = ctx.input(|i| i.key_pressed(egui::Key::T) && !i.modifiers.any());

    // Camera controls
    let reset_camera = ctx.input(|i| i.key_pressed(egui::Key::R) && !i.modifiers.any());
    let focus_site = ctx.input(|i| i.key_pressed(egui::Key::F) && !i.modifiers.any());
    let align_north = ctx.input(|i| i.key_pressed(egui::Key::N) && !i.modifiers.any());
    let reset_pivot = ctx.input(|i| i.key_pressed(egui::Key::Home) && !i.modifiers.any());

    // WASD continuous movement (held keys)
    let dt = ctx.input(|i| i.stable_dt).min(0.1); // cap to avoid jumps
    let w_held = ctx.input(|i| i.key_down(egui::Key::W));
    let a_held = ctx.input(|i| i.key_down(egui::Key::A));
    let s_held = ctx.input(|i| i.key_down(egui::Key::S));
    let d_held = ctx.input(|i| i.key_down(egui::Key::D));
    let q_held = ctx.input(|i| i.key_down(egui::Key::Q));
    let e_held = ctx.input(|i| i.key_down(egui::Key::E));
    let up_held = ctx.input(|i| i.key_down(egui::Key::ArrowUp));
    let down_held = ctx.input(|i| i.key_down(egui::Key::ArrowDown));
    let left_held = ctx.input(|i| i.key_down(egui::Key::ArrowLeft));
    let right_held = ctx.input(|i| i.key_down(egui::Key::ArrowRight));
    let shift_held = ctx.input(|i| i.modifiers.shift);
    let ctrl_held = ctx.input(|i| i.modifiers.command);

    // Skip if focus is held (re-check after reading inputs)
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

    if cycle_product {
        let products = RadarProduct::all();
        if let Some(idx) = products.iter().position(|p| *p == state.viz_state.product) {
            state.viz_state.product = products[(idx + 1) % products.len()];
        }
    }

    // E cycles elevation in 2D mode (where Q/E vertical movement doesn't apply).
    // In 3D mode, E is reserved for upward camera movement.
    if cycle_elevation && state.viz_state.view_mode == ViewMode::Flat2D {
        let new_elev = state.viz_state.target_elevation + 0.5;
        state.viz_state.target_elevation = if new_elev > 19.5 { 0.5 } else { new_elev };
    }

    // S opens site modal only in 2D mode. In 3D, S is reserved for backward movement.
    if open_site && state.viz_state.view_mode == ViewMode::Flat2D {
        state.site_modal_open = true;
    }

    // ── View mode switching ──

    if mode_1 {
        state.viz_state.view_mode = ViewMode::Flat2D;
    }

    if mode_2 {
        state.viz_state.view_mode = ViewMode::Globe3D;
        state.viz_state.camera.switch_mode(CameraMode::PlanetOrbit);
    }

    if mode_3 {
        state.viz_state.view_mode = ViewMode::Globe3D;
        state.viz_state.camera.switch_mode(CameraMode::SiteOrbit);
    }

    if mode_4 {
        state.viz_state.view_mode = ViewMode::Globe3D;
        state.viz_state.camera.switch_mode(CameraMode::FreeLook);
    }

    if toggle_2d_3d {
        state.viz_state.view_mode = match state.viz_state.view_mode {
            ViewMode::Flat2D => ViewMode::Globe3D,
            ViewMode::Globe3D => ViewMode::Flat2D,
        };
    }

    // ── Camera controls ──

    if reset_camera {
        if state.viz_state.view_mode == ViewMode::Flat2D {
            state.viz_state.zoom = 1.0;
            state.viz_state.pan_offset = eframe::egui::Vec2::ZERO;
        } else {
            state.viz_state.camera.reset();
        }
    }

    if focus_site {
        if state.viz_state.view_mode == ViewMode::Flat2D {
            state.viz_state.pan_offset = eframe::egui::Vec2::ZERO;
        } else {
            state.viz_state.camera.focus_site();
        }
    }

    if align_north && state.viz_state.view_mode == ViewMode::Globe3D {
        state.viz_state.camera.align_north();
    }

    if reset_pivot {
        if state.viz_state.view_mode == ViewMode::Globe3D {
            state.viz_state.camera.reset_pivot();
        }
    }

    // ── WASD / Arrow key movement ──
    // S and E are mode-split: in 2D they trigger site selection / elevation cycling,
    // in 3D they are reserved for camera movement (backward / up).

    let forward = if w_held || up_held { 1.0f32 } else { 0.0 }
        - if s_held || down_held { 1.0 } else { 0.0 };
    let right_move = if d_held || right_held { 1.0f32 } else { 0.0 }
        - if a_held || left_held { 1.0 } else { 0.0 };
    let up_move = if e_held { 1.0f32 } else { 0.0 } - if q_held { 1.0 } else { 0.0 };

    let speed_mult = if shift_held {
        2.0
    } else if ctrl_held {
        0.25
    } else {
        1.0
    };

    if forward != 0.0 || right_move != 0.0 || up_move != 0.0 {
        if state.viz_state.view_mode == ViewMode::Globe3D {
            let moved = state
                .viz_state
                .camera
                .keyboard_move(forward, right_move, up_move, speed_mult, dt);
            if moved {
                ctx.request_repaint();
            }
        } else {
            // 2D mode: WASD/arrows pan the map
            let pan_speed = 200.0 * speed_mult * dt;
            state.viz_state.pan_offset.x -= right_move * pan_speed;
            state.viz_state.pan_offset.y += forward * pan_speed;
            ctx.request_repaint();
        }
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

    // Close on Escape (checked here because the overlay area may consume the key
    // event before handle_shortcuts sees it)
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.shortcuts_help_visible = false;
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
                    ui.set_min_width(320.0);
                    ui.set_max_width(420.0);

                    ui.horizontal(|ui| {
                        ui.heading("Keyboard Shortcuts");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(egui_phosphor::regular::X).clicked() {
                                state.shortcuts_help_visible = false;
                            }
                        });
                    });

                    ui.separator();

                    for section in SHORTCUT_SECTIONS {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(section.title)
                                .strong()
                                .size(12.0)
                                .color(ui.visuals().strong_text_color()),
                        );
                        ui.add_space(2.0);

                        egui::Grid::new(format!("shortcuts_grid_{}", section.title))
                            .num_columns(2)
                            .spacing([20.0, 4.0])
                            .show(ui, |ui| {
                                for shortcut in section.shortcuts.iter() {
                                    ui.label(RichText::new(shortcut.key).monospace().strong());
                                    ui.label(shortcut.description);
                                    ui.end_row();
                                }
                            });
                    }
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
