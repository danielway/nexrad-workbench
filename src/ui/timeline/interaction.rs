//! Timeline strip interaction (spec §12 rows 1, 2, 4).
//!
//! Gesture grammar:
//!   - **Middle-drag** or **ctrl/cmd-drag** pans the view. This is the modeless
//!     pan: it never moves the playhead and never touches the selection. The
//!     overview's view box and horizontal scroll are the other two pan
//!     surfaces.
//!   - **Primary press** seeks the playhead immediately (on press, not
//!     release); **primary drag** scrubs it continuously, following the
//!     pointer. Starting a scrub pauses playback and detaches the tether.
//!   - **Vertical scroll / pinch** zooms anchored at the cursor / pinch center
//!     (routed through the hysteretic tier machine).
//!   - Ranges are made on the selection strip above the frames
//!     (`super::selection_strip`), not here; pan and zoom never clear one.
//!   - **Right-click** opens the scan inspector. (There is deliberately no
//!     long-press alias here: the strip shares one response across seek and
//!     scrub, so a long-press fired mid-scrub — see the UX audit. The
//!     phone's mini timeline has its long-press, decided by
//!     `core::phone::PressTrack`, because touch has no secondary button.)
//!
//! Which gesture *wins* a given frame is not decided here — it is
//! [`crate::core::resolve_gesture`], a pure function with unit tests. This
//! module only collects raw pointer facts and performs the resulting mutation.
//!
//! What a seek does is [`crate::core::transport::reduce_seek`]: a press is
//! `Intent::Seek { gesture: Press }`, a scrub runs the same reducer
//! synchronously through `Live::seek` (a direct-manipulation gesture must not
//! trail the pointer by a frame).

use crate::core::transport::SeekGesture;
use crate::core::{clamp_view_start, resolve_gesture, Intent, StripGesture, StripInput};
use crate::state::AppState;
use eframe::egui::{self, PointerButton, Rect};

/// Result of one frame of strip interaction, surfaced to the renderer so it can
/// suppress conflicting affordances (tooltips during a scrub).
#[derive(Clone, Copy, Default)]
pub(super) struct InteractionOutcome {
    /// A primary-drag scrub is active this frame — suppress hover tooltips and
    /// press-seek re-triggering.
    pub scrubbing: bool,
}

/// Handle mouse / touch interaction on the timeline strip.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_timeline_interaction(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    response: &egui::Response,
    frame: &super::TimelineFrame<'_>,
    // Rects whose clicks/presses belong to another control (now-affordance
    // cap/chip, failed-cell ticks) — a press inside any of them never seeks.
    suppress_rects: &[Rect],
) -> InteractionOutcome {
    let mut outcome = InteractionOutcome::default();

    // egui's `command` is cmd on macOS and ctrl elsewhere — exactly the
    // platform-correct pan modifier, and unclaimed on every pointer surface in
    // this app.
    let cmd_held = ui.input(|i| i.modifiers.command);
    let primary_pressed = ui.input(|i| i.pointer.primary_pressed());

    // A drag that BEGINS on a suppressed control (the now cap) must not also
    // scrub — the control owns that drag. We can't read the drag origin from egui's Response, so remember on
    // the start frame whether the press landed in a suppress rect, keyed in
    // memory for the drag's lifetime.
    let drag_lock_id = response.id.with("scrub_suppressed");
    if response.drag_started_by(PointerButton::Primary) {
        let on_suppressed = response
            .interact_pointer_pos()
            .is_some_and(|p| suppress_rects.iter().any(|r| r.contains(p)));
        ui.ctx()
            .memory_mut(|m| m.data.insert_temp(drag_lock_id, on_suppressed));
    }
    if response.drag_stopped() {
        ui.ctx().memory_mut(|m| m.data.remove::<bool>(drag_lock_id));
    }
    let scrub_suppressed = ui
        .ctx()
        .memory(|m| m.data.get_temp::<bool>(drag_lock_id))
        .unwrap_or(false);

    let pointer_pos = response.interact_pointer_pos();
    let input = StripInput {
        primary_pressed: primary_pressed && response.is_pointer_button_down_on(),
        primary_drag_started: response.drag_started_by(PointerButton::Primary),
        primary_dragging: response.dragged_by(PointerButton::Primary),
        secondary_clicked: response.clicked_by(PointerButton::Secondary),
        middle_dragging: response.dragged_by(PointerButton::Middle),
        pan_mod: cmd_held,
        on_suppressed_rect: pointer_pos
            .is_some_and(|p| suppress_rects.iter().any(|r| r.contains(p))),
        scrub_suppressed,
    };
    let gesture = resolve_gesture(&input);

    match gesture {
        // -- Pan (middle-drag / ctrl-drag) -----------------------------------
        // Content follows the pointer: dragging right moves the view earlier.
        // Clamped to the addressable era so a flick at the widest zoom can't
        // strand the view centuries away.
        StripGesture::Pan => {
            let delta_x = ui.input(|i| i.pointer.delta().x);
            if delta_x != 0.0 && frame.zoom > 0.0 {
                pan_view(state, playback, -delta_x as f64 / frame.zoom);
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }

        // -- Scan inspector (right-click) -------------------------------------
        StripGesture::Inspect => {
            if let Some(pos) = pointer_pos {
                open_inspector_at(state, chrome, frame, pos.x);
            }
        }

        // -- Press-seek and drag-scrub ---------------------------------------
        // What a seek does (keep a range it lands in, keep a parked I point,
        // detach from now, pause on a scrub) is core::transport::reduce_seek.
        // A press is an intent; a scrub runs the same reducer synchronously
        // so the playhead keeps up with the pointer.
        StripGesture::Seek => {
            if let Some(pos) = pointer_pos {
                let ts = frame.x_to_ts(pos.x);
                if input.primary_dragging || input.primary_drag_started {
                    live.seek(
                        &mut playback.state,
                        state.frame_now.secs(),
                        state.pause_stream_while_reviewing,
                        ts,
                        SeekGesture::Scrub,
                    );
                } else {
                    state.push_command(Intent::Seek {
                        ts,
                        gesture: SeekGesture::Press,
                    });
                }
            }
            outcome.scrubbing = input.primary_dragging;
        }

        StripGesture::None => {}
    }

    // A scrub in progress suppresses the reactive prefetch's debounce-free
    // anchor fast path, so dragging the playhead across the archive doesn't
    // fire a download per scan crossed. Read next frame by
    // `pump_implicit_prefetch` (which runs before the UI renders).
    state.pointer_scrub_active = outcome.scrubbing;

    // -- Pinch zoom (touch, anchored at the pinch center) --------------------
    // Consume egui multi-touch only when the pinch is over the timeline rect, so
    // the canvas doesn't also zoom from the same gesture (the canvas mirrors
    // this by skipping pinches whose focus lands here). Routed through
    // `set_timeline_zoom` so the tier hysteresis applies.
    let mut pinched = false;
    if let Some(t) = crate::ui::mobile::gestures::consume(ui.ctx()) {
        if frame.rects.scan.contains(t.focus) && (t.zoom - 1.0).abs() > f32::EPSILON {
            apply_zoom(playback, frame, t.zoom as f64, t.focus.x);
            pinched = true;
        }
    }

    // -- Scroll: vertical zooms, horizontal pans -----------------------------
    // (Skip when a pinch already handled this frame's zoom.)
    if response.hovered() && !pinched {
        let scroll = ui.input(|i| i.smooth_scroll_delta);
        if scroll.y != 0.0 {
            let zoom_factor = 1.0 + scroll.y as f64 * 0.002;
            let anchor_x = response
                .hover_pos()
                .map(|p| p.x)
                .unwrap_or(frame.rects.scan.left());
            apply_zoom(playback, frame, zoom_factor, anchor_x);
        }
        // Horizontal scroll (trackpad two-finger) pans the view. Never clears
        // the selection (spec: pan/zoom preserve loop bounds).
        if scroll.x != 0.0 && frame.zoom > 0.0 {
            pan_view(state, playback, -scroll.x as f64 / frame.zoom);
        }
    }

    outcome
}

/// Shift the timeline view by `delta_secs`, clamped to the addressable archive
/// era. The single pan-write seam: every pan surface on the strip routes here
/// so the clamp and the view-follow release can't be forgotten at one of them.
fn pan_view(state: &AppState, playback: &mut crate::subsystem::Playback, delta_secs: f64) {
    // A deliberate pan releases the live view-follow nudge, which would
    // otherwise snap the view back to "now" on the very next frame.
    playback.state.view_follows_now = false;
    let span = playback.state.view_width_secs();
    playback.state.timeline_view_start = clamp_view_start(
        playback.state.timeline_view_start + delta_secs,
        span,
        crate::core::NEXRAD_ARCHIVE_START_SECS,
        state.frame_now.secs(),
    );
}

/// Open the scan inspector for the scan under screen-x `x`. Resolves the
/// timestamp to a scan-start via the merged view (`scan_start_at`) — the same
/// source the strip's containers come from — and stores it as the open flag.
/// A no-op over empty timeline (nothing to inspect).
fn open_inspector_at(
    state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
    frame: &super::TimelineFrame<'_>,
    x: f32,
) {
    let ts = frame.x_to_ts(x);
    if let Some(scan_start) = frame.view.scan_start_at(ts) {
        chrome.scan_inspector = Some(scan_start);
    } else {
        // No scan there — surface a tiny hint instead of silently doing nothing.
        state.toasts.notice("No scan to inspect at that time");
    }
}

/// Apply a multiplicative zoom anchored at screen-x `anchor_x`, keeping the
/// timestamp under that x fixed. Zoom is view-only: it never detaches the
/// playhead from live, ends a loop, or changes the Frames/Rays mode. The single
/// zoom-mutation path (`set_timeline_zoom`) advances the tier with
/// hysteresis.
fn apply_zoom(
    playback: &mut crate::subsystem::Playback,
    frame: &super::TimelineFrame<'_>,
    zoom_factor: f64,
    anchor_x: f32,
) {
    let old_zoom = playback.state.timeline_zoom;
    let width = playback.state.timeline_width_px;
    // Clamp with the WIDTH-AWARE floor (not the loose hard min) so the anchor
    // math below agrees with the floor `set_timeline_zoom` will apply (the
    // strip shows at most three days).
    let new_zoom = (old_zoom * zoom_factor).clamp(
        crate::core::PlaybackState::min_zoom_for_width(width),
        crate::core::TIMELINE_ZOOM_MAX,
    );

    // Keep the timestamp under the anchor fixed while scaling (spans per px).
    playback.state.timeline_view_start = crate::core::zoom_about(
        frame.x_to_ts(anchor_x),
        frame.view_start,
        1.0 / frame.zoom,
        1.0 / new_zoom,
    );
    playback.state.set_timeline_zoom(new_zoom, width);
}
