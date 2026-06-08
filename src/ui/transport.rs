//! Shared play/pause transport logic.
//!
//! Used by both the desktop play button (`playback_controls.rs`) and the
//! spacebar (`bottom_panel.rs`) so the ARCHIVE / LIVE-NOW / LIVE-LOOKBACK
//! branching lives in exactly one place. Play/pause is fully decoupled from
//! the stream: going live and stopping the stream belong to the now-line cap /
//! Ctrl+L, never to this button.

use crate::state::AppState;
use crate::subsystem::{Live, Playback, Timeline};

/// Toggle play/pause according to the current mode:
///
/// - **ARCHIVE** (not live): ordinary play/pause through the selection / from
///   the current position.
/// - **LIVE-NOW** (live, locked to now): Play starts a lookback replay that
///   frame-steps the last [`crate::LOOKBACK_FRAMES`] matching sweeps, looping.
/// - **LIVE-LOOKBACK** (replaying): Pause snaps back to "now" (re-locks). The
///   stream keeps running throughout — `mode_state` is never touched here.
pub(crate) fn toggle_play_pause(
    state: &mut AppState,
    timeline: &Timeline,
    live: &mut Live,
    playback: &mut Playback,
) {
    // LIVE-LOOKBACK → LIVE-NOW: stop the replay and re-pin to now. The stream
    // stays active (we never call mode_state.stop), so streaming continues.
    if playback.state.lookback_active {
        playback.state.playing = false;
        playback.state.clear_lookback();
        playback.state.time_model.enable_realtime_lock(); // snaps pos=now, clears bounds
        return;
    }

    // LIVE-NOW → LIVE-LOOKBACK: frame-step the recent matching sweeps, looping.
    // `tick_live` owns the frame window and the backfill pump fetches any
    // missing recent volumes — so we enter unconditionally even when few/no
    // frames are cached yet (the loop fills in as they land). Seed the playhead
    // at the oldest cached frame so the first pass runs oldest→newest.
    if live.mode_state.is_active() {
        playback.state.time_model.disable_realtime_lock();
        let now = crate::state::TimeModel::wall_clock_time();
        match timeline.scans.lookback_window(
            &state.viz_state.elevation_selection,
            now,
            crate::LOOKBACK_FRAMES,
        ) {
            Some((oldest, _)) => playback.state.set_playback_position(oldest),
            None => state.status_message = "Acquiring recent frames…".to_string(),
        }
        playback.state.start_lookback();
        return;
    }

    // ARCHIVE: ordinary play/pause.
    if playback.state.playing {
        playback.state.playing = false;
    } else if playback.state.is_playback_allowed() {
        playback.state.playing = true;
    }
}
