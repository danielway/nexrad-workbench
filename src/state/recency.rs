//! Live-vs-archive recency gate for live-only overlays.
//!
//! Used by the NWS warnings and national mosaic overlays so they hide (and
//! stop polling) when the user has scrubbed to archive radar far enough behind
//! wall-clock that current-time data would be misleading.
//!
//! mPING deliberately does **not** gate on this: storm reports are historical
//! observations, so they stay meaningful for archive playback and the mPING
//! manager fetches a window around the playhead instead.

use crate::core::TimeModel;

/// True when the playhead is close enough to now that overlays reflecting
/// current real-world conditions are still appropriate — the one recency
/// rule ([`crate::core::recency::live_overlays_apply`]) read against the
/// wall clock.
pub(crate) fn data_is_live(playback: &crate::core::PlaybackState) -> bool {
    crate::core::recency::live_overlays_apply(
        TimeModel::wall_clock_time(),
        playback.playback_position(),
    )
}
