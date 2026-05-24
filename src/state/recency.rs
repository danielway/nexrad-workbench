//! Live-vs-archive recency gate for live-only overlays.
//!
//! Used by the NWS warnings, national mosaic, and mPING storm-report
//! overlays so they hide (and stop polling) when the user has scrubbed
//! to archive radar far enough behind wall-clock that current-time data
//! would be misleading.

use crate::state::playback::TimeModel;

/// Maximum lag from wall-clock at which "live" overlays remain meaningful.
/// Beyond this, the user is viewing archive data and overlays would mislead.
pub const LIVE_OVERLAY_MAX_LAG_SECS: f64 = 15.0 * 60.0;

/// True when the displayed scan is fresh enough that overlays reflecting
/// current real-world conditions are still appropriate.
pub fn data_is_live(playback: &super::PlaybackState) -> bool {
    let playback_ts = playback.playback_position();
    let now = TimeModel::wall_clock_time();
    (now - playback_ts) <= LIVE_OVERLAY_MAX_LAG_SECS
}
