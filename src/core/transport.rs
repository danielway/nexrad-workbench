//! Pure transport reducers — play/pause, seek, step, in/out points, loop
//! style, speed, playhead detach, and stop-live — plus the transport row's
//! view-model ([`transport_vm`]).
//!
//! One set of rules for every surface (the transport buttons, the arrow keys,
//! the phone's jog buttons, the playhead clock, the timeline):
//!
//! - **Seek** ([`reduce_seek`]): a press or a typed time seeks and does not
//!   pause; a scrub pauses. A seek inside a range keeps the range; a seek
//!   outside it turns the selection into a point. Off the live edge either way.
//! - **Step** ([`reduce_step`]): the next or previous frame of the selected
//!   product and tilt (a whole volume with Shift, or in volumetric 3D),
//!   wrapping only inside a range. Stepping pauses. Forward from the newest
//!   frame is a no-op rather than a jump into the future.
//! - **In / out** ([`reduce_loop_point`]): I and O move one edge of the range
//!   to the playhead; the first press parks a half range.
//! - **Ends at now** ([`ends_at_now`]): a range whose end lands within one
//!   frame of now pins to now while a stream runs. The strip, I/O and the
//!   chip menu all reach it through [`select_range`].
//! - **Ranges start paused**: drawing, I/O, presets and "loop what's in view"
//!   never start playback. Only pinning to now does.
//! - **Never disabled**: every control always emits its intent; the reducers
//!   decide what it means in the current state, and may no-op.
//!
//! These are the decisions the UI used to make inline (`ui/transport.rs`'s
//! `toggle_play_pause`, `ui/timeline/now_edge.rs`'s `stop_live`) and the one
//! `Live::detach_playhead` owned. They all operate on the same three pieces of
//! core state — the live state machine, the projection engine's volume
//! observations, and the playback state — and describe the rest.
//!
//! The shell (`app::command_dispatch` via [`crate::core::Intent`], plus
//! [`crate::subsystem::Live`] for the detach helper) assembles
//! [`TransportSlices`] over that core state, calls a reducer, and executes the
//! returned [`TransportActions`] in field order. Nothing here touches the
//! worker channel, egui, or the browser.
//!
//! Pattern exemplar: [`crate::core::worker_ingest`] (Env / Slices / Actions).

use crate::core::projection::ProjectionEngine;
use crate::core::{
    FreezeAt, LiveExitReason, LiveModeState, LoopMode, PlaybackMode, PlaybackSpeed, PlaybackState,
    RadarTimeline,
};

/// Read-only frame context for one transport gesture.
pub(crate) struct TransportEnv {
    /// `state.frame_now.secs()` — this frame's wall clock.
    pub now_secs: f64,
    /// The data-saver policy: stop the background stream the moment the user
    /// starts reviewing.
    pub pause_stream_while_reviewing: bool,
    /// One frame of the selected product and tilt (seconds): the "ends at
    /// now" tolerance. `PlaybackState::median_frame_spacing`.
    pub frame_secs: f64,
}

/// Mutable borrows of the core state the transport reducers update directly.
pub(crate) struct TransportSlices<'a> {
    pub live_mode: &'a mut LiveModeState,
    pub engine: &'a mut ProjectionEngine,
    pub playback: &'a mut PlaybackState,
}

/// Described effects the shell executes, in this field order.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct TransportActions {
    /// `live.channel.stop()` — tear down the realtime worker stream.
    pub stop_channel: bool,
    /// Assign `state.status_message`.
    pub status_message: Option<String>,
}

/// Stop the live state machine: reset the phase/exit reason and drop the
/// projection engine's volume observations together (the engine owns the
/// observations, so the state-machine clear must also clear them).
///
/// The subsystem wrapper is [`crate::subsystem::Live::stop`], which is the only
/// thing that also owns the (non-core) worker channel.
fn stop_live_machine(
    live_mode: &mut LiveModeState,
    engine: &mut ProjectionEngine,
    reason: LiveExitReason,
) {
    live_mode.stop(reason);
    engine.reset_volume_observations();
}

/// Detach the playhead from the live edge — the decision behind
/// [`crate::subsystem::Live::detach_playhead`], which is what every seek / jog
/// / jump gesture does while live.
///
/// By default (`pause_stream_while_reviewing == false`) the stream keeps
/// ingesting at the right edge and the now-cap offers an instant return; it
/// only stops on an explicit stop, an error, a site change, or the detached
/// idle timeout. When the data-saver policy is on, detaching stops the
/// background stream immediately — this is the ONE place that policy is
/// checked, so every seek/jog/jump call site routes through it. No-op on the
/// stream when already detached or not streaming.
pub(crate) fn detach_playhead(env: &TransportEnv, slices: TransportSlices<'_>) -> TransportActions {
    let TransportSlices {
        live_mode,
        engine,
        playback,
    } = slices;

    playback.exit_live(FreezeAt::Keep);
    if !live_mode.is_active() {
        return TransportActions::default();
    }
    if env.pause_stream_while_reviewing {
        // Data-saver: stop the moment the user starts reviewing.
        stop_live_machine(live_mode, engine, LiveExitReason::UserStopped);
        return TransportActions {
            stop_channel: true,
            status_message: None,
        };
    }
    if live_mode.detached_since.is_none() {
        live_mode.detached_since = Some(env.now_secs);
    }
    TransportActions::default()
}

/// The "ends at now" tolerance when no frame spacing is known: one volume.
pub(crate) const LIVE_EDGE_SNAP_SECS: f64 = 300.0;

/// The one "ends at now" rule: a range end within one frame (`frame_secs`) of
/// now reads as "up to now" and pins. A non-positive spacing falls back to
/// [`LIVE_EDGE_SNAP_SECS`].
pub(crate) fn ends_at_now(end: f64, now: f64, frame_secs: f64) -> bool {
    let tolerance = if frame_secs.is_finite() && frame_secs > 0.0 {
        frame_secs
    } else {
        LIVE_EDGE_SNAP_SECS
    };
    now - end < tolerance
}

/// Select a range from any surface (the strip, I/O, the chip menu). It pins
/// to now when the caller asks (`to_live`, the strip's handle dropped on the
/// live edge) or when its end [`ends_at_now`], and a stream runs: the playhead
/// stays attached and the live tick slides the window forward with now.
/// Otherwise the playhead detaches (see [`detach_playhead`]) and the range is
/// fixed. A new range starts paused. Returns the detach's actions and whether
/// a range was selected (it must span over a second).
pub(crate) fn select_range(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
    a: f64,
    b: f64,
    to_live: bool,
) -> (TransportActions, bool) {
    let pin = to_live || ends_at_now(a.max(b), env.now_secs, env.frame_secs);
    if pin && slices.live_mode.is_active() {
        slices.live_mode.detached_since = None;
        let selected = slices.playback.select_range(a, b, true);
        if selected {
            slices.playback.playing = false;
        }
        return (TransportActions::default(), selected);
    }
    let TransportSlices {
        live_mode,
        engine,
        playback,
    } = slices;
    let actions = detach_playhead(
        env,
        TransportSlices {
            live_mode,
            engine,
            playback: &mut *playback,
        },
    );
    let selected = playback.select_range(a, b, false);
    if selected {
        playback.playing = false;
    }
    (actions, selected)
}

/// How a seek was made; only a scrub pauses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeekGesture {
    /// A press on the timeline: seeks and keeps playing.
    #[allow(dead_code)]
    // The timeline's press-seek routes here with the timeline-marks change.
    Press,
    /// A drag along the timeline: pauses so the playhead stays put.
    Scrub,
    /// A time typed or pasted into the playhead clock: seeks, keeps playing,
    /// and brings the time into view on the strip.
    Typed,
}

/// Seek the playhead to `ts`. Inside the selected range (fixed or pinned) the
/// range is kept and the playhead moves within it. Anywhere else the playhead
/// leaves the live edge (see [`detach_playhead`]) and a range becomes a point;
/// a half-placed I/O range survives so O can follow a seek after I.
pub(crate) fn reduce_seek(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
    ts: f64,
    gesture: SeekGesture,
) -> TransportActions {
    if !ts.is_finite() {
        return TransportActions::default();
    }
    if gesture == SeekGesture::Scrub {
        slices.playback.playing = false;
    }
    if gesture == SeekGesture::Typed {
        reveal(slices.playback, ts);
    }
    seek_inner(env, slices, ts)
}

/// The seek shared by [`reduce_seek`] and [`reduce_step`].
fn seek_inner(env: &TransportEnv, slices: TransportSlices<'_>, ts: f64) -> TransportActions {
    let inside = slices
        .playback
        .playback_bounds()
        .is_some_and(|(start, end)| ts >= start && ts <= end);
    if inside {
        slices.playback.seek_within_range(ts);
        slices.playback.sync_macro_frame_index();
        return TransportActions::default();
    }
    let TransportSlices {
        live_mode,
        engine,
        playback,
    } = slices;
    let actions = detach_playhead(
        env,
        TransportSlices {
            live_mode,
            engine,
            playback: &mut *playback,
        },
    );
    let parked = playback.drag.filter(|d| !d.in_progress);
    playback.clear_selection();
    playback.drag = parked;
    playback.set_playback_position(ts);
    playback.sync_macro_frame_index();
    actions
}

/// Bring `ts` onto the strip when it is off screen, centered. A deliberate
/// jump away, so the view stops following now.
fn reveal(playback: &mut PlaybackState, ts: f64) {
    let span = playback.view_width_secs();
    let start = playback.timeline_view_start;
    if span > 0.0 && (ts < start || ts > start + span) {
        playback.timeline_view_start = ts - span / 2.0;
        playback.view_follows_now = false;
    }
}

/// Which way a step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepDir {
    Back,
    Forward,
}

/// How far a step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepUnit {
    /// The next frame of the selected product and tilt (← →, the step
    /// buttons).
    Frame,
    /// The next volume (Shift+← →).
    Volume,
}

/// The frames a step can land on, by time. `next_after` / `prev_before` are
/// strict (a frame at `ts` itself is not a neighbour).
pub(crate) trait FrameIndex {
    fn next_after(&self, ts: f64) -> Option<f64>;
    fn prev_before(&self, ts: f64) -> Option<f64>;
}

/// [`FrameIndex`] over the scan inventory: sweeps of one tilt (`tilt`), any
/// tilt (`None`, the Latest selection), or whole volumes (`volumes`, for a
/// Shift step or volumetric 3D, where a frame is a scan).
pub(crate) struct ScanFrames<'a> {
    pub scans: &'a RadarTimeline,
    pub tilt: Option<u8>,
    pub volumes: bool,
}

impl FrameIndex for ScanFrames<'_> {
    fn next_after(&self, ts: f64) -> Option<f64> {
        match (self.volumes, self.tilt) {
            (true, Some(n)) => self.scans.next_scan_matching_sweep_end_by_number(ts, n),
            (true, None) => self.scans.next_scan_any_sweep_end(ts),
            (false, Some(n)) => self.scans.next_matching_sweep_end_by_number(ts, n),
            (false, None) => self.scans.next_any_sweep_end(ts),
        }
    }

    fn prev_before(&self, ts: f64) -> Option<f64> {
        match (self.volumes, self.tilt) {
            (true, Some(n)) => self.scans.prev_scan_matching_sweep_end_by_number(ts, n),
            (true, None) => self.scans.prev_scan_any_sweep_end(ts),
            (false, Some(n)) => self.scans.prev_matching_sweep_end_by_number(ts, n),
            (false, None) => self.scans.prev_any_sweep_end(ts),
        }
    }
}

/// Where a step from `pos` lands, or `None` for a no-op.
///
/// Inside a range (`bounds`) the step wraps: past the last frame to the first,
/// before the first to the last. Without one it never wraps; where there is no
/// neighbouring frame it moves `nudge_secs` instead (a gap is never a dead
/// end), but never past `now`.
pub(crate) fn step_target(
    index: &dyn FrameIndex,
    pos: f64,
    dir: StepDir,
    bounds: Option<(f64, f64)>,
    nudge_secs: f64,
    now: f64,
) -> Option<f64> {
    match (dir, bounds) {
        (StepDir::Forward, Some((start, end))) => index
            .next_after(pos)
            .filter(|t| *t <= end && *t >= start)
            .or_else(|| index.next_after(start - 1.0).filter(|t| *t <= end))
            .or(Some(start)),
        (StepDir::Back, Some((start, end))) => index
            .prev_before(pos)
            .filter(|t| *t >= start && *t <= end)
            .or_else(|| index.prev_before(end + 1.0).filter(|t| *t >= start))
            .or(Some(end)),
        (StepDir::Forward, None) => {
            let target = index.next_after(pos).unwrap_or(pos + nudge_secs).min(now);
            (target > pos + 0.5).then_some(target)
        }
        (StepDir::Back, None) => Some(index.prev_before(pos).unwrap_or(pos - nudge_secs)),
    }
}

/// Step one frame (or volume) through `index` and pause there. A step is a
/// seek ([`reduce_seek`]'s range rule), so it keeps a range it stays inside
/// and leaves the live edge otherwise.
pub(crate) fn reduce_step(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
    index: &dyn FrameIndex,
    dir: StepDir,
) -> TransportActions {
    let playback = &*slices.playback;
    let nudge = playback.speed.timeline_seconds_per_real_second();
    let Some(target) = step_target(
        index,
        playback.playback_position(),
        dir,
        playback.playback_bounds(),
        nudge,
        env.now_secs,
    ) else {
        return TransportActions::default();
    };
    slices.playback.playing = false;
    seek_inner(env, slices, target)
}

/// Which edge of the range I / O sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoopEdge {
    /// I: the range starts at the playhead.
    In,
    /// O: the range ends at the playhead.
    Out,
}

/// Move one edge of the range to the playhead (I / O). With no range yet the
/// other edge starts at the playhead too, so the first press parks a half
/// range (`PlaybackState::drag`, not in progress, drawn as a bar with one
/// handle) without leaving the live edge, and the second completes it through
/// [`select_range`] (so the "ends at now" rule applies, and it starts paused).
/// Returns the actions and whether a range was selected.
pub(crate) fn reduce_loop_point(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
    edge: LoopEdge,
) -> (TransportActions, bool) {
    let playback = &mut *slices.playback;
    let pos = playback.playback_position();
    let (mut a, mut b) = playback
        .playback_bounds()
        .or(playback.drag.map(|d| (d.a.min(d.b), d.a.max(d.b))))
        .unwrap_or((pos, pos));
    match edge {
        LoopEdge::In => a = pos,
        LoopEdge::Out => b = pos,
    }
    if (b - a).abs() <= 1.0 {
        if playback.playback_bounds().is_none() {
            playback.drag = Some(crate::core::TimeSelection {
                a,
                b,
                in_progress: false,
            });
        }
        return (TransportActions::default(), false);
    }
    select_range(env, slices, a, b, false)
}

/// Choose how a range loops (Loop, Ping-pong, Once). It is a standing
/// preference, so it applies to the next range too.
pub(crate) fn set_loop_style(playback: &mut PlaybackState, style: LoopMode) {
    playback.time_model.loop_mode = style;
    playback.time_model.direction = crate::core::PlaybackDirection::Forward;
}

/// While pinned to now in Rays the speed is real time and is not a choice.
pub(crate) fn speed_locked(playback: &PlaybackState) -> bool {
    playback.is_pinned() && playback.playback_mode() == PlaybackMode::Micro
}

/// The speeds the current mode can express: frame rates in Frames, multiples
/// of real time in Rays.
pub(crate) fn mode_speeds(mode: PlaybackMode) -> &'static [PlaybackSpeed] {
    match mode {
        PlaybackMode::Macro => PlaybackSpeed::macro_speeds(),
        PlaybackMode::Micro => PlaybackSpeed::all(),
    }
}

/// Choose a speed. A no-op while [`speed_locked`] or when the mode cannot
/// express it.
pub(crate) fn reduce_set_speed(playback: &mut PlaybackState, speed: PlaybackSpeed) {
    if speed_locked(playback) || !mode_speeds(playback.playback_mode()).contains(&speed) {
        return;
    }
    playback.speed = speed;
}

/// One rung faster or slower within the mode's speeds ([ / ]). A speed the mode
/// cannot express lands on its slowest rung; the ends do not wrap.
pub(crate) fn reduce_step_speed(playback: &mut PlaybackState, faster: bool) {
    if speed_locked(playback) {
        return;
    }
    let speeds = mode_speeds(playback.playback_mode());
    let next = match speeds.iter().position(|s| *s == playback.speed) {
        Some(i) if faster => speeds.get(i + 1).copied(),
        Some(i) => i.checked_sub(1).map(|j| speeds[j]),
        None => speeds.first().copied(),
    };
    if let Some(speed) = next {
        playback.speed = speed;
    }
}

/// The Play button's face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlayButton {
    Play,
    Pause,
}

/// The speed control: a menu, or a fixed readout while pinned in Rays.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpeedControl {
    Locked {
        label: String,
    },
    Menu {
        selected: String,
        choices: Vec<(PlaybackSpeed, String)>,
    },
}

/// What the transport row draws. Every control is always enabled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TransportVm {
    pub play: PlayButton,
    pub play_hover: &'static str,
    pub mode: PlaybackMode,
    pub speed: SpeedControl,
}

/// Build the transport row from the playback state.
pub(crate) fn transport_vm(playback: &PlaybackState) -> TransportVm {
    let pinned = playback.is_pinned();
    let (play, play_hover) = if pinned {
        (PlayButton::Pause, "Pause (the stream keeps filling)")
    } else if playback.playing {
        (PlayButton::Pause, "Pause")
    } else {
        (PlayButton::Play, "Play")
    };
    let mode = playback.playback_mode();
    let label = |s: &PlaybackSpeed| match mode {
        PlaybackMode::Macro => s.macro_label(),
        PlaybackMode::Micro => s.label(),
    };
    let speed = if speed_locked(playback) {
        SpeedControl::Locked {
            label: label(&playback.speed).to_string(),
        }
    } else {
        SpeedControl::Menu {
            selected: label(&playback.speed).to_string(),
            choices: mode_speeds(mode)
                .iter()
                .map(|s| (*s, label(s).to_string()))
                .collect(),
        }
    };
    TransportVm {
        play,
        play_hover,
        mode,
        speed,
    }
}

/// Stop looping and stay where the playhead is. A live range detaches to a
/// fixed point at the playhead (see [`detach_playhead`]); a fixed range is
/// cleared to a fixed point. A point is left alone.
pub(crate) fn reduce_stop_looping(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
) -> TransportActions {
    match slices.playback.selection.range() {
        Some(r) if r.live => detach_playhead(env, slices),
        Some(_) => {
            slices.playback.clear_selection();
            TransportActions::default()
        }
        None => TransportActions::default(),
    }
}

/// Toggle play/pause according to the current mode (spec §7 pause-while-tethered):
///
/// - **ARCHIVE** (off live: a fixed point or range): ordinary play/pause through the selection
///   / from the current position. After a tethered freeze, this is how the user
///   resumes — ordinary archive playback from the pause point.
/// - **LIVE-NOW** (the live point): the live feed is conceptually *playing*, so
///   the button reads PAUSE. Pressing it FREEZES — the detach drops to a fixed
///   point at the current position with `playing = false`. Detachment is made
///   explicit by the LIVE button hollowing out + showing the growing lag; no
///   separate counter widget. The stream keeps running in the background
///   (unless the data-saver policy says otherwise).
/// - **LIVE RANGE** (a range attached to now, from a loop preset or a range
///   drawn up to now): ordinary play/pause looping through the window, which
///   keeps sliding with now either way. The range stays attached; clearing it
///   (not pausing) returns to the live point. The stream is never touched.
pub(crate) fn reduce_toggle_play_pause(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
) -> TransportActions {
    // LIVE-NOW → freeze: PAUSE while tethered drops to ARCHIVE at the current
    // position (the live edge), stopped. Routing through `detach_playhead`
    // stamps `detached_since` (so the LIVE button's lag readout and the idle
    // stop both work) and applies the data-saver policy in the one place that
    // owns it. Play (below, in `Free`) resumes from here as ordinary archive
    // playback.
    if slices.playback.is_pinned() {
        let TransportSlices {
            live_mode,
            engine,
            playback,
        } = slices;
        let actions = detach_playhead(
            env,
            TransportSlices {
                live_mode,
                engine,
                playback,
            },
        );
        playback.playing = false;
        return actions;
    }

    // ARCHIVE: ordinary play/pause.
    slices.playback.playing = !slices.playback.playing;
    TransportActions::default()
}

/// Where a user-driven stop leaves the playhead as the app drops to ARCHIVE.
///
/// The two surfaces that stop a stream have always differed here, and the
/// difference is only observable during a lookback replay (pinned-to-now, the
/// dominant case, has the playhead *on* the live edge already). Naming it keeps
/// both behaviours exact while collapsing them onto one reducer; unifying them
/// is a product decision, not a refactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveStopPlacement {
    /// Snap to the live edge, and report the exit reason in the status bar.
    /// The desktop timeline now-cap's "click to stop".
    LiveEdge,
    /// Keep the current position, and say nothing — the mobile LIVE button's
    /// "tap to freeze" (mobile chrome has no status line to report into).
    InPlace,
}

/// Stop streaming and drop to ARCHIVE, placing the playhead per `placement`.
pub(crate) fn reduce_stop_live(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
    placement: LiveStopPlacement,
) -> TransportActions {
    let TransportSlices {
        live_mode,
        engine,
        playback,
    } = slices;

    stop_live_machine(live_mode, engine, LiveExitReason::UserStopped);
    playback.playing = false;
    playback.exit_live(match placement {
        LiveStopPlacement::LiveEdge => FreezeAt::Now(env.now_secs),
        LiveStopPlacement::InPlace => FreezeAt::Keep,
    });

    TransportActions {
        // An explicit stop tears the worker channel down, matching
        // `stop_live_mode` (the error / site-change path for the same intent).
        // This is what separates STOP from DETACH: detaching keeps a *visible*
        // background stream (the LIVE chip hollows and counts lag), whereas a
        // stopped stream shows no live indicator at all — so leaving it running
        // would download and store chunks the user asked to stop and has no way
        // to see.
        stop_channel: true,
        status_message: match placement {
            LiveStopPlacement::LiveEdge => Some(
                live_mode
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default(),
            ),
            LiveStopPlacement::InPlace => None,
        },
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::core::projection::ProjectionEngine;
    use crate::core::{LivePhase, LoopBasis};

    /// A default env with the data-saver policy off.
    fn env_at(now: f64) -> TransportEnv {
        TransportEnv {
            now_secs: now,
            pause_stream_while_reviewing: false,
            frame_secs: 300.0,
        }
    }

    /// The three core slices a reducer needs, owned by the caller.
    struct Fixture {
        live_mode: LiveModeState,
        engine: ProjectionEngine,
        playback: PlaybackState,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                live_mode: LiveModeState::default(),
                engine: ProjectionEngine::new(),
                playback: PlaybackState::default(),
            }
        }

        fn slices(&mut self) -> TransportSlices<'_> {
            TransportSlices {
                live_mode: &mut self.live_mode,
                engine: &mut self.engine,
                playback: &mut self.playback,
            }
        }
    }

    // ---- select_range -------------------------------------------------

    #[wasm_bindgen_test]
    fn ends_at_now_is_within_one_frame_of_now() {
        assert!(ends_at_now(1000.0, 1000.0, 120.0));
        assert!(ends_at_now(1000.0 - 119.0, 1000.0, 120.0));
        assert!(!ends_at_now(1000.0 - 120.0, 1000.0, 120.0));
        // No spacing known: one volume.
        assert!(ends_at_now(1000.0 - LIVE_EDGE_SNAP_SECS + 1.0, 1000.0, 0.0));
        assert!(!ends_at_now(1000.0 - LIVE_EDGE_SNAP_SECS, 1000.0, f64::NAN));
    }

    #[wasm_bindgen_test]
    fn select_to_live_while_streaming_stays_attached() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.live_mode.detached_since = Some(400.0);
        f.playback.time_model.playback_position = 500.0;

        let (actions, selected) = select_range(&env_at(1000.0), f.slices(), 700.0, 1000.0, true);

        assert!(selected);
        assert_eq!(actions, TransportActions::default());
        assert!(f.playback.is_live_range());
        assert_eq!(f.playback.playback_bounds(), Some((700.0, 1000.0)));
        // Re-tethered: the detach stamp is cleared.
        assert_eq!(f.live_mode.detached_since, None);
    }

    #[wasm_bindgen_test]
    fn select_to_live_without_a_stream_is_a_fixed_range() {
        let mut f = Fixture::new();
        let (_, selected) = select_range(&env_at(1000.0), f.slices(), 700.0, 1000.0, true);
        assert!(selected);
        assert!(!f.playback.is_live_range());
        assert_eq!(f.playback.playback_bounds(), Some((700.0, 1000.0)));
    }

    #[wasm_bindgen_test]
    fn select_fixed_range_detaches_the_playhead() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);

        let (_, selected) = select_range(&env_at(1000.0), f.slices(), 100.0, 400.0, false);

        assert!(selected);
        assert!(!f.playback.selection.is_live());
        assert_eq!(f.playback.playback_bounds(), Some((100.0, 400.0)));
        assert_eq!(f.live_mode.detached_since, Some(1000.0));
    }

    // ---- reduce_stop_looping --------------------------------------------

    #[wasm_bindgen_test]
    fn stop_looping_a_live_range_stays_at_the_playhead() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        f.playback
            .enter_live_range(Some(900.0), LoopBasis::default());

        reduce_stop_looping(&env_at(1000.0), f.slices());

        assert_eq!(
            f.playback.selection,
            crate::core::Selection::Point { live: false }
        );
        assert_eq!(f.playback.playback_position(), 900.0);
        assert_eq!(f.live_mode.detached_since, Some(1000.0));
    }

    #[wasm_bindgen_test]
    fn stop_looping_a_fixed_range_keeps_the_position() {
        let mut f = Fixture::new();
        assert!(f.playback.select_range(100.0, 400.0, false));
        f.playback.time_model.playback_position = 250.0;

        reduce_stop_looping(&env_at(1000.0), f.slices());

        assert_eq!(
            f.playback.selection,
            crate::core::Selection::Point { live: false }
        );
        assert_eq!(f.playback.playback_position(), 250.0);
    }

    #[wasm_bindgen_test]
    fn stop_looping_leaves_the_live_point_alone() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        reduce_stop_looping(&env_at(1000.0), f.slices());
        assert!(f.playback.is_pinned());
    }

    // ---- detach_playhead ----------------------------------------------

    #[wasm_bindgen_test]
    fn detach_when_not_streaming_only_exits_live() {
        let mut f = Fixture::new();
        f.playback.enter_pinned_live(1000.0);

        let actions = detach_playhead(&env_at(500.0), f.slices());

        assert_eq!(actions, TransportActions::default());
        assert!(!f.playback.is_pinned());
        // Inactive stream untouched — no detach stamp.
        assert_eq!(f.live_mode.phase, LivePhase::Idle);
        assert_eq!(f.live_mode.detached_since, None);
    }

    #[wasm_bindgen_test]
    fn detach_while_streaming_latches_detached_since() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);

        let actions = detach_playhead(&env_at(321.0), f.slices());

        assert!(!actions.stop_channel);
        assert!(f.live_mode.is_active());
        assert_eq!(f.live_mode.detached_since, Some(321.0));
    }

    #[wasm_bindgen_test]
    fn detach_does_not_restamp_an_existing_detached_since() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.live_mode.detached_since = Some(100.0);

        let _ = detach_playhead(&env_at(999.0), f.slices());

        assert_eq!(f.live_mode.detached_since, Some(100.0));
    }

    #[wasm_bindgen_test]
    fn detach_with_data_saver_stops_the_stream() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);

        let actions = detach_playhead(
            &TransportEnv {
                now_secs: 777.0,
                pause_stream_while_reviewing: true,
                frame_secs: 300.0,
            },
            f.slices(),
        );

        assert!(actions.stop_channel);
        assert_eq!(f.live_mode.phase, LivePhase::Idle);
        assert!(!f.live_mode.is_active());
        assert_eq!(f.live_mode.detached_since, None);
        assert_eq!(
            f.live_mode.last_exit_reason,
            Some(LiveExitReason::UserStopped)
        );
    }

    // ---- reduce_toggle_play_pause: LIVE-LOOKBACK branch ----------------

    #[wasm_bindgen_test]
    fn live_range_pauses_and_plays_in_place() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        // A preset loop starts paused; Play and Pause keep the attached range.
        f.playback.enter_pinned_live(1000.0);
        f.playback.enter_live_range(None, LoopBasis::default());
        assert!(!f.playback.playing);

        reduce_toggle_play_pause(&env_at(4242.0), f.slices());
        assert!(f.playback.playing);
        let actions = reduce_toggle_play_pause(&env_at(4242.0), f.slices());

        assert_eq!(actions, TransportActions::default());
        assert!(f.playback.is_live_range());
        assert!(!f.playback.playing);
        assert_eq!(f.live_mode.detached_since, None);

        // A range drawn up to now starts paused; Play loops through it.
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        let (_, selected) = select_range(&env_at(1000.0), f.slices(), 700.0, 1000.0, true);
        assert!(selected);
        assert!(!f.playback.playing);

        reduce_toggle_play_pause(&env_at(1000.0), f.slices());

        assert!(f.playback.playing);
        assert!(f.playback.is_live_range());
        assert_eq!(f.playback.playback_bounds(), Some((700.0, 1000.0)));
    }

    // ---- reduce_toggle_play_pause: LIVE-NOW freeze branch --------------

    #[wasm_bindgen_test]
    fn pinned_freezes_to_archive_inactive_stream() {
        let mut f = Fixture::new(); // live machine Idle => inactive
        f.playback.enter_pinned_live(1000.0);
        assert!(f.playback.is_pinned());

        let actions = reduce_toggle_play_pause(&env_at(500.0), f.slices());

        // Pinned -> Free (archive), paused; position kept (exit_live Keep).
        assert_eq!(actions, TransportActions::default());
        assert!(!f.playback.is_pinned());
        assert!(!f.playback.is_live_range());
        assert!(!f.playback.playing);
        assert!((f.playback.playback_position() - 1000.0).abs() < 1e-9);
        // Inactive stream untouched.
        assert_eq!(f.live_mode.phase, LivePhase::Idle);
        assert_eq!(f.live_mode.detached_since, None);
    }

    #[wasm_bindgen_test]
    fn pinned_freeze_active_stream_latches_detached_since() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming; // active
        f.playback.enter_pinned_live(1000.0);

        let actions = reduce_toggle_play_pause(&env_at(321.0), f.slices());

        assert!(!f.playback.is_pinned());
        assert!(!f.playback.playing);
        // Non-data-saver: stream keeps running, detached_since latches now.
        assert!(!actions.stop_channel);
        assert!(f.live_mode.is_active());
        assert_eq!(f.live_mode.detached_since, Some(321.0));
    }

    #[wasm_bindgen_test]
    fn pinned_freeze_data_saver_stops_stream() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming; // active
        f.playback.enter_pinned_live(1000.0);

        let actions = reduce_toggle_play_pause(
            &TransportEnv {
                now_secs: 777.0,
                pause_stream_while_reviewing: true,
                frame_secs: 300.0,
            },
            f.slices(),
        );

        assert!(!f.playback.is_pinned());
        assert!(!f.playback.playing);
        // Data-saver path stops the stream immediately and never latches.
        assert!(actions.stop_channel);
        assert_eq!(f.live_mode.phase, LivePhase::Idle);
        assert!(!f.live_mode.is_active());
        assert_eq!(f.live_mode.detached_since, None);
        assert_eq!(
            f.live_mode.last_exit_reason,
            Some(LiveExitReason::UserStopped)
        );
    }

    // ---- reduce_toggle_play_pause: ARCHIVE (Free) ----------------------

    #[wasm_bindgen_test]
    fn archive_playing_pauses() {
        let mut f = Fixture::new(); // mode Free
        f.playback.playing = true;
        assert!(!f.playback.is_pinned());
        assert!(!f.playback.is_live_range());

        let actions = reduce_toggle_play_pause(&env_at(100.0), f.slices());

        assert_eq!(actions, TransportActions::default());
        assert!(!f.playback.playing);
    }

    #[wasm_bindgen_test]
    fn archive_paused_resumes_when_playback_allowed() {
        let mut f = Fixture::new(); // Free
        f.playback.playing = false;

        reduce_toggle_play_pause(&env_at(100.0), f.slices());

        assert!(f.playback.playing);
    }

    // ---- reduce_stop_live ----------------------------------------------

    #[wasm_bindgen_test]
    fn stop_live_freezes_on_now_and_reports_the_exit_reason() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        f.playback.playing = true;

        let actions = reduce_stop_live(&env_at(2500.0), f.slices(), LiveStopPlacement::LiveEdge);

        assert_eq!(f.live_mode.phase, LivePhase::Idle);
        assert_eq!(
            f.live_mode.last_exit_reason,
            Some(LiveExitReason::UserStopped)
        );
        assert!(!f.playback.playing);
        assert!(!f.playback.is_pinned());
        // FreezeAt::Now snaps the cursor to the live edge.
        assert!((f.playback.playback_position() - 2500.0).abs() < 1e-9);
        // An explicit stop tears down the stream (unlike a detach).
        assert!(actions.stop_channel);
        assert_eq!(
            actions.status_message.as_deref(),
            Some(LiveExitReason::UserStopped.message())
        );
    }

    #[wasm_bindgen_test]
    fn stop_live_from_lookback_also_drops_to_archive() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        f.playback.enter_live_range(None, LoopBasis::default());

        reduce_stop_live(&env_at(1234.0), f.slices(), LiveStopPlacement::LiveEdge);

        assert!(!f.playback.is_live_range());
        assert!(!f.playback.is_pinned());
        assert!((f.playback.playback_position() - 1234.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn stop_live_in_place_keeps_the_position_and_stays_silent() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        f.playback.pin_tick(1400.0);

        let actions = reduce_stop_live(&env_at(2500.0), f.slices(), LiveStopPlacement::InPlace);

        assert_eq!(f.live_mode.phase, LivePhase::Idle);
        assert!(!f.playback.playing);
        assert!(!f.playback.is_pinned());
        // FreezeAt::Keep: the playhead stays where the pin left it.
        assert!((f.playback.playback_position() - 1400.0).abs() < 1e-9);
        assert_eq!(actions.status_message, None);
        // Placement differs between surfaces; tearing the stream down does not.
        assert!(actions.stop_channel);
    }

    // ---- the one "ends at now" rule ------------------------------------

    #[wasm_bindgen_test]
    fn a_range_ending_within_a_frame_of_now_pins_while_streaming() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        // Not asked to pin, but its end is 200 s (under one 300 s frame) from now.
        let (_, selected) = select_range(&env_at(1000.0), f.slices(), 500.0, 800.0, false);
        assert!(selected);
        assert!(f.playback.is_live_range());

        // A frame or more short of now stays fixed.
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        let (_, selected) = select_range(&env_at(1000.0), f.slices(), 400.0, 700.0, false);
        assert!(selected);
        assert!(!f.playback.is_live_range());
    }

    #[wasm_bindgen_test]
    fn every_new_range_starts_paused() {
        let mut f = Fixture::new();
        f.playback.playing = true;
        let (_, selected) = select_range(&env_at(1000.0), f.slices(), 100.0, 400.0, false);
        assert!(selected);
        assert!(!f.playback.playing);
    }

    // ---- reduce_seek ----------------------------------------------------

    #[wasm_bindgen_test]
    fn a_press_seeks_without_pausing_and_a_scrub_pauses() {
        let mut f = Fixture::new();
        f.playback.playing = true;
        reduce_seek(&env_at(1000.0), f.slices(), 500.0, SeekGesture::Press);
        assert_eq!(f.playback.playback_position(), 500.0);
        assert!(f.playback.playing);

        reduce_seek(&env_at(1000.0), f.slices(), 450.0, SeekGesture::Scrub);
        assert_eq!(f.playback.playback_position(), 450.0);
        assert!(!f.playback.playing);
    }

    #[wasm_bindgen_test]
    fn a_seek_inside_a_range_keeps_it_and_outside_makes_a_point() {
        let mut f = Fixture::new();
        assert!(f.playback.select_range(100.0, 400.0, false));
        reduce_seek(&env_at(1000.0), f.slices(), 250.0, SeekGesture::Press);
        assert_eq!(f.playback.playback_bounds(), Some((100.0, 400.0)));
        assert_eq!(f.playback.playback_position(), 250.0);

        reduce_seek(&env_at(1000.0), f.slices(), 600.0, SeekGesture::Press);
        assert_eq!(
            f.playback.selection,
            crate::core::Selection::Point { live: false }
        );
        assert_eq!(f.playback.playback_position(), 600.0);
    }

    #[wasm_bindgen_test]
    fn a_seek_inside_a_pinned_range_keeps_it_pinned() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        f.playback
            .enter_live_range(Some(700.0), LoopBasis::default());
        f.playback.slide_live_range(700.0, 1000.0);

        reduce_seek(&env_at(1000.0), f.slices(), 800.0, SeekGesture::Press);

        assert!(f.playback.is_live_range());
        assert_eq!(f.playback.playback_position(), 800.0);
        assert_eq!(f.live_mode.detached_since, None);
    }

    #[wasm_bindgen_test]
    fn a_seek_from_the_live_point_detaches() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        reduce_seek(&env_at(1000.0), f.slices(), 600.0, SeekGesture::Press);
        assert!(!f.playback.is_pinned());
        assert_eq!(f.playback.playback_position(), 600.0);
        assert_eq!(f.live_mode.detached_since, Some(1000.0));
        assert!(f.live_mode.is_active(), "a seek never stops the stream");
    }

    #[wasm_bindgen_test]
    fn a_typed_time_comes_into_view() {
        let mut f = Fixture::new();
        f.playback.timeline_width_px = 1000.0;
        f.playback.timeline_zoom = 1.0; // 1000 s across
        f.playback.timeline_view_start = 10_000.0;
        f.playback.view_follows_now = true;
        reduce_seek(&env_at(20_000.0), f.slices(), 5_000.0, SeekGesture::Typed);
        assert_eq!(f.playback.timeline_view_start, 4_500.0);
        assert!(!f.playback.view_follows_now);

        // Already in view: the strip stays put.
        reduce_seek(&env_at(20_000.0), f.slices(), 4_800.0, SeekGesture::Typed);
        assert_eq!(f.playback.timeline_view_start, 4_500.0);
    }

    #[wasm_bindgen_test]
    fn a_seek_keeps_a_half_placed_range() {
        let mut f = Fixture::new();
        f.playback.time_model.playback_position = 100.0;
        let (_, selected) = reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::In);
        assert!(!selected);
        reduce_seek(&env_at(1000.0), f.slices(), 400.0, SeekGesture::Press);
        let (_, selected) = reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::Out);
        assert!(selected);
        assert_eq!(f.playback.playback_bounds(), Some((100.0, 400.0)));
    }

    // ---- step -------------------------------------------------------------

    /// Frames at fixed times.
    struct Frames(Vec<f64>);

    impl FrameIndex for Frames {
        fn next_after(&self, ts: f64) -> Option<f64> {
            self.0.iter().copied().find(|t| *t > ts + 0.5)
        }
        fn prev_before(&self, ts: f64) -> Option<f64> {
            self.0.iter().rev().copied().find(|t| *t < ts - 0.5)
        }
    }

    fn frames() -> Frames {
        Frames(vec![100.0, 200.0, 300.0, 400.0, 500.0])
    }

    #[wasm_bindgen_test]
    fn steps_land_on_neighbouring_frames() {
        let i = frames();
        assert_eq!(
            step_target(&i, 200.0, StepDir::Forward, None, 30.0, 1000.0),
            Some(300.0)
        );
        assert_eq!(
            step_target(&i, 250.0, StepDir::Back, None, 30.0, 1000.0),
            Some(200.0)
        );
    }

    #[wasm_bindgen_test]
    fn steps_wrap_only_inside_a_range() {
        let i = frames();
        let range = Some((150.0, 420.0));
        assert_eq!(
            step_target(&i, 400.0, StepDir::Forward, range, 30.0, 1000.0),
            Some(200.0)
        );
        assert_eq!(
            step_target(&i, 200.0, StepDir::Back, range, 30.0, 1000.0),
            Some(400.0)
        );
        // Without a range the ends do not wrap: before the first frame it
        // nudges back; past the last it nudges forward (never past now).
        assert_eq!(
            step_target(&i, 100.0, StepDir::Back, None, 30.0, 1000.0),
            Some(70.0)
        );
        assert_eq!(
            step_target(&i, 500.0, StepDir::Forward, None, 30.0, 1000.0),
            Some(530.0)
        );
        assert_eq!(
            step_target(&i, 500.0, StepDir::Forward, None, 30.0, 510.0),
            Some(510.0)
        );
    }

    #[wasm_bindgen_test]
    fn stepping_forward_at_now_is_a_no_op() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        let actions = reduce_step(&env_at(1000.0), f.slices(), &frames(), StepDir::Forward);
        assert_eq!(actions, TransportActions::default());
        assert!(f.playback.is_pinned(), "nothing newer: stay on now");
    }

    #[wasm_bindgen_test]
    fn stepping_back_from_now_detaches_and_pauses() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        reduce_step(&env_at(1000.0), f.slices(), &frames(), StepDir::Back);
        assert!(!f.playback.is_pinned());
        assert_eq!(f.playback.playback_position(), 500.0);
        assert!(!f.playback.playing);
        assert_eq!(f.live_mode.detached_since, Some(1000.0));
    }

    #[wasm_bindgen_test]
    fn stepping_inside_a_range_keeps_it_and_pauses() {
        let mut f = Fixture::new();
        assert!(f.playback.select_range(150.0, 420.0, false));
        f.playback.time_model.playback_position = 400.0;
        f.playback.playing = true;
        reduce_step(&env_at(1000.0), f.slices(), &frames(), StepDir::Forward);
        assert_eq!(f.playback.playback_bounds(), Some((150.0, 420.0)));
        assert_eq!(f.playback.playback_position(), 200.0);
        assert!(!f.playback.playing);
    }

    // ---- I / O --------------------------------------------------------------

    #[wasm_bindgen_test]
    fn in_then_out_draws_a_range_that_starts_paused() {
        let mut f = Fixture::new();
        f.playback.playing = true;
        f.playback.time_model.playback_position = 200.0;
        let (_, selected) = reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::In);
        assert!(!selected);
        let parked = f.playback.drag.expect("half range parked");
        assert!(!parked.in_progress);

        f.playback.time_model.playback_position = 500.0;
        let (_, selected) = reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::Out);
        assert!(selected);
        assert_eq!(f.playback.playback_bounds(), Some((200.0, 500.0)));
        assert!(!f.playback.playing);
        assert_eq!(f.playback.drag, None);
    }

    #[wasm_bindgen_test]
    fn in_while_live_parks_without_leaving_now() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.enter_pinned_live(1000.0);
        reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::In);
        assert!(f.playback.is_pinned());
        assert_eq!(f.live_mode.detached_since, None);
    }

    #[wasm_bindgen_test]
    fn out_at_now_pins_the_range() {
        let mut f = Fixture::new();
        f.live_mode.phase = LivePhase::Streaming;
        f.playback.time_model.playback_position = 400.0;
        reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::In);
        // O a minute short of now: within a frame, so the range pins.
        reduce_seek(&env_at(1000.0), f.slices(), 940.0, SeekGesture::Press);
        let (_, selected) = reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::Out);
        assert!(selected);
        assert!(f.playback.is_live_range());
    }

    #[wasm_bindgen_test]
    fn in_and_out_move_an_existing_range_edge() {
        let mut f = Fixture::new();
        assert!(f.playback.select_range(100.0, 400.0, false));
        f.playback.time_model.playback_position = 250.0;
        reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::In);
        assert_eq!(f.playback.playback_bounds(), Some((250.0, 400.0)));
        f.playback.time_model.playback_position = 300.0;
        reduce_loop_point(&env_at(1000.0), f.slices(), LoopEdge::Out);
        assert_eq!(f.playback.playback_bounds(), Some((250.0, 300.0)));
    }

    // ---- loop style, speed, view-model ------------------------------------

    #[wasm_bindgen_test]
    fn loop_style_is_kept_and_restarts_the_direction() {
        let mut ps = PlaybackState::default();
        ps.time_model.direction = crate::core::PlaybackDirection::Backward;
        set_loop_style(&mut ps, LoopMode::Once);
        assert_eq!(ps.time_model.loop_mode, LoopMode::Once);
        assert!(ps.time_model.direction == crate::core::PlaybackDirection::Forward);
        assert!(ps.select_range(100.0, 400.0, false));
        assert_eq!(ps.time_model.loop_mode, LoopMode::Once);
    }

    #[wasm_bindgen_test]
    fn speed_is_real_time_and_not_a_menu_while_pinned_in_rays() {
        let mut ps = PlaybackState::default();
        ps.set_play_mode(PlaybackMode::Micro);
        ps.enter_pinned_live(1000.0);
        assert!(speed_locked(&ps));
        assert!(matches!(
            transport_vm(&ps).speed,
            SpeedControl::Locked { .. }
        ));
        reduce_set_speed(&mut ps, PlaybackSpeed::FifteenToOne);
        reduce_step_speed(&mut ps, true);
        assert_eq!(ps.speed, PlaybackSpeed::Realtime);

        // Frames keeps its menu while pinned.
        ps.set_play_mode(PlaybackMode::Macro);
        assert!(!speed_locked(&ps));
        let SpeedControl::Menu { choices, .. } = transport_vm(&ps).speed else {
            panic!("Frames has a speed menu");
        };
        assert!(choices
            .iter()
            .all(|(s, _)| s.macro_frames_per_second().is_some()));
    }

    #[wasm_bindgen_test]
    fn speed_steps_stay_within_the_mode() {
        let mut ps = PlaybackState::default();
        ps.set_play_mode(PlaybackMode::Macro);
        let speeds = mode_speeds(PlaybackMode::Macro);
        ps.speed = speeds[0];
        reduce_step_speed(&mut ps, false);
        assert_eq!(ps.speed, speeds[0], "the slow end does not wrap");
        reduce_step_speed(&mut ps, true);
        assert_eq!(ps.speed, speeds[1]);
        // A Rays-only multiple cannot be chosen in Frames.
        reduce_set_speed(&mut ps, PlaybackSpeed::FifteenToOne);
        assert_eq!(ps.speed, speeds[1]);
    }

    #[wasm_bindgen_test]
    fn the_play_button_reads_pause_while_pinned_or_playing() {
        let mut ps = PlaybackState::default();
        assert_eq!(transport_vm(&ps).play, PlayButton::Play);
        ps.playing = true;
        assert_eq!(transport_vm(&ps).play, PlayButton::Pause);
        ps.playing = false;
        ps.enter_pinned_live(1000.0);
        assert_eq!(transport_vm(&ps).play, PlayButton::Pause);
    }
}
