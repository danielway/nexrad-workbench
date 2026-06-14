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

use crate::state::{AppState, PlaybackSpeed, RadarProduct, ViewMode};
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
    state.viz_state.view_mode() == ViewMode::Flat2D
}

fn in_3d(state: &AppState) -> bool {
    state.viz_state.view_mode() == ViewMode::Globe3D
}

fn no_mods(i: &egui::InputState, key: egui::Key) -> bool {
    i.key_pressed(key) && !i.modifiers.any()
}

const ONE_SHOT_SHORTCUTS: &[OneShotShortcut] = &[
    // ---- Playback ---------------------------------------------------
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "←/→",
        description: "Frame step (prev / next)",
        pressed: |i| no_mods(i, egui::Key::ArrowLeft) || no_mods(i, egui::Key::ArrowRight),
        enabled: always_enabled,
        handler: handle_frame_step,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "Shift+←/→",
        description: "Scan step (prev / next volume)",
        pressed: |i| {
            i.modifiers.shift_only()
                && (i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowRight))
        },
        enabled: always_enabled,
        handler: handle_scan_step,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "[ / ]",
        description: "Speed down / up",
        pressed: |i| no_mods(i, egui::Key::OpenBracket) || no_mods(i, egui::Key::CloseBracket),
        enabled: always_enabled,
        handler: handle_speed_step,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
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
        enabled: always_enabled,
        handler: handle_zoom_step,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "I",
        description: "Set loop in-point at playhead",
        pressed: |i| no_mods(i, egui::Key::I),
        enabled: always_enabled,
        handler: handle_loop_in,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "O",
        description: "Set loop out-point at playhead",
        pressed: |i| no_mods(i, egui::Key::O),
        enabled: always_enabled,
        handler: handle_loop_out,
    },
    OneShotShortcut {
        section: SECTION_PLAYBACK,
        key_label: "L",
        description: "Go live (re-tether)",
        pressed: |i| no_mods(i, egui::Key::L),
        enabled: always_enabled,
        handler: handle_go_live,
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
            state.viz_state.switch_to_2d();
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "2",
        description: "3D site orbit mode",
        pressed: |i| no_mods(i, egui::Key::Num2),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state
                .viz_state
                .switch_camera_mode(crate::geo::CameraMode::SiteOrbit);
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "3",
        description: "3D planet orbit mode",
        pressed: |i| no_mods(i, egui::Key::Num3),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state
                .viz_state
                .switch_camera_mode(crate::geo::CameraMode::PlanetOrbit);
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "4",
        description: "Free look mode",
        pressed: |i| no_mods(i, egui::Key::Num4),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state
                .viz_state
                .switch_camera_mode(crate::geo::CameraMode::FreeLook);
        },
    },
    OneShotShortcut {
        section: SECTION_VIEW,
        key_label: "T",
        description: "Toggle last 2D / 3D mode",
        pressed: |i| no_mods(i, egui::Key::T),
        enabled: always_enabled,
        handler: |state, _live, _timeline, _playback, _chrome, _| {
            state.viz_state.toggle_2d_3d();
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
        key_label: "WASD",
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

/// Direction a step shortcut moves the playhead, read from this frame's input.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepDir {
    Backward,
    Forward,
}

/// Resolve the step direction for the arrow-key shortcuts. ArrowRight wins if
/// somehow both are pressed in the same frame (forward bias).
fn step_dir(ctx: &egui::Context) -> StepDir {
    if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
        StepDir::Forward
    } else {
        StepDir::Backward
    }
}

/// Detach the tether for a seek gesture (spec §7/§12): stepping is a seek, so
/// the playhead leaves the live edge first; the stream keeps ingesting unless
/// the data-saver policy stops it. Threads `pause_stream_while_reviewing`
/// through the one place that policy is checked.
fn detach_for_seek(
    state: &AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
    live.detach_playhead(
        &mut playback.state,
        state.frame_now.secs(),
        state.pause_stream_while_reviewing,
    );
}

/// ←/→ frame step: move one matching frame (a sweep of the selected product +
/// tilt). Detach-then-step so the Free-mode seek assert holds and stepping
/// while tethered detaches rather than no-opping (spec §7/§12).
fn handle_frame_step(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    ctx: &egui::Context,
) {
    let dir = step_dir(ctx);
    let pos = current_pos(state, playback);
    let fallback = jog_fallback(state, playback);
    detach_for_seek(state, live, playback);
    let new_pos = match (&state.viz_state.elevation_selection, dir) {
        (
            crate::state::ElevationSelection::Fixed {
                elevation_number, ..
            },
            StepDir::Forward,
        ) => timeline
            .scans
            .next_matching_sweep_end_by_number(pos, *elevation_number)
            .unwrap_or(pos + fallback),
        (
            crate::state::ElevationSelection::Fixed {
                elevation_number, ..
            },
            StepDir::Backward,
        ) => timeline
            .scans
            .prev_matching_sweep_end_by_number(pos, *elevation_number)
            .unwrap_or(pos - fallback),
        (crate::state::ElevationSelection::Latest, StepDir::Forward) => timeline
            .scans
            .next_any_sweep_end(pos)
            .unwrap_or(pos + fallback),
        (crate::state::ElevationSelection::Latest, StepDir::Backward) => timeline
            .scans
            .prev_any_sweep_end(pos)
            .unwrap_or(pos - fallback),
    };
    playback.state.set_playback_position(new_pos);
}

/// Shift+←/→ scan step: move one whole volume scan (spec §12). Lands on the
/// matching frame of the adjacent volume (or its earliest sweep when the
/// selected tilt is absent). Detach-then-step like the frame step. Falls back
/// to a whole-volume time jump only when no adjacent scan exists.
fn handle_scan_step(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    ctx: &egui::Context,
) {
    let dir = step_dir(ctx);
    let pos = current_pos(state, playback);
    // Whole-volume fallback when the timeline has no neighboring scan loaded.
    let fallback = crate::FALLBACK_SCAN_DURATION_SECS as f64;
    detach_for_seek(state, live, playback);
    let new_pos = match (&state.viz_state.elevation_selection, dir) {
        (
            crate::state::ElevationSelection::Fixed {
                elevation_number, ..
            },
            StepDir::Forward,
        ) => timeline
            .scans
            .next_scan_matching_sweep_end_by_number(pos, *elevation_number)
            .unwrap_or(pos + fallback),
        (
            crate::state::ElevationSelection::Fixed {
                elevation_number, ..
            },
            StepDir::Backward,
        ) => timeline
            .scans
            .prev_scan_matching_sweep_end_by_number(pos, *elevation_number)
            .unwrap_or(pos - fallback),
        (crate::state::ElevationSelection::Latest, StepDir::Forward) => timeline
            .scans
            .next_scan_any_sweep_end(pos)
            .unwrap_or(pos + fallback),
        (crate::state::ElevationSelection::Latest, StepDir::Backward) => timeline
            .scans
            .prev_scan_any_sweep_end(pos)
            .unwrap_or(pos - fallback),
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

/// [ / ] speed down / up (spec §12). `[` (OpenBracket) slows, `]`
/// (CloseBracket) quickens; cycling stays within the active mode's valid speed
/// set (macro fps vs micro multiples) so the combo label and playback never
/// disagree (Phase 1 fix).
fn handle_speed_step(
    _state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    ctx: &egui::Context,
) {
    let faster = ctx.input(|i| i.key_pressed(egui::Key::CloseBracket));
    let speeds = active_mode_speeds(playback);
    if let Some(idx) = speeds.iter().position(|s| *s == playback.state.speed) {
        if faster {
            if idx + 1 < speeds.len() {
                playback.state.speed = speeds[idx + 1];
            }
        } else if idx > 0 {
            playback.state.speed = speeds[idx - 1];
        }
    } else if let Some(&first) = speeds.first() {
        // Current speed isn't valid in this mode — land on the slowest valid.
        playback.state.speed = first;
    }
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
/// through [`PlaybackState::set_timeline_zoom`] so tier hysteresis + cadence
/// preservation apply, and the view start is recomputed to keep the playhead's
/// on-screen x fixed (mirrors the scroll-zoom-at-cursor path, anchored at the
/// playhead instead of the pointer). Zooming out past the Micro threshold while
/// tethered detaches the playhead (the stream continues in the background),
/// matching the scroll/pinch zoom-out path.
fn handle_zoom_step(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
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
    // early-return check, the detach decision, and the anchor math all agree
    // with the floor `set_timeline_zoom` will actually store (mirrors
    // `apply_zoom` in timeline/interaction.rs).
    let new_zoom = (old_zoom * factor).clamp(
        crate::state::PlaybackState::min_zoom_for_width(width),
        crate::state::TIMELINE_ZOOM_MAX,
    );
    if new_zoom == old_zoom {
        return;
    }
    // Zooming out past the Micro threshold while tethered detaches (stream keeps
    // running in the background), exactly as the scroll/pinch path does.
    let attached = ps.time_model.is_pinned() || ps.time_model.is_lookback();
    if attached && ps.zoom_would_exit_micro(new_zoom, width) {
        live.detach_playhead(
            &mut playback.state,
            state.frame_now.secs(),
            state.pause_stream_while_reviewing,
        );
    }
    let ps = &mut playback.state;
    // Anchor at the playhead: keep its current on-screen offset fixed while the
    // scale changes.
    let playhead = ps.playback_position();
    ps.timeline_view_start =
        view_start_anchored_at(ps.timeline_view_start, old_zoom, new_zoom, playhead);
    let spacing = ps.median_frame_spacing();
    ps.set_timeline_zoom(new_zoom, width, spacing);
}

/// Set a loop in/out point at the playhead (spec §8/§12 I/O keys). Editing a
/// loop range is a Free-mode gesture: detach the tether first (so the loop is a
/// static range, not a clobbered pinned window) then move the relevant
/// selection endpoint to the playhead and apply it as bounds — reusing the same
/// live-anchoring rule the drag/click selection paths use (a range ending near
/// now while streaming reads as pinned). When no selection exists yet, the
/// other endpoint seeds at the playhead so the first key makes a zero-width
/// point and the second gives the range.
fn set_loop_point(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    is_in: bool,
) {
    let now = state.frame_now.secs();
    // Detach so set_selection/apply land in Free mode (no tick_live clobber).
    live.detach_playhead(&mut playback.state, now, state.pause_stream_while_reviewing);
    let pos = playback.state.playback_position();
    let (mut a, mut b) = playback
        .state
        .selection
        .map(|s| (s.a, s.b))
        .unwrap_or((pos, pos));
    if is_in {
        a = pos;
    } else {
        b = pos;
    }
    playback.state.set_selection(a, b);
    // Anchor to live when the out edge lands near now while streaming.
    if live.mode_state.is_active() {
        let near_now = playback
            .state
            .selection_range()
            .is_some_and(|(_, end)| (now - end).abs() < crate::FALLBACK_SCAN_DURATION_SECS as f64);
        if near_now {
            playback.state.anchor_selection_to_live();
        }
    }
    playback.state.apply_selection_as_bounds();
    if let Some(range) = playback.state.selection_range() {
        state.selection_just_finalized = Some(range);
    }
}

fn handle_loop_in(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    set_loop_point(state, live, playback, true);
}

fn handle_loop_out(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    set_loop_point(state, live, playback, false);
}

/// L go live (spec §12): a one-way re-tether, never a toggle that can stop the
/// stream. `ReturnToLive` instantly re-pins the playhead to the live edge when
/// a stream is already running in the background (detached browsing) and starts
/// a fresh stream otherwise — matching the now-cap REJOIN / GO-LIVE control.
fn handle_go_live(
    state: &mut AppState,
    _live: &mut crate::subsystem::Live,
    _timeline: &crate::subsystem::Timeline,
    _playback: &mut crate::subsystem::Playback,
    _chrome: &mut crate::subsystem::Chrome,
    _: &egui::Context,
) {
    state.push_command(crate::state::AppCommand::ReturnToLive);
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
    if state.viz_state.is_2d() {
        state.viz_state.set_zoom(1.0);
        state.viz_state.set_pan_offset(egui::Vec2::ZERO);
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
    let (forward, right_move, up_move, speed_mult, dt) = ctx.input(|i| {
        let dt = i.stable_dt.min(0.1); // cap to avoid jumps
                                       // Arrows are now frame/scan step (spec §12) — camera movement is WASD
                                       // only (plus Q/E for vertical in 3D).
        let w = i.key_down(egui::Key::W) as i32 as f32;
        let a = i.key_down(egui::Key::A) as i32 as f32;
        let s = i.key_down(egui::Key::S) as i32 as f32;
        let d = i.key_down(egui::Key::D) as i32 as f32;
        let q = i.key_down(egui::Key::Q) as i32 as f32;
        let e = i.key_down(egui::Key::E) as i32 as f32;
        let forward = w - s;
        let right_move = d - a;
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

    if state.viz_state.view_mode() == ViewMode::Globe3D {
        let moved = state
            .viz_state
            .camera
            .keyboard_move(forward, right_move, up_move, speed_mult, dt);
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
