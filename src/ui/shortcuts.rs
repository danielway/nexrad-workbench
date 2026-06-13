//! Centralized keyboard shortcut handling and help overlay.
//!
//! Most shortcuts are data-driven: each entry in [`ONE_SHOT_SHORTCUTS`]
//! pairs a trigger predicate with a typed handler, and both dispatch
//! (`handle_shortcuts`) and the help overlay (`ShortcutsHelpLayer`)
//! iterate the same registry — so the help can never drift from the
//! actually-wired-up bindings.
//!
//! Continuous (held-key) movement — WASD / arrows / Q+E with shift/ctrl
//! modifiers — is structurally different (it integrates dt and accumulates)
//! and lives in its own block, documented separately via
//! [`HELD_KEY_DOCS`].
//!
//! Spacebar play/pause is handled in `bottom_panel.rs` because the focus
//! and live-mode interactions there don't generalize; its entry in the
//! help overlay still lives here so users see the full set in one place.

use crate::geo::camera::CameraMode;
use crate::state::{AppState, LiveExitReason, PlaybackSpeed, RadarProduct, ViewMode};
use eframe::egui::{self, RichText};

// ---------------------------------------------------------------------------
// Help-overlay sections
// ---------------------------------------------------------------------------

const SECTION_PLAYBACK: &str = "Playback";
const SECTION_VIEW: &str = "View Modes";
const SECTION_CAMERA: &str = "Camera";
const SECTION_GENERAL: &str = "General";

/// Order in which sections are rendered in the help overlay.
const SECTION_ORDER: &[&str] = &[
    SECTION_PLAYBACK,
    SECTION_VIEW,
    SECTION_CAMERA,
    SECTION_GENERAL,
];

// ---------------------------------------------------------------------------
// Registry-driven one-shot shortcuts
// ---------------------------------------------------------------------------

/// Returns true when the shortcut should fire this frame.
///
/// Uses `&egui::InputState` rather than `&egui::Context` so the closure
/// can be stored in a `const` entry — `Context` is not `Copy`.
type PressedFn = fn(&egui::InputState) -> bool;

/// Returns true when the shortcut is applicable in the current state.
/// Mode-restricted shortcuts (2D-only, 3D-only) use this to gate firing
/// and to gray themselves out of the help overlay (future enhancement).
type EnabledFn = fn(&AppState) -> bool;

/// Side-effect to apply when the shortcut fires.
type HandlerFn = fn(
    &mut AppState,
    &mut crate::subsystem::Live,
    &crate::subsystem::Timeline,
    &mut crate::subsystem::Playback,
    &mut crate::subsystem::Chrome,
    &egui::Context,
);

/// A one-shot keyboard shortcut definition.
struct OneShotShortcut {
    section: &'static str,
    /// Display label for the key combo as it appears in the help overlay
    /// (e.g. "Ctrl+L", "?", "1").
    key_label: &'static str,
    description: &'static str,
    pressed: PressedFn,
    enabled: EnabledFn,
    handler: HandlerFn,
}

/// A static help-overlay entry for behavior handled outside the registry
/// (held-key movement, spacebar-in-bottom-panel) so users see one
/// canonical list of bindings.
#[derive(Clone, Copy)]
struct HelpEntry {
    section: &'static str,
    key_label: &'static str,
    description: &'static str,
}

const fn always_enabled(_: &AppState) -> bool {
    true
}

fn in_2d(state: &AppState) -> bool {
    state.viz_state.view_mode == ViewMode::Flat2D
}

fn in_3d(state: &AppState) -> bool {
    state.viz_state.view_mode == ViewMode::Globe3D
}

fn no_mods(i: &egui::InputState, key: egui::Key) -> bool {
    i.key_pressed(key) && !i.modifiers.any()
}

const ONE_SHOT_SHORTCUTS: &[OneShotShortcut] = &[
    // ---- Playback ---------------------------------------------------
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "[",
        description: "Step backward",
        pressed: |i| no_mods(i, egui::Key::OpenBracket),
        enabled: always_enabled,
        handler: handle_step_backward,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "]",
        description: "Step forward",
        pressed: |i| no_mods(i, egui::Key::CloseBracket),
        enabled: always_enabled,
        handler: handle_step_forward,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "-",
        description: "Decrease playback speed",
        pressed: |i| no_mods(i, egui::Key::Minus),
        enabled: always_enabled,
        handler: handle_speed_down,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "=",
        description: "Increase playback speed",
        pressed: |i| no_mods(i, egui::Key::Equals),
        enabled: always_enabled,
        handler: handle_speed_up,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "Ctrl+L",
        description: "Toggle live mode",
        pressed: |i| i.key_pressed(egui::Key::L) && i.modifiers.command,
        enabled: always_enabled,
        handler: handle_toggle_live,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "P",
        description: "Cycle product",
        pressed: |i| no_mods(i, egui::Key::P),
        enabled: always_enabled,
        handler: handle_cycle_product,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "E",
        description: "Cycle elevation up (2D)",
        pressed: |i| no_mods(i, egui::Key::E),
        // In 3D, E is reserved for upward camera movement.
        enabled: in_2d,
        handler: handle_cycle_elevation,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "S",
        description: "Open site selection (2D)",
        pressed: |i| no_mods(i, egui::Key::S),
        // In 3D, S is reserved for backward camera movement.
        enabled: in_2d,
        handler: handle_open_site,
    },
    // ---- View modes -------------------------------------------------
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "1",
        description: "2D top-down mode",
        pressed: |i| no_mods(i, egui::Key::Num1),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.viz_state.view_mode = ViewMode::Flat2D
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "2",
        description: "3D site orbit mode",
        pressed: |i| no_mods(i, egui::Key::Num2),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.viz_state.view_mode = ViewMode::Globe3D;
            state.viz_state.camera.switch_mode(CameraMode::SiteOrbit);
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "3",
        description: "3D planet orbit mode",
        pressed: |i| no_mods(i, egui::Key::Num3),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.viz_state.view_mode = ViewMode::Globe3D;
            state.viz_state.camera.switch_mode(CameraMode::PlanetOrbit);
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "4",
        description: "Free look mode",
        pressed: |i| no_mods(i, egui::Key::Num4),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.viz_state.view_mode = ViewMode::Globe3D;
            state.viz_state.camera.switch_mode(CameraMode::FreeLook);
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "T",
        description: "Toggle last 2D / 3D mode",
        pressed: |i| no_mods(i, egui::Key::T),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.viz_state.view_mode = match state.viz_state.view_mode {
                ViewMode::Flat2D => ViewMode::Globe3D,
                ViewMode::Globe3D => ViewMode::Flat2D,
            };
        },
    },
    // ---- Camera -----------------------------------------------------
    OneShotShortcut {
        section: SECTION_CAMERA,
        key_label: "R",
        description: "Reset camera to default",
        pressed: |i| no_mods(i, egui::Key::R),
        enabled: always_enabled,
        handler: handle_reset_camera,
    },
    OneShotShortcut {
        section: SECTION_CAMERA,
        key_label: "F",
        description: "Focus on radar site",
        pressed: |i| no_mods(i, egui::Key::F),
        enabled: always_enabled,
        handler: handle_focus_site,
    },
    OneShotShortcut {
        section: SECTION_CAMERA,
        key_label: "N",
        description: "Align North up (3D)",
        pressed: |i| no_mods(i, egui::Key::N),
        enabled: in_3d,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.viz_state.camera.align_north()
        },
    },
    OneShotShortcut {
        section: SECTION_CAMERA,
        key_label: "Home",
        description: "Reset pivot to default (3D)",
        pressed: |i| no_mods(i, egui::Key::Home),
        enabled: in_3d,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.viz_state.camera.reset_pivot()
        },
    },
    // ---- General ----------------------------------------------------
    OneShotShortcut {
        section: SECTION_GENERAL,
        key_label: "?",
        description: "Toggle this help overlay",
        // ? requires Shift+/ on most layouts; some keyboards send Questionmark directly.
        pressed: |i| {
            i.key_pressed(egui::Key::Questionmark)
                || (i.key_pressed(egui::Key::Slash) && i.modifiers.shift)
        },
        enabled: always_enabled,
        handler: |_state, _live, _timeline, _playback, chrome, _| {
            chrome.shortcuts_help_visible = !chrome.shortcuts_help_visible
        },
    },
    OneShotShortcut {
        section: SECTION_GENERAL,
        key_label: "Ctrl+Shift+A",
        description: "Toggle Basic / Advanced controls",
        pressed: |i| i.key_pressed(egui::Key::A) && i.modifiers.command && i.modifiers.shift,
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.advanced_mode = !state.advanced_mode
        },
    },
];

/// Help-only entries for behavior dispatched outside this registry.
const HELD_KEY_DOCS: &[HelpEntry] = &[
    HelpEntry {
        section: SECTION_PLAYBACK,
        key_label: "Space",
        description: "Play / Pause",
    },
    HelpEntry {
        section: SECTION_CAMERA,
        key_label: "WASD / Arrows",
        description: "Move / pan camera",
    },
    HelpEntry {
        section: SECTION_CAMERA,
        key_label: "Q / E",
        description: "Move down / up (3D)",
    },
    HelpEntry {
        section: SECTION_CAMERA,
        key_label: "Shift",
        description: "2× camera speed",
    },
    HelpEntry {
        section: SECTION_CAMERA,
        key_label: "Ctrl",
        description: "¼× camera speed",
    },
    HelpEntry {
        section: SECTION_GENERAL,
        key_label: "Esc",
        description: "Close open modal / overlay",
    },
];

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Process keyboard shortcuts. Call once per frame from the main update loop.
pub fn handle_shortcuts(
    ctx: &egui::Context,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    // No keyboard on mobile; skip shortcut processing entirely.
    if state.is_mobile {
        return;
    }
    // Skip when a text field has focus so typing doesn't trigger shortcuts.
    if ctx.memory(|m| m.focused().is_some()) {
        return;
    }

    // Registry-driven one-shot shortcuts: each entry's `pressed` predicate
    // is checked against the current frame's input; matches run the handler.
    for sc in ONE_SHOT_SHORTCUTS {
        if !(sc.enabled)(state) {
            continue;
        }
        if ctx.input(sc.pressed) {
            (sc.handler)(state, live, timeline, playback, chrome, ctx);
        }
    }

    // Continuous WASD/arrows movement. Structurally different from
    // one-shot dispatch (integrates dt, multi-key combinations).
    handle_continuous_movement(ctx, state, playback);
}

// ---------------------------------------------------------------------------
// Per-handler functions for the registry
// ---------------------------------------------------------------------------

fn current_pos(_state: &AppState, playback: &crate::subsystem::Playback) -> f64 {
    playback.state.playback_position()
}

fn jog_fallback(_state: &AppState, playback: &crate::subsystem::Playback) -> f64 {
    playback.state.speed.timeline_seconds_per_real_second()
}

fn handle_step_backward(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    let pos = current_pos(state, playback);
    let fallback = jog_fallback(state, playback);
    // Stepping is a seek gesture: detach the tether first (spec §7/§12), then
    // step. The stream keeps ingesting unless the data-saver policy stops it.
    live.detach_playhead(
        &mut playback.state,
        state.frame_now.secs(),
        state.pause_stream_while_reviewing,
    );
    let new_pos = match &state.viz_state.elevation_selection {
        crate::state::ElevationSelection::Fixed {
            elevation_number, ..
        } => timeline
            .scans
            .prev_matching_sweep_end_by_number(pos, *elevation_number)
            .unwrap_or(pos - fallback),
        crate::state::ElevationSelection::Latest => timeline
            .scans
            .prev_any_sweep_end(pos)
            .unwrap_or(pos - fallback),
    };
    playback.state.set_playback_position(new_pos);
}

fn handle_step_forward(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    let pos = current_pos(state, playback);
    let fallback = jog_fallback(state, playback);
    // Stepping is a seek gesture: detach the tether first (spec §7/§12), then
    // step. The stream keeps ingesting unless the data-saver policy stops it.
    live.detach_playhead(
        &mut playback.state,
        state.frame_now.secs(),
        state.pause_stream_while_reviewing,
    );
    let new_pos = match &state.viz_state.elevation_selection {
        crate::state::ElevationSelection::Fixed {
            elevation_number, ..
        } => timeline
            .scans
            .next_matching_sweep_end_by_number(pos, *elevation_number)
            .unwrap_or(pos + fallback),
        crate::state::ElevationSelection::Latest => timeline
            .scans
            .next_any_sweep_end(pos)
            .unwrap_or(pos + fallback),
    };
    playback.state.set_playback_position(new_pos);
}

/// The speed variants the keyboard cycles through, restricted to the active
/// effective mode's setting space: in Macro only the macro-capable (fps)
/// variants, in Micro all variants. Cycling outside this set would land on a
/// variant whose combo label and actual playback disagree (advance_macro
/// silently falls back). When the current speed isn't in the active set
/// (e.g. just snapped modes), the nearest index is found by position in the
/// fallback `all()` ordering.
fn active_mode_speeds(playback: &crate::subsystem::Playback) -> &'static [PlaybackSpeed] {
    match playback.state.effective_playback_mode() {
        crate::state::PlaybackMode::Macro => PlaybackSpeed::macro_speeds(),
        crate::state::PlaybackMode::Micro => PlaybackSpeed::all(),
    }
}

fn handle_speed_down(
    _state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    let speeds = active_mode_speeds(playback);
    if let Some(idx) = speeds.iter().position(|s| *s == playback.state.speed) {
        if idx > 0 {
            playback.state.speed = speeds[idx - 1];
        }
    } else if let Some(&first) = speeds.first() {
        // Current speed isn't valid in this mode — land on the slowest valid.
        playback.state.speed = first;
    }
}

fn handle_speed_up(
    _state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    let speeds = active_mode_speeds(playback);
    if let Some(idx) = speeds.iter().position(|s| *s == playback.state.speed) {
        if idx + 1 < speeds.len() {
            playback.state.speed = speeds[idx + 1];
        }
    } else if let Some(&first) = speeds.first() {
        // Current speed isn't valid in this mode — land on the slowest valid.
        playback.state.speed = first;
    }
}

fn handle_toggle_live(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    if live.mode_state.is_active() {
        live.stop(LiveExitReason::UserStopped);
        playback.state.exit_live(crate::state::FreezeAt::Keep);
        playback.state.playing = false;
        state.status_message = live
            .mode_state
            .last_exit_reason
            .map(|r| r.message().to_string())
            .unwrap_or_default();
    } else {
        state.push_command(crate::state::AppCommand::StartLive);
        playback.state.speed = PlaybackSpeed::Realtime;
    }
}

fn handle_cycle_product(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    let products = RadarProduct::all();
    if let Some(idx) = products.iter().position(|p| *p == state.viz_state.product) {
        state.viz_state.product = products[(idx + 1) % products.len()];
    }
}

fn handle_cycle_elevation(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    let entries = state.current_elevation_list(
        &playback.state,
        &timeline.scans,
        live.radar_model
            .volume
            .as_ref()
            .and_then(|v| v.vcp_pattern.as_ref()),
    );
    if entries.is_empty() {
        return;
    }
    let current_idx = match &state.viz_state.elevation_selection {
        crate::state::ElevationSelection::Fixed {
            elevation_number, ..
        } => entries
            .iter()
            .position(|e| e.elevation_number == *elevation_number)
            .unwrap_or(0),
        crate::state::ElevationSelection::Latest => 0,
    };
    let next_idx = (current_idx + 1) % entries.len();
    let entry = &entries[next_idx];
    state.viz_state.elevation_selection = crate::state::ElevationSelection::Fixed {
        elevation_number: entry.elevation_number,
        angle: entry.angle,
    };
}

fn handle_open_site(
    _state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    _playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    chrome.site_modal_open = true;
}

fn handle_reset_camera(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    if state.viz_state.view_mode == ViewMode::Flat2D {
        state.viz_state.zoom = 1.0;
        state.viz_state.pan_offset = egui::Vec2::ZERO;
    } else {
        state.viz_state.camera.reset();
    }
}

fn handle_focus_site(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    if state.viz_state.view_mode == ViewMode::Flat2D {
        state.viz_state.pan_offset = egui::Vec2::ZERO;
    } else {
        state.viz_state.camera.focus_site();
    }
}

// ---------------------------------------------------------------------------
// Continuous (held-key) movement
// ---------------------------------------------------------------------------

fn handle_continuous_movement(
    ctx: &egui::Context,
    state: &mut AppState,
    _playback: &mut crate::subsystem::Playback,
) {
    let (forward, right_move, up_move, speed_mult, dt) = ctx.input(|i| {
        let dt = i.stable_dt.min(0.1); // cap to avoid jumps
        let w = i.key_down(egui::Key::W) as i32 as f32;
        let a = i.key_down(egui::Key::A) as i32 as f32;
        let s = i.key_down(egui::Key::S) as i32 as f32;
        let d = i.key_down(egui::Key::D) as i32 as f32;
        let q = i.key_down(egui::Key::Q) as i32 as f32;
        let e = i.key_down(egui::Key::E) as i32 as f32;
        let up = i.key_down(egui::Key::ArrowUp) as i32 as f32;
        let down = i.key_down(egui::Key::ArrowDown) as i32 as f32;
        let left = i.key_down(egui::Key::ArrowLeft) as i32 as f32;
        let right = i.key_down(egui::Key::ArrowRight) as i32 as f32;
        let forward = (w + up) - (s + down);
        let right_move = (d + right) - (a + left);
        let up_move = e - q;
        let speed_mult = if i.modifiers.shift {
            2.0
        } else if i.modifiers.command {
            0.25
        } else {
            1.0
        };
        (forward, right_move, up_move, speed_mult, dt)
    });

    if forward == 0.0 && right_move == 0.0 && up_move == 0.0 {
        return;
    }

    if state.viz_state.view_mode == ViewMode::Globe3D {
        let moved = state
            .viz_state
            .camera
            .keyboard_move(forward, right_move, up_move, speed_mult, dt);
        if moved {
            ctx.request_repaint();
        }
    } else {
        // 2D mode: WASD/arrows pan the map.
        let pan_speed = 200.0 * speed_mult * dt;
        state.viz_state.pan_offset.x -= right_move * pan_speed;
        state.viz_state.pan_offset.y += forward * pan_speed;
        ctx.request_repaint();
    }
}

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

pub(super) struct ShortcutsHelpLayer;

impl super::layout::Layer for ShortcutsHelpLayer {
    fn kind(&self) -> super::layout::LayerKind {
        super::layout::LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        20
    }
    fn visible(&self, ctx: &super::layout::LayoutCtx) -> bool {
        ctx.chrome.shortcuts_help_visible
    }
    fn render(&self, ctx: &mut super::layout::LayoutCtx) {
        draw_shortcuts_help(ctx.ctx, ctx.state, ctx.chrome);
    }
}

fn draw_shortcuts_help(
    ctx: &egui::Context,
    _state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
) {
    // Close on Escape (checked here because the overlay area may consume the key
    // event before handle_shortcuts sees it)
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        chrome.shortcuts_help_visible = false;
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
                                chrome.shortcuts_help_visible = false;
                            }
                        });
                    });

                    ui.separator();

                    for section in SECTION_ORDER {
                        render_help_section(ui, section);
                    }
                });
        });
}

fn render_help_section(ui: &mut egui::Ui, section: &'static str) {
    let registry_entries = ONE_SHOT_SHORTCUTS
        .iter()
        .filter(|sc| sc.section == section)
        .map(|sc| (sc.key_label, sc.description));
    let doc_entries = HELD_KEY_DOCS
        .iter()
        .filter(|h| h.section == section)
        .map(|h| (h.key_label, h.description));
    let entries: Vec<_> = doc_entries.chain(registry_entries).collect();
    if entries.is_empty() {
        return;
    }

    ui.add_space(4.0);
    ui.label(
        RichText::new(section)
            .strong()
            .size(12.0)
            .color(ui.visuals().strong_text_color()),
    );
    ui.add_space(2.0);

    egui::Grid::new(format!("shortcuts_grid_{}", section))
        .num_columns(2)
        .spacing([20.0, 4.0])
        .show(ui, |ui| {
            for (key, description) in entries {
                ui.label(RichText::new(key).monospace().strong());
                ui.label(description);
                ui.end_row();
            }
        });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn registry_has_no_duplicate_key_labels() {
        // Two entries with the same key_label would render identically in
        // help and look like duplicates to users — catch that early. (Two
        // entries can legitimately share a key when one is mode-gated;
        // they should differ in description, but the user-visible label
        // should still be distinct.)
        let mut seen = HashSet::new();
        for sc in ONE_SHOT_SHORTCUTS {
            let dup = !seen.insert((sc.key_label, sc.description));
            assert!(
                !dup,
                "duplicate registry entry for key={} desc={}",
                sc.key_label, sc.description
            );
        }
    }

    #[wasm_bindgen_test]
    fn registry_sections_are_in_order_list() {
        // Every section used in the registry must appear in SECTION_ORDER,
        // otherwise the help overlay would silently skip those entries.
        for sc in ONE_SHOT_SHORTCUTS {
            assert!(
                SECTION_ORDER.contains(&sc.section),
                "section {:?} missing from SECTION_ORDER",
                sc.section
            );
        }
        for h in HELD_KEY_DOCS {
            assert!(
                SECTION_ORDER.contains(&h.section),
                "section {:?} missing from SECTION_ORDER",
                h.section
            );
        }
    }
}
