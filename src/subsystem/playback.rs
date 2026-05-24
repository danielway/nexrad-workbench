//! Playback subsystem: cursor position, speed, mode, animation.
//!
//! Owns the [`PlaybackState`] that drives the timeline cursor, the
//! playback-rate selector, the macro/micro mode flip, and the realtime
//! lock that keeps the cursor pinned to wall-clock time during live
//! streaming. UI panels (timeline, scrubber, transport buttons,
//! shortcuts) all reach into one type instead of touching the AppState
//! field directly.
//!
//! Splitting playback out of [`AppState`](crate::state::AppState) means
//! the subsystem boundary line — "this code touches the cursor" — is
//! visible in every function signature, and avoids the situation where
//! every other field on AppState reads as similarly playback-related.

use crate::state::PlaybackState;

/// Owner of cursor state, playback rate, and animation timing.
#[derive(Default)]
pub struct Playback {
    /// Cursor position (seconds since epoch), playback rate, macro/micro
    /// mode, animation flags, timeline view bounds, realtime time-model.
    pub state: PlaybackState,
}
