//! Wall-clock constructors — the shell half of the playback time model.
//!
//! The playback types themselves are pure domain vocabulary
//! ([`crate::core::TimeModel`] / [`crate::core::PlaybackState`], see
//! `src/core/domain/playback.rs`); this module holds the impls that read the
//! browser clock: the wall-clock accessor and the constructors seeded from it.

use crate::core::{
    LoopMode, MacroPlaybackState, PlaybackDirection, PlaybackSpeed, PlaybackState, PlayheadMode,
    TimeModel,
};

impl Default for TimeModel {
    fn default() -> Self {
        Self {
            playback_position: Self::wall_clock_time(),
            mode: PlayheadMode::Free,
            playback_bounds: None,
            loop_mode: LoopMode::Loop,
            direction: PlaybackDirection::Forward,
        }
    }
}

impl TimeModel {
    /// Get current wall-clock time as Unix seconds.
    pub(crate) fn wall_clock_time() -> f64 {
        js_sys::Date::now() / 1000.0
    }

    /// Create a new time model at the given position.
    pub(crate) fn at_position(position: f64) -> Self {
        Self {
            playback_position: position,
            ..Default::default()
        }
    }
}

impl Default for PlaybackState {
    fn default() -> Self {
        let now = TimeModel::wall_clock_time();
        let zoom = 0.15; // ~0.15 px/sec means ~1.8 hours visible in 1000px
        let view_width_secs = 1000.0 / zoom;

        Self {
            playing: false,
            time_model: TimeModel::at_position(now),
            speed: PlaybackSpeed::default(),
            timeline_zoom: zoom,
            timeline_tier: Self::seed_tier(zoom, 1000.0),
            calendar_granularity: crate::core::BucketGranularity::seed(zoom),
            timeline_view_start: now - view_width_secs / 2.0,
            view_follows_now: true,
            selection: None,
            loop_window: None,
            pending_loop_window: None,
            timeline_width_px: 1000.0,
            macro_playback: MacroPlaybackState::default(),
        }
    }
}

impl PlaybackState {
    pub(crate) fn new_at_time(now: f64) -> Self {
        let zoom = 0.15;
        let view_width_secs = 1000.0 / zoom;

        Self {
            time_model: TimeModel::at_position(now),
            timeline_view_start: now - view_width_secs / 2.0,
            ..Default::default()
        }
    }
}
