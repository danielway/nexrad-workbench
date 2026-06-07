//! Shared play/pause transport logic.
//!
//! Used by both the desktop play button (`playback_controls.rs`) and the
//! spacebar (`bottom_panel.rs`) so the ARCHIVE / LIVE-NOW / LIVE-LOOKBACK
//! branching lives in exactly one place. Play/pause is fully decoupled from
//! the stream: going live and stopping the stream belong to the now-line cap /
//! Ctrl+L, never to this button.

use crate::state::AppState;
use crate::subsystem::{Live, Playback, Timeline};

/// How many recent frames the live "lookback" replay covers.
const LOOKBACK_FRAMES: usize = 5;

/// Toggle play/pause according to the current mode:
///
/// - **ARCHIVE** (not live): ordinary play/pause through the selection / from
///   the current position.
/// - **LIVE-NOW** (live, locked to now): Play starts a lookback replay of the
///   last [`LOOKBACK_FRAMES`] frames, looping.
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

    // LIVE-NOW → LIVE-LOOKBACK: replay the last N frames, looping.
    if live.mode_state.is_active() {
        let now = crate::state::TimeModel::wall_clock_time();
        match timeline.scans.lookback_window(
            &state.viz_state.elevation_selection,
            now,
            LOOKBACK_FRAMES,
        ) {
            // Reject a zero-width window — looping divides by the span width.
            Some((start, end)) if end - start > 1.0 => {
                playback.state.time_model.disable_realtime_lock();
                playback.state.start_lookback(start, end);
            }
            _ => {
                // Not enough recent frames to replay yet (e.g. still acquiring
                // the lock). Stay pinned to now.
                state.status_message = "No recent frames to replay yet".to_string();
            }
        }
        return;
    }

    // ARCHIVE: ordinary play/pause.
    if playback.state.playing {
        playback.state.playing = false;
    } else if playback.state.is_playback_allowed() {
        playback.state.playing = true;
    }
}
