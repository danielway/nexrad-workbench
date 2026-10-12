//! Pure transport reducers — play/pause, playhead detach, and stop-live.
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
use crate::core::{FreezeAt, LiveExitReason, LiveModeState, PlaybackState};

/// Read-only frame context for one transport gesture.
pub(crate) struct TransportEnv {
    /// `state.frame_now.secs()` — this frame's wall clock.
    pub now_secs: f64,
    /// The data-saver policy: stop the background stream the moment the user
    /// starts reviewing.
    pub pause_stream_while_reviewing: bool,
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

/// How close (seconds) a range's end must be to now to attach to the live
/// edge when it is drawn — one scan, so "up to the latest sweep" counts.
pub(crate) const LIVE_EDGE_SNAP_SECS: f64 = 300.0;

/// Whether a range ending at `end` reads as "up to now".
pub(crate) fn ends_at_live_edge(end: f64, now: f64) -> bool {
    (now - end).abs() < LIVE_EDGE_SNAP_SECS
}

/// Select a range from a timeline gesture. With `to_live` and a running
/// stream it becomes a live range: the playhead stays attached and the live
/// tick slides the window forward with now. Otherwise the playhead detaches
/// (see [`detach_playhead`]) and the range is fixed. Returns the detach's
/// actions and whether a range was selected (it must span over a second).
pub(crate) fn select_range(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
    a: f64,
    b: f64,
    to_live: bool,
) -> (TransportActions, bool) {
    if to_live && slices.live_mode.is_active() {
        slices.live_mode.detached_since = None;
        let selected = slices.playback.select_range(a, b, true);
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
    (actions, selected)
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
///   drawn up to now): Pause snaps back to the live point.
///   The stream keeps running throughout — the live machine is never touched
///   on that branch.
pub(crate) fn reduce_toggle_play_pause(
    env: &TransportEnv,
    slices: TransportSlices<'_>,
) -> TransportActions {
    // LIVE RANGE → LIVE-NOW: stop the replay and re-pin to now. The stream
    // stays active (we never stop the live machine), so streaming continues.
    if slices.playback.is_live_range() {
        slices.playback.playing = false;
        slices.playback.exit_live_range_to_now(env.now_secs);
        return TransportActions::default();
    }

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
    if slices.playback.playing {
        slices.playback.playing = false;
    } else if slices.playback.is_playback_allowed() {
        slices.playback.playing = true;
    }
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
    use crate::core::{LivePhase, LoopBasis, TimelineTier};

    /// A default env with the data-saver policy off.
    fn env_at(now: f64) -> TransportEnv {
        TransportEnv {
            now_secs: now,
            pause_stream_while_reviewing: false,
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
    fn ends_at_live_edge_is_within_one_scan_of_now() {
        assert!(ends_at_live_edge(1000.0, 1000.0));
        assert!(ends_at_live_edge(
            1000.0 - LIVE_EDGE_SNAP_SECS + 1.0,
            1000.0
        ));
        assert!(!ends_at_live_edge(1000.0 - LIVE_EDGE_SNAP_SECS, 1000.0));
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
    fn lookback_repins_to_now_and_pauses() {
        let mut f = Fixture::new();
        // Reach a live range via the legal transition (live point -> range).
        f.playback.enter_pinned_live(1000.0);
        f.playback.enter_live_range(None, LoopBasis::default());
        assert!(f.playback.is_live_range());

        let actions = reduce_toggle_play_pause(&env_at(4242.0), f.slices());

        // exit_lookback_to_now: back to pinned-now, snapped to now, paused.
        assert_eq!(actions, TransportActions::default());
        assert!(!f.playback.is_live_range());
        assert!(f.playback.is_pinned());
        assert!(!f.playback.playing);
        assert!((f.playback.playback_position() - 4242.0).abs() < 1e-9);
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
        let mut f = Fixture::new(); // Free, default tier permits playback
        f.playback.playing = false;
        assert!(f.playback.is_playback_allowed());

        reduce_toggle_play_pause(&env_at(100.0), f.slices());

        assert!(f.playback.playing);
    }

    #[wasm_bindgen_test]
    fn archive_paused_stays_paused_when_archive_tier() {
        let mut f = Fixture::new(); // Free
        f.playback.playing = false;
        // Archive tier is a navigator only: playback is not allowed.
        f.playback.timeline_tier = TimelineTier::Archive;
        assert!(!f.playback.is_playback_allowed());

        reduce_toggle_play_pause(&env_at(100.0), f.slices());

        // No transition into pinned/lookback, and resume is blocked.
        assert!(!f.playback.is_pinned());
        assert!(!f.playback.is_live_range());
        assert!(!f.playback.playing);
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
}
