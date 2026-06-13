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
