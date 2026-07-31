//! Timeline strip interaction (spec §12 rows 1, 2, 4).
//!
//! Gesture grammar:
//!   - **Middle-drag** or **ctrl/cmd-drag** pans the view. This is the modeless
//!     pan: it never moves the playhead and never touches the selection. The
//!     minimap thumb and horizontal scroll are the other two pan surfaces.
//!   - **Primary press** seeks the playhead immediately (on press, not
//!     release); **primary drag** scrubs it continuously, following the
//!     pointer. Starting a scrub pauses playback and detaches the tether.
//!   - **Vertical scroll / pinch** zooms anchored at the cursor / pinch center
//!     (routed through the hysteretic tier machine).
//!   - **Alt-drag or right-drag** creates the loop/selection range; **shift**
//!     is kept as an established alias. Selection creation never seeks and the
//!     pan/zoom paths never clear a selection.
//!   - **Right-click** opens the scan inspector. (There is deliberately no
//!     long-press alias here: the strip shares one response across seek, scrub
//!     and select, so a long-press fired mid-scrub — see the UX audit. The
//!     mobile scrubber keeps its long-press, on its own dedicated response,
//!     because touch has no secondary button.)
//!
//! Which gesture *wins* a given frame is not decided here — it is
//! [`crate::core::resolve_gesture`], a pure function with unit tests. This
//! module only collects raw pointer facts and performs the resulting mutation.
//!
//! Every seek path detaches the playhead first (`live.detach_playhead`, which
//! keeps the stream ingesting) so `set_playback_position`'s Free-mode assert
//! holds.

use crate::core::{clamp_view_start, resolve_gesture, StripGesture, StripInput};
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

    let (shift_held, alt_held, cmd_held) = ui.input(|i| {
        (
            i.modifiers.shift,
            i.modifiers.alt,
            // egui's `command` is cmd on macOS and ctrl elsewhere — exactly the
            // platform-correct pan modifier, and unclaimed on every pointer
            // surface in this app.
            i.modifiers.command,
        )
    });
    let primary_pressed = ui.input(|i| i.pointer.primary_pressed());

    // A drag that BEGINS on a suppressed control (a loop handle whose hit rect
    // extends up into the strip) must not also scrub — the handle owns that
    // drag. We can't read the drag origin from egui's Response, so remember on
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
        secondary_down: response.dragged_by(PointerButton::Secondary)
            || response.drag_started_by(PointerButton::Secondary),
        secondary_clicked: response.clicked_by(PointerButton::Secondary),
        middle_dragging: response.dragged_by(PointerButton::Middle),
        clicked: response.clicked(),
        selection_mod: shift_held || alt_held,
        pan_mod: cmd_held,
        selection_in_progress: playback.state.selection_in_progress(),
        on_suppressed_rect: pointer_pos
            .is_some_and(|p| suppress_rects.iter().any(|r| r.contains(p))),
        scrub_suppressed,
        archive_tier: frame.tier == crate::core::TimelineTier::Archive,
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

        // -- Loop / selection range ------------------------------------------
        StripGesture::Select => {
            handle_selection(state, live, playback, response, frame, &input);
        }

        // -- Scan inspector (right-click) -------------------------------------
        StripGesture::Inspect => {
            if let Some(pos) = pointer_pos {
                open_inspector_at(state, chrome, frame, pos.x);
            }
        }

        // -- Press-seek and drag-scrub ---------------------------------------
        StripGesture::Seek => {
            // The first frame of a scrub pauses playback (so the playhead stays
            // where the user puts it) and detaches the tether.
            if input.primary_drag_started {
                playback.state.playing = false;
                live.detach_playhead(
                    &mut playback.state,
                    state.frame_now.secs(),
                    state.pause_stream_while_reviewing,
                );
            }
            if let Some(pos) = pointer_pos {
                seek_to(state, live, playback, frame.x_to_ts(pos.x));
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
            apply_zoom(state, live, playback, frame, t.zoom as f64, t.focus.x);
            pinched = true;
        }
    }

    // -- Scroll: vertical zooms, horizontal pans -----------------------------
    // (Skip when a pinch already handled this frame's zoom.)
    if response.hovered() && !pinched {
        let scroll = ui.input(|i| i.raw_scroll_delta);
        if scroll.y != 0.0 {
            let zoom_factor = 1.0 + scroll.y as f64 * 0.002;
            let anchor_x = response
                .hover_pos()
                .map(|p| p.x)
                .unwrap_or(frame.rects.scan.left());
            apply_zoom(state, live, playback, frame, zoom_factor, anchor_x);
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

/// Run the loop/selection-range lifecycle for a frame the arbitration awarded
/// to [`StripGesture::Select`]: the modifier click-to-extend, drag start,
/// drag update, and the finalize-on-release that arms the bulk fetch.
fn handle_selection(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    response: &egui::Response,
    frame: &super::TimelineFrame<'_>,
    input: &StripInput,
) {
    // Click variant: shift/alt-click stretches a range from the playhead to the
    // click. (Right-click has no click variant — it would clash with a future
    // context menu; only right-DRAG selects.)
    if input.selection_mod && input.clicked && !input.selection_in_progress {
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
            log::debug!("Range from playhead: {:.0} minutes", duration_mins);
        }
    }

    // A selection drag begins on either the selection modifier (primary) or the
    // secondary button. Once in progress it is owned by `selection_in_progress`
    // and tracked regardless of which trigger started it.
    let drag_started = (input.selection_mod && input.primary_drag_started) || input.secondary_down;
    if drag_started && !playback.state.selection_in_progress() {
        if let Some(pos) = response.interact_pointer_pos() {
            playback.state.begin_selection_drag(frame.x_to_ts(pos.x));
        }
    }

    if playback.state.selection_in_progress() && response.dragged() {
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
        state.status_message = "No scan to inspect at that time".to_string();
    }
}

/// Seek the playhead to `ts`, detaching the tether first so the Free-mode
/// invariant on `set_playback_position` holds. Selection survival: a seek
/// *inside* the selection keeps the loop bounds; a seek outside clears it.
fn seek_to(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    ts: f64,
) {
    live.detach_playhead(
        &mut playback.state,
        state.frame_now.secs(),
        state.pause_stream_while_reviewing,
    );
    let inside = playback.state.selection_contains(ts);
    playback.state.set_playback_position(ts);
    if !inside {
        playback.state.clear_selection();
    }
}

/// Apply a multiplicative zoom anchored at screen-x `anchor_x`, keeping the
/// timestamp under that x fixed. Zooming out of the Micro tier while tethered
/// detaches (the stream keeps running) rather than fighting the tether. The
/// single zoom-mutation path (`set_timeline_zoom`) advances the tier with
/// hysteresis and preserves playback cadence on a behavioral flip.
fn apply_zoom(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    frame: &super::TimelineFrame<'_>,
    zoom_factor: f64,
    anchor_x: f32,
) {
    let old_zoom = playback.state.timeline_zoom;
    let width = playback.state.timeline_width_px;
    // Clamp with the WIDTH-AWARE floor (not the loose hard min) so the
    // zoom-would-exit-micro check and the anchor math below agree with the
    // floor `set_timeline_zoom` will apply — the widest view stays the readable
    // calendar span, never the deprecated year-wide strip.
    let new_zoom = (old_zoom * zoom_factor).clamp(
        crate::core::PlaybackState::min_zoom_for_width(width),
        crate::core::TIMELINE_ZOOM_MAX,
    );

    let attached = playback.state.time_model.is_pinned() || playback.state.time_model.is_lookback();
    if attached && playback.state.zoom_would_exit_micro(new_zoom, width) {
        live.detach_playhead(
            &mut playback.state,
            state.frame_now.secs(),
            state.pause_stream_while_reviewing,
        );
    }

    // Keep the timestamp under the anchor fixed while scaling.
    let anchor_ts = frame.x_to_ts(anchor_x);
    let new_view_start = anchor_ts - (anchor_x - frame.rects.scan.left()) as f64 / new_zoom;
    playback.state.timeline_view_start = new_view_start;

    let spacing = playback.state.median_frame_spacing();
    playback.state.set_timeline_zoom(new_zoom, width, spacing);
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
