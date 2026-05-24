//! Live subsystem: real-time NEXRAD streaming and the top-level
//! app-mode derivation that flows from it.
//!
//! Owns the worker-driven [`RealtimeChannel`] plus the live-streaming
//! state machine ([`LiveModeState`]) and its per-frame derivations
//! ([`LiveRadarModel`], [`AppMode`]). Folding these together replaces
//! four scattered fields on [`AppState`] (one source-of-truth + two
//! derived models + the channel) with a single owner whose responsibility
//! ("is the app streaming, and if so what is the data state?") is
//! coherent.
//!
//! The per-frame [`Live::refresh`] call recomputes the derived models
//! once at the top of the update loop so every UI consumer sees the
//! same snapshot for that frame.

use crate::nexrad::RealtimeChannel;
use crate::state::{AppMode, LiveModeState, LiveRadarModel, PlaybackState, RadarTimeline};

/// Inputs the per-frame `refresh` call reads from outside the
/// subsystem. Kept explicit so [`Live`] doesn't take a back-reference
/// to [`AppState`].
pub struct LiveRefreshInputs<'a> {
    pub radar_timeline: &'a RadarTimeline,
    pub playback: &'a PlaybackState,
}

/// Owner of the real-time streaming pipeline and the derived
/// app-mode model.
pub struct Live {
    /// Worker-driven streaming-loop handle. Drains observation +
    /// chunk results each frame.
    pub channel: RealtimeChannel,
    /// Live streaming-loop state machine (lock acquisition → streaming
    /// → waiting-for-chunk transitions, pulse animation, chunk-arrival
    /// tracking). Source of truth for whether the app is streaming.
    pub mode_state: LiveModeState,
    /// Per-frame snapshot of the live radar model — derived from
    /// `mode_state.compute_model(now)` at the start of every frame so
    /// UI consumers within one frame see a consistent picture.
    pub radar_model: LiveRadarModel,
    /// Per-frame top-level app mode (Idle / Archive / Live), derived
    /// from `mode_state.is_active()` + whether a scan exists at the
    /// playback cursor.
    pub app_mode: AppMode,
}

impl Live {
    pub fn new(channel: RealtimeChannel) -> Self {
        Self {
            channel,
            mode_state: LiveModeState::default(),
            radar_model: LiveRadarModel::default(),
            app_mode: AppMode::default(),
        }
    }

    /// Recompute the derived `radar_model` and `app_mode` for this
    /// frame.
    ///
    /// Call once at the start of each UI frame so all consumers see
    /// consistent state derived from the same `now` timestamp.
    pub fn refresh(&mut self, inputs: LiveRefreshInputs<'_>) {
        let now = js_sys::Date::now() / 1000.0;
        self.radar_model = self.mode_state.compute_model(now);
        self.app_mode = if self.mode_state.is_active() {
            AppMode::Live
        } else if inputs
            .radar_timeline
            .find_scan_at_timestamp(inputs.playback.playback_position())
            .is_some()
        {
            AppMode::Archive
        } else {
            AppMode::Idle
        };
    }
}
