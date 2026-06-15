//! Shared play/pause transport logic.
//!
//! Used by both the desktop play button (`playback_controls.rs`) and the
//! spacebar (`bottom_panel.rs`) so the ARCHIVE / LIVE-NOW / LIVE-LOOKBACK
//! branching lives in exactly one place. Play/pause is fully decoupled from
//! the stream: going live belongs to the LIVE button / now-line cap / the `L`
//! key, stopping the stream belongs to the now-line cap, never to this button.

use crate::state::AppState;
use crate::subsystem::{Live, Playback, Timeline};

/// Toggle play/pause according to the current mode (spec §7 pause-while-tethered):
///
/// - **ARCHIVE** (not live, `Free`): ordinary play/pause through the selection
///   / from the current position. After a tethered freeze, this is how the user
///   resumes — ordinary archive playback from the pause point.
/// - **LIVE-NOW** (`PinnedToNow`): the live feed is conceptually *playing*, so
///   the button reads PAUSE. Pressing it FREEZES — `exit_live(Keep)` drops to
///   `Free` at the current position with `playing = false`. Detachment is made
///   explicit by the LIVE button hollowing out + showing the growing lag; no
///   separate counter widget. The stream keeps running in the background.
/// - **LIVE-LOOKBACK** (`LookbackLoop`): only reachable once loop presets land
///   (nothing enters it this phase). Kept working: Pause snaps back to "now".
///   The stream keeps running throughout — `mode_state` is never touched here.
///
/// `timeline` is unused now that Play-while-pinned no longer enters the lookback
/// replay; kept in the signature so the call sites (which thread it for other
/// transport surfaces) don't churn and the lookback re-entry phase can use it.
pub(crate) fn toggle_play_pause(
    state: &mut AppState,
    _timeline: &Timeline,
    live: &mut Live,
    playback: &mut Playback,
) {
    // LIVE-LOOKBACK → LIVE-NOW: stop the replay and re-pin to now. The stream
    // stays active (we never call mode_state.stop), so streaming continues.
    // Unreachable until loop presets re-enter lookback, but kept correct.
    if playback.state.time_model.is_lookback() {
        playback.state.playing = false;
        playback.state.exit_lookback_to_now(state.frame_now.secs());
        return;
    }

    // LIVE-NOW → freeze: PAUSE while tethered drops to ARCHIVE at the current
    // position (the live edge), stopped. Routing through `detach_playhead`
    // stamps `detached_since` (so the LIVE button's lag readout and the idle
    // stop both work) and applies the data-saver policy in the one place that
    // owns it. Play (below, in `Free`) resumes from here as ordinary archive
    // playback. The detachment is surfaced by the LIVE button hollowing out.
    if playback.state.time_model.is_pinned() {
        live.detach_playhead(
            &mut playback.state,
            state.frame_now.secs(),
            state.pause_stream_while_reviewing,
        );
        playback.state.playing = false;
        return;
    }

    // ARCHIVE: ordinary play/pause.
    if playback.state.playing {
        playback.state.playing = false;
    } else if playback.state.is_playback_allowed() {
        playback.state.playing = true;
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::nexrad::RealtimeChannel;
    use crate::state::{FrameNow, LiveExitReason, LivePhase, LoopBasis, TimelineTier};

    fn live() -> Live {
        Live::new(RealtimeChannel::new())
    }

    /// An AppState with a known frame clock and the default (data-saver-off)
    /// reviewing policy. Built from `AppState::default()` (no browser frame).
    fn state_at(now: f64) -> AppState {
        let mut s = AppState::default();
        s.frame_now = FrameNow(now);
        s.pause_stream_while_reviewing = false;
        s
    }

    // ---- LIVE-LOOKBACK branch -----------------------------------------

    #[wasm_bindgen_test]
    fn lookback_repins_to_now_and_pauses() {
        let mut state = state_at(4242.0);
        let timeline = Timeline::default();
        let mut live = live();
        let mut playback = Playback::default();

        // Reach LookbackLoop via the legal transition (pinned -> lookback).
        playback.state.enter_pinned_live(1000.0);
        playback.state.enter_lookback(None, LoopBasis::default());
        assert!(playback.state.time_model.is_lookback());

        toggle_play_pause(&mut state, &timeline, &mut live, &mut playback);

        // exit_lookback_to_now: back to pinned-now, snapped to frame_now, paused.
        assert!(!playback.state.time_model.is_lookback());
        assert!(playback.state.time_model.is_pinned());
        assert!(!playback.state.playing);
        assert!((playback.state.playback_position() - 4242.0).abs() < 1e-9);
        // Stream session untouched by lookback->pinned (mode_state never stopped).
    }

    // ---- LIVE-NOW (pinned) freeze branch ------------------------------

    #[wasm_bindgen_test]
    fn pinned_freezes_to_archive_inactive_stream() {
        let mut state = state_at(500.0);
        let timeline = Timeline::default();
        let mut live = live(); // mode_state Idle => inactive
        let mut playback = Playback::default();

        playback.state.enter_pinned_live(1000.0);
        assert!(playback.state.time_model.is_pinned());

        toggle_play_pause(&mut state, &timeline, &mut live, &mut playback);

        // Pinned -> Free (archive), paused; position kept (exit_live Keep).
        assert!(!playback.state.time_model.is_pinned());
        assert!(!playback.state.time_model.is_lookback());
        assert!(!playback.state.playing);
        assert!((playback.state.playback_position() - 1000.0).abs() < 1e-9);
        // Inactive stream untouched.
        assert_eq!(live.mode_state.phase, LivePhase::Idle);
        assert_eq!(live.mode_state.detached_since, None);
        assert!(!live.channel.is_active());
    }

    #[wasm_bindgen_test]
    fn pinned_freeze_active_stream_latches_detached_since() {
        let mut state = state_at(321.0); // data-saver off via state_at
        let timeline = Timeline::default();
        let mut live = live();
        live.mode_state.phase = LivePhase::Streaming; // active
        let mut playback = Playback::default();
        playback.state.enter_pinned_live(1000.0);

        toggle_play_pause(&mut state, &timeline, &mut live, &mut playback);

        assert!(!playback.state.time_model.is_pinned());
        assert!(!playback.state.playing);
        // Non-data-saver: stream keeps running, detached_since latches frame_now.
        assert!(live.mode_state.is_active());
        assert_eq!(live.mode_state.detached_since, Some(321.0));
    }

    #[wasm_bindgen_test]
    fn pinned_freeze_data_saver_stops_stream() {
        let mut state = state_at(777.0);
        state.pause_stream_while_reviewing = true; // data-saver ON
        let timeline = Timeline::default();
        let mut live = live();
        live.mode_state.phase = LivePhase::Streaming; // active
        let mut playback = Playback::default();
        playback.state.enter_pinned_live(1000.0);

        toggle_play_pause(&mut state, &timeline, &mut live, &mut playback);

        assert!(!playback.state.time_model.is_pinned());
        assert!(!playback.state.playing);
        // Data-saver path stops the stream immediately and never latches.
        assert_eq!(live.mode_state.phase, LivePhase::Idle);
        assert!(!live.mode_state.is_active());
        assert_eq!(live.mode_state.detached_since, None);
        assert!(!live.channel.is_active());
        assert_eq!(
            live.mode_state.last_exit_reason,
            Some(LiveExitReason::UserStopped)
        );
    }

    // ---- ARCHIVE (Free) ordinary play/pause ---------------------------

    #[wasm_bindgen_test]
    fn archive_playing_pauses() {
        let mut state = state_at(100.0);
        let timeline = Timeline::default();
        let mut live = live();
        let mut playback = Playback::default(); // mode Free
        playback.state.playing = true;
        assert!(!playback.state.time_model.is_pinned());
        assert!(!playback.state.time_model.is_lookback());

        toggle_play_pause(&mut state, &timeline, &mut live, &mut playback);

        assert!(!playback.state.playing);
    }

    #[wasm_bindgen_test]
    fn archive_paused_resumes_when_playback_allowed() {
        let mut state = state_at(100.0);
        let timeline = Timeline::default();
        let mut live = live();
        let mut playback = Playback::default(); // Free, default tier permits playback
        playback.state.playing = false;
        assert!(playback.state.is_playback_allowed());

        toggle_play_pause(&mut state, &timeline, &mut live, &mut playback);

        assert!(playback.state.playing);
    }

    #[wasm_bindgen_test]
    fn archive_paused_stays_paused_when_archive_tier() {
        let mut state = state_at(100.0);
        let timeline = Timeline::default();
        let mut live = live();
        let mut playback = Playback::default(); // Free
        playback.state.playing = false;
        // Archive tier is a navigator only: playback is not allowed.
        playback.state.timeline_tier = TimelineTier::Archive;
        assert!(!playback.state.is_playback_allowed());

        toggle_play_pause(&mut state, &timeline, &mut live, &mut playback);

        // No transition into pinned/lookback, and resume is blocked.
        assert!(!playback.state.time_model.is_pinned());
        assert!(!playback.state.time_model.is_lookback());
        assert!(!playback.state.playing);
    }
}
