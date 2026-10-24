//! Live-vs-archive recency gate for live-only overlays.
//!
//! Used by the NWS warnings and national mosaic overlays so they hide (and
//! stop polling) when what is shown is far enough behind wall-clock that
//! current-time data would be misleading: the playhead, or, while a range
//! plays, the whole loop (see [`crate::core::recency`]). Read once per frame
//! into [`crate::subsystem::Derived`], whose `data_is_live()` every consumer
//! gates on.
//!
//! mPING deliberately does **not** gate on this: storm reports are historical
//! observations, so they stay meaningful for archive playback and the mPING
//! manager fetches a window around the playhead instead.

use crate::core::recency::{LiveOverlaysOff, PlayingLoop};
use crate::core::TimeModel;

/// Why overlays reflecting current real-world conditions are off, or `None`
/// while they still apply — the one recency rule
/// ([`crate::core::recency::live_overlays_off`]) read against the wall clock
/// and the playback state (playhead, selection, playing).
pub(crate) fn live_overlays_off(playback: &crate::core::PlaybackState) -> Option<LiveOverlaysOff> {
    crate::core::recency::live_overlays_off(
        TimeModel::wall_clock_time(),
        playback.playback_position(),
        PlayingLoop::of(playback.playing, &playback.selection),
    )
}
