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

use crate::core::dialogs::{DialogId, DialogIntent, OwnedShortcut};
use crate::core::keymap::{keymap_rows, KeyScope};
use crate::core::transport::{LoopEdge, StepDir, StepUnit};
use crate::geo::ViewMode;
use crate::state::AppState;
use crate::ui::theme;
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

/// Side-effect to apply when the shortcut fires.
type HandlerFn = fn(
    &mut AppState,
    &mut crate::subsystem::Live,
    &crate::subsystem::Sites,
    &mut crate::subsystem::Playback,
    &mut crate::subsystem::Chrome,
    &egui::Context,
);

/// A one-shot keyboard shortcut definition.
struct OneShotShortcut {
    section: &'static str,
    /// The dialog that owns this shortcut, so it still fires while that
    /// dialog is on top (`None`: suppressed under any dialog).
    owned: Option<OwnedShortcut>,
    /// Display label for the key combo as it appears in the help overlay
    /// (e.g. "Ctrl+L", "?", "1").
    key_label: &'static str,
    description: &'static str,
    pressed: PressedFn,
    /// Where it works: gates firing and greys its row on the map.
    scope: KeyScope,
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
    scope: KeyScope,
}

fn no_mods(i: &egui::InputState, key: egui::Key) -> bool {
    i.key_pressed(key) && !i.modifiers.any()
}

const ONE_SHOT_SHORTCUTS: &[OneShotShortcut] = &[
    // ---- Playback ---------------------------------------------------
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "←/→",
        description: "Frame step (prev / next)",
        pressed: |i| no_mods(i, egui::Key::ArrowLeft) || no_mods(i, egui::Key::ArrowRight),
        scope: KeyScope::Always,
        handler: handle_frame_step,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "Shift+←/→",
        description: "Scan step (prev / next volume)",
        pressed: |i| {
            i.modifiers.shift_only()
                && (i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowRight))
        },
        scope: KeyScope::Always,
        handler: handle_scan_step,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "[ / ]",
        description: "Speed down / up",
        pressed: |i| no_mods(i, egui::Key::OpenBracket) || no_mods(i, egui::Key::CloseBracket),
        scope: KeyScope::Always,
        handler: handle_speed_step,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "+ / −",
        description: "Zoom timeline (in / out)",
        // Equals carries the unshifted "+" key on most layouts; accept both the
        // bare Plus and Shift+Equals so "+" works regardless of keymap. Minus
        // zooms out.
        pressed: |i| {
            no_mods(i, egui::Key::Minus)
                || no_mods(i, egui::Key::Equals)
                || no_mods(i, egui::Key::Plus)
                || (i.key_pressed(egui::Key::Equals) && i.modifiers.shift_only())
        },
        scope: KeyScope::Always,
        handler: handle_zoom_step,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "I",
        description: "Set loop in-point at playhead",
        pressed: |i| no_mods(i, egui::Key::I),
        scope: KeyScope::Always,
        handler: handle_loop_in,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "O",
        description: "Set loop out-point at playhead",
        pressed: |i| no_mods(i, egui::Key::O),
        scope: KeyScope::Always,
        handler: handle_loop_out,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "L",
        description: "Go live (re-tether)",
        pressed: |i| no_mods(i, egui::Key::L),
        scope: KeyScope::Always,
        handler: handle_go_live,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "Shift+L",
        description: "Stop live stream",
        pressed: |i| i.modifiers.shift_only() && i.key_pressed(egui::Key::L),
        scope: KeyScope::Always,
        handler: handle_stop_live,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "P",
        description: "Next product",
        pressed: |i| no_mods(i, egui::Key::P),
        scope: KeyScope::Always,
        handler: |state, _live, _sites, _playback, _chrome, _| step_product(state, true),
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "Shift+P",
        description: "Previous product",
        pressed: |i| i.modifiers.shift_only() && i.key_pressed(egui::Key::P),
        scope: KeyScope::Always,
        handler: |state, _live, _sites, _playback, _chrome, _| step_product(state, false),
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "E",
        description: "Next tilt down the ladder (2D)",
        pressed: |i| no_mods(i, egui::Key::E),
        // In 3D, E is reserved for heading rotation.
        scope: KeyScope::Only2D,
        handler: |state, live, sites, playback, _chrome, _| {
            step_tilt(state, live, sites, playback, true)
        },
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "Shift+E",
        description: "Next tilt up the ladder (2D)",
        pressed: |i| i.modifiers.shift_only() && i.key_pressed(egui::Key::E),
        scope: KeyScope::Only2D,
        handler: |state, live, sites, playback, _chrome, _| {
            step_tilt(state, live, sites, playback, false)
        },
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "G",
        description: "Go to a day",
        pressed: |i| no_mods(i, egui::Key::G),
        scope: KeyScope::Always,
        handler: |state, _live, _sites, _playback, _chrome, _| {
            state.push_command(crate::core::Intent::OpenGoToDay)
        },
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        owned: None,
        key_label: "S",
        description: "Open site selection (2D)",
        pressed: |i| no_mods(i, egui::Key::S),
        // In 3D, S is reserved for camera panning.
        scope: KeyScope::Only2D,
        handler: handle_open_site,
    },
    // ---- View modes -------------------------------------------------
    OneShotShortcut {
        section: SECTION_VIEW,
        owned: None,
        key_label: "1",
        description: "2D top-down mode",
        pressed: |i| no_mods(i, egui::Key::Num1),
        scope: KeyScope::Always,
        handler: |state, _live, _sites, _playback, _chrome, _| {
            state.viz_state.switch_to_2d();
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        owned: None,
        key_label: "2",
        description: "3D globe mode",
        pressed: |i| no_mods(i, egui::Key::Num2),
        scope: KeyScope::Always,
        handler: |state, _live, _sites, _playback, _chrome, _| {
            state.viz_state.switch_to_3d_view();
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        owned: None,
        key_label: "T",
        description: "Toggle 2D / 3D view",
        pressed: |i| no_mods(i, egui::Key::T),
        scope: KeyScope::Always,
        handler: |state, _live, _sites, _playback, _chrome, _| {
            state.viz_state.toggle_2d_3d();
        },
    },
    // ---- Camera -----------------------------------------------------
    OneShotShortcut {
        section: SECTION_CAMERA,
        owned: None,
        key_label: "R",
        description: "Reset camera to default",
        pressed: |i| no_mods(i, egui::Key::R),
        scope: KeyScope::Always,
        handler: handle_reset_camera,
    },
    OneShotShortcut {
        section: SECTION_CAMERA,
        owned: None,
        key_label: "F",
        description: "Focus on radar site",
        pressed: |i| no_mods(i, egui::Key::F),
        scope: KeyScope::Always,
        handler: handle_focus_site,
    },
    OneShotShortcut {
        section: SECTION_CAMERA,
        owned: None,
        key_label: "N",
        description: "Align North up (3D)",
        pressed: |i| no_mods(i, egui::Key::N),
        scope: KeyScope::Only3D,
        handler: |state, _live, _sites, _playback, _chrome, _| state.viz_state.camera.align_north(),
    },
    // ---- General ----------------------------------------------------
    OneShotShortcut {
        section: SECTION_GENERAL,
        owned: Some(OwnedShortcut::ToggleHelp),
        key_label: "?",
        description: "Toggle this help overlay",
        // ? requires Shift+/ on most layouts; some keyboards send Questionmark directly.
        pressed: |i| {
            i.key_pressed(egui::Key::Questionmark)
                || (i.key_pressed(egui::Key::Slash) && i.modifiers.shift)
        },
        scope: KeyScope::Always,
        handler: |_state, _live, _sites, _playback, chrome, _| {
            chrome.shortcuts_help_visible = !chrome.shortcuts_help_visible
        },
    },
];

/// Help-only entries for behavior dispatched outside this registry.
const HELD_KEY_DOCS: &[HelpEntry] = &[
    HelpEntry {
        section: SECTION_PLAYBACK,
        key_label: "Space",
        description: "Play / Pause",
        scope: KeyScope::Always,
    },
    HelpEntry {
        section: SECTION_CAMERA,
        key_label: "WASD",
        description: "Pan map / camera",
        scope: KeyScope::Always,
    },
    HelpEntry {
        section: SECTION_CAMERA,
        key_label: "Q / E",
        description: "Rotate view (3D)",
        scope: KeyScope::Only3D,
    },
    HelpEntry {
        section: SECTION_CAMERA,
        key_label: "Shift",
        description: "2× camera speed",
        scope: KeyScope::Always,
    },
    HelpEntry {
        section: SECTION_CAMERA,
        key_label: "Ctrl",
        description: "¼× camera speed",
        scope: KeyScope::Always,
    },
    HelpEntry {
        section: SECTION_GENERAL,
        key_label: "Esc",
        description: "Close the top dialog (shortcuts pause while one is open)",
        scope: KeyScope::Always,
    },
];

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Process keyboard shortcuts. Call once per frame from the main update loop.
pub(crate) fn handle_shortcuts(
    ctx: &egui::Context,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    sites: &crate::subsystem::Sites,
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
    // Under a dialog or popover only the shortcuts the top one owns fire
    // (core::dialogs); Escape is routed to the stack separately.
    let is_3d = state.viz_state.view_mode() == ViewMode::Globe3D;
    for sc in ONE_SHOT_SHORTCUTS {
        if !sc.scope.active(is_3d) || !chrome.dialogs.allows(sc.owned) {
            continue;
        }
        if ctx.input(sc.pressed) {
            (sc.handler)(state, live, sites, playback, chrome, ctx);
        }
    }

    // Continuous WASD/arrows movement. Structurally different from
    // one-shot dispatch (integrates dt, multi-key combinations).
    if chrome.dialogs.allows(None) {
        handle_continuous_movement(ctx, state, playback);
    }
}

// ---------------------------------------------------------------------------
// Per-handler functions for the registry
// ---------------------------------------------------------------------------

/// Direction a step shortcut moves the playhead, read from this frame's input.
/// ArrowRight wins if somehow both are pressed in the same frame (forward
/// bias).
fn step_dir(ctx: &egui::Context) -> StepDir {
    if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
        StepDir::Forward
    } else {
        StepDir::Back
    }
}

/// ←/→ frame step: the same step as the transport buttons (one frame of the
/// selected product and tilt; a whole scan in volumetric 3D), decided by
/// `core::transport::reduce_step`.
fn handle_frame_step(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    ctx: &egui::Context,
) {
    state.push_command(crate::core::Intent::Step {
        dir: step_dir(ctx),
        unit: StepUnit::Frame,
    });
}

/// Shift+←/→ scan step: one whole volume.
fn handle_scan_step(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    ctx: &egui::Context,
) {
    state.push_command(crate::core::Intent::Step {
        dir: step_dir(ctx),
        unit: StepUnit::Volume,
    });
}

/// [ / ] speed down / up (spec §12): one rung within the mode's speeds,
/// decided by `core::transport::reduce_step_speed`.
fn handle_speed_step(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    ctx: &egui::Context,
) {
    let faster = ctx.input(|i| i.key_pressed(egui::Key::CloseBracket));
    state.push_command(crate::core::Intent::StepSpeed { faster });
}

/// Multiplicative timeline-zoom step per `+`/`−` press (spec §12). A ~1.3×
/// factor gives a brisk but controllable zoom.
const ZOOM_KEY_STEP: f64 = 1.3;

/// New `timeline_view_start` that keeps the timestamp `anchor_ts` at the same
/// on-screen pixel while the zoom changes `old_zoom → new_zoom`. Pure so the
/// anchoring math is unit-testable: `offset_px = (anchor_ts - view_start) *
/// old_zoom` is held fixed, so `view_start' = anchor_ts - offset_px /
/// new_zoom`.
fn view_start_anchored_at(view_start: f64, old_zoom: f64, new_zoom: f64, anchor_ts: f64) -> f64 {
    let offset_px = (anchor_ts - view_start) * old_zoom;
    anchor_ts - offset_px / new_zoom
}

/// Timeline zoom in/out on `+`/`−`, anchored at the playhead (spec §12). Routed
/// through [`PlaybackState::set_timeline_zoom`] so tier hysteresis applies,
/// and the view start is recomputed to keep the playhead's
/// on-screen x fixed (mirrors the scroll-zoom-at-cursor path, anchored at the
/// playhead instead of the pointer). Zoom is view-only: like the scroll/pinch
/// path, it never detaches the playhead from live or ends a loop.
fn handle_zoom_step(
    _state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    ctx: &egui::Context,
) {
    let zoom_in = ctx.input(|i| i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus));
    let factor = if zoom_in {
        ZOOM_KEY_STEP
    } else {
        1.0 / ZOOM_KEY_STEP
    };
    let ps = &mut playback.state;
    let old_zoom = ps.timeline_zoom;
    let width = ps.timeline_width_px;
    // Clamp with the WIDTH-AWARE floor (not the loose hard min) so the
    // early-return check and the anchor math agree with the floor
    // `set_timeline_zoom` will actually store (mirrors `apply_zoom` in
    // timeline/interaction.rs).
    let new_zoom = (old_zoom * factor).clamp(
        crate::core::PlaybackState::min_zoom_for_width(width),
        crate::core::TIMELINE_ZOOM_MAX,
    );
    if new_zoom == old_zoom {
        return;
    }
    // Anchor at the playhead: keep its current on-screen offset fixed while the
    // scale changes.
    let playhead = ps.playback_position();
    ps.timeline_view_start =
        view_start_anchored_at(ps.timeline_view_start, old_zoom, new_zoom, playhead);
    ps.set_timeline_zoom(new_zoom, width);
}

/// I: the range starts at the playhead (`core::transport::reduce_loop_point`).
fn handle_loop_in(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    state.push_command(crate::core::Intent::SetLoopPoint(LoopEdge::In));
}

/// O: the range ends at the playhead.
fn handle_loop_out(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    state.push_command(crate::core::Intent::SetLoopPoint(LoopEdge::Out));
}

/// L go live (spec §12): a one-way re-tether, never a toggle that can stop the
/// stream. `ReturnToLive` instantly re-pins the playhead to the live edge when
/// a stream is already running in the background (detached browsing) and starts
/// a fresh stream otherwise — matching the now-cap REJOIN / GO-LIVE control.
fn handle_go_live(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    state.push_command(crate::core::Intent::ReturnToLive);
}

/// Shift+L stop live: the explicit inverse of L, kept on a separate combo so
/// go-live can never accidentally tear the stream down (L stays one-way).
/// Gated on an active stream so an idle press doesn't fire
/// `reduce_stop_live`'s status toast; `LiveEdge` placement matches the
/// now-cap and LIVE-button stops.
fn handle_stop_live(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    if live.mode_state.is_active() {
        state.push_command(crate::core::Intent::StopLive(
            crate::core::transport::LiveStopPlacement::LiveEdge,
        ));
    }
}

/// P / Shift+P: the next or previous product ([`crate::core::RadarProduct::step`]).
fn step_product(state: &mut AppState, forward: bool) {
    state.push_command(crate::core::Intent::TopBar(
        crate::core::top_bar::TopBarIntent::SetProduct(state.viz_state.product.step(forward)),
    ));
}

/// E / Shift+E: the next row of the one tilt list ([`crate::core::ladder`]),
/// Latest included, as the same pick a ladder row click makes.
fn step_tilt(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    sites: &crate::subsystem::Sites,
    playback: &mut crate::subsystem::Playback,
    down: bool,
) {
    let entries = state.current_elevation_list(
        &playback.state,
        &sites.focus().scans,
        sites
            .focus()
            .live
            .radar_model
            .volume
            .as_ref()
            .and_then(|v| v.vcp_pattern.as_ref()),
    );
    let rows = crate::core::ladder::tilt_rows(
        &state.viz_state.elevation_selection,
        &entries,
        state.viz_state.product,
    );
    if let Some(pick) = crate::core::ladder::step_tilt(&rows, down) {
        state.push_command(crate::core::Intent::TopBar(
            crate::core::top_bar::TopBarIntent::SetTilt(pick),
        ));
    }
}

fn handle_open_site(
    _state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    chrome.site_modal_open = true;
}

fn handle_reset_camera(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    if state.viz_state.is_2d() {
        state.viz_state.set_zoom(crate::geo::DEFAULT_FLAT_ZOOM);
        state.viz_state.set_pan_offset(egui::Vec2::ZERO);
    } else {
        state.viz_state.camera.reset();
    }
}

fn handle_focus_site(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _sites: &crate::subsystem::Sites,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    if state.viz_state.is_2d() {
        state.viz_state.set_pan_offset(egui::Vec2::ZERO);
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
    let (forward, right_move, rotate, speed_mult, dt) = ctx.input(|i| {
        let dt = i.stable_dt.min(0.1); // cap to avoid jumps
                                       // Arrows are now frame/scan step (spec §12) — camera movement is WASD
                                       // only (plus Q/E for heading rotation in 3D).
        let w = i.key_down(egui::Key::W) as i32 as f32;
        let a = i.key_down(egui::Key::A) as i32 as f32;
        let s = i.key_down(egui::Key::S) as i32 as f32;
        let d = i.key_down(egui::Key::D) as i32 as f32;
        let q = i.key_down(egui::Key::Q) as i32 as f32;
        let e = i.key_down(egui::Key::E) as i32 as f32;
        let forward = w - s;
        let right_move = d - a;
        let rotate = e - q;
        let speed_mult = if i.modifiers.shift {
            2.0
        } else if i.modifiers.command {
            0.25
        } else {
            1.0
        };
        (forward, right_move, rotate, speed_mult, dt)
    });

    if forward == 0.0 && right_move == 0.0 && rotate == 0.0 {
        return;
    }

    if state.viz_state.view_mode() == ViewMode::Globe3D {
        let moved = state
            .viz_state
            .camera
            .keyboard_move(forward, right_move, rotate, speed_mult, dt);
        if moved {
            ctx.request_repaint();
        }
    } else {
        // 2D mode: WASD pan the map.
        let pan_speed = 200.0 * speed_mult * dt;
        if let Some(pan) = state.viz_state.flat_pan_mut() {
            pan.x -= right_move * pan_speed;
            pan.y += forward * pan_speed;
        }
        ctx.request_repaint();
    }
}

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

/// Below this content width the help map stacks its sections in one column.
const TWO_COLUMN_MIN_WIDTH: f32 = 620.0;

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
        draw_shortcuts_help(ctx.ctx, ctx.state);
    }
}

fn draw_shortcuts_help(ctx: &egui::Context, state: &mut AppState) {
    let out = super::dialog::Dialog::new(DialogId::ShortcutsHelp, "Keyboard shortcuts")
        .subtitle("Shortcuts pause while a dialog is open; ? closes this one")
        .width(super::dialog::Width::Full)
        .show(ctx, |ui| {
            let max_h = ctx.input(|i| i.viewport_rect().height()) * 0.65;
            let is_3d = state.viz_state.view_mode() == ViewMode::Globe3D;
            let mac = ctx.os() == egui::os::OperatingSystem::Mac;
            egui::ScrollArea::vertical()
                .max_height(max_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    // Playback is as long as the other three sections together,
                    // so two columns fit the whole map without scrolling.
                    if ui.available_width() >= TWO_COLUMN_MIN_WIDTH {
                        ui.columns(2, |cols| {
                            let (first, rest) = SECTION_ORDER.split_at(1);
                            for section in first {
                                render_help_section(&mut cols[0], section, is_3d, mac);
                            }
                            for section in rest {
                                render_help_section(&mut cols[1], section, is_3d, mac);
                            }
                        });
                    } else {
                        for section in SECTION_ORDER {
                            render_help_section(ui, section, is_3d, mac);
                        }
                    }
                });
        });
    if out.close {
        state.push_command(crate::core::Intent::Dialog(DialogIntent::Close(
            DialogId::ShortcutsHelp,
        )));
    }
}

/// One section of the map: every shortcut, the ones inactive in this view
/// greyed (core::keymap), the modifier spelled the platform's way.
fn render_help_section(ui: &mut egui::Ui, section: &'static str, is_3d: bool, mac: bool) {
    let registry_entries = ONE_SHOT_SHORTCUTS
        .iter()
        .filter(|sc| sc.section == section)
        .map(|sc| (sc.key_label, sc.description, sc.scope));
    let doc_entries = HELD_KEY_DOCS
        .iter()
        .filter(|h| h.section == section)
        .map(|h| (h.key_label, h.description, h.scope));
    let rows = keymap_rows(doc_entries.chain(registry_entries), is_3d, mac);
    if rows.is_empty() {
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
            for row in rows {
                let (key_color, text_color) = if row.active {
                    (theme::TEXT, theme::TEXT_DIM)
                } else {
                    (theme::TEXT_FAINT, theme::TEXT_FAINT)
                };
                let key = ui.label(RichText::new(row.key).monospace().strong().color(key_color));
                let desc = ui.label(RichText::new(row.description).color(text_color));
                if !row.active {
                    let hint = if is_3d { "Works in 2D" } else { "Works in 3D" };
                    key.on_hover_text(hint);
                    desc.on_hover_text(hint);
                }
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

    /// Drive `handle_stop_live` directly with the real state bundle.
    fn run_stop_live(live_phase: crate::core::LivePhase) -> Vec<crate::core::Intent> {
        let mut state = crate::state::AppState::default();
        let mut live = crate::subsystem::Live::new();
        live.mode_state.phase = live_phase;
        let sites = crate::subsystem::Sites::default();
        let mut playback = crate::subsystem::Playback::default();
        let mut chrome = crate::subsystem::Chrome::new();
        let ctx = egui::Context::default();
        state.drain_commands(); // discard the bootstrap RefreshTimeline
        handle_stop_live(
            &mut state,
            &mut live,
            &sites,
            &mut playback,
            &mut chrome,
            &ctx,
        );
        state.drain_commands()
    }

    #[wasm_bindgen_test]
    fn stop_live_shortcut_emits_live_edge_stop_while_streaming() {
        let cmds = run_stop_live(crate::core::LivePhase::Streaming);
        assert_eq!(
            cmds,
            vec![crate::core::Intent::StopLive(
                crate::core::transport::LiveStopPlacement::LiveEdge
            )]
        );
    }

    #[wasm_bindgen_test]
    fn stop_live_shortcut_is_a_noop_when_idle() {
        // No stream → no intent, so an idle keypress cannot stop anything.
        assert!(run_stop_live(crate::core::LivePhase::Idle).is_empty());
    }

    #[wasm_bindgen_test]
    fn go_live_and_stop_live_are_separate_registry_entries() {
        // L must stay one-way (go-live) and Shift+L one-way (stop) — a single
        // toggle key was rejected as accidental-stop-prone.
        let labels: Vec<_> = ONE_SHOT_SHORTCUTS.iter().map(|s| s.key_label).collect();
        assert!(labels.contains(&"L"));
        assert!(labels.contains(&"Shift+L"));
    }

    #[wasm_bindgen_test]
    fn zoom_keeps_anchor_timestamp_pixel_fixed() {
        // The on-screen x of the anchor must not move across a zoom. Pixel x is
        // (anchor_ts - view_start) * zoom; assert it's invariant after the zoom.
        let view_start = 1000.0_f64;
        let anchor_ts = 1600.0_f64; // 600s right of the left edge
        let old_zoom = 0.5_f64;
        let px_before = (anchor_ts - view_start) * old_zoom;

        for &new_zoom in &[ZOOM_KEY_STEP * old_zoom, old_zoom / ZOOM_KEY_STEP, 4.0] {
            let vs = view_start_anchored_at(view_start, old_zoom, new_zoom, anchor_ts);
            let px_after = (anchor_ts - vs) * new_zoom;
            assert!(
                (px_after - px_before).abs() < 1e-6,
                "anchor pixel moved: before={px_before} after={px_after} new_zoom={new_zoom}"
            );
        }
    }

    #[wasm_bindgen_test]
    fn zoom_anchor_at_left_edge_keeps_view_start() {
        // Anchoring on the left edge (offset 0) must leave view_start unchanged
        // for any zoom — the degenerate case.
        let view_start = 1000.0_f64;
        let vs = view_start_anchored_at(view_start, 0.5, 2.0, view_start);
        assert!((vs - view_start).abs() < 1e-9);
    }
}
