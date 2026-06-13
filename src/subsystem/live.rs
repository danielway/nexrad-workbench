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

use crate::nexrad::projection::{new_shared_engine, Projection, SharedProjectionEngine};
use crate::nexrad::RealtimeChannel;
use crate::state::{AppMode, LiveModeState, LiveRadarModel, PlaybackState, RadarTimeline};

/// Inputs the per-frame `refresh` call reads from outside the
/// subsystem. Kept explicit so [`Live`] doesn't take a back-reference
/// to [`AppState`].
pub struct LiveRefreshInputs<'a> {
    pub radar_timeline: &'a RadarTimeline,
    pub playback: &'a PlaybackState,
    /// Available archive scan boundaries — fed to the engine for authoritative
    /// next-scan extent.
    pub archive_boundaries: &'a [crate::nexrad::ScanBoundary],
    /// This frame's wall clock (from `AppState::frame_now`).
    pub now: crate::state::FrameNow,
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
    /// The single, main-thread-shared projection engine. The streaming loop
    /// holds a clone and feeds it observations/listings while reading sleep
    /// targets; UI consumers read this same instance. One source of truth for
    /// all forward-looking timing.
    pub engine: SharedProjectionEngine,
    /// This frame's projection, cloned from the engine in [`Live::refresh`].
    /// Consumers read the plan (countdown, next-target) and live-scan from here
    /// instead of a copy written back onto `LiveModeState`.
    pub frame_projection: Option<Projection>,
}

impl Live {
    pub fn new(channel: RealtimeChannel) -> Self {
        Self {
            channel,
            mode_state: LiveModeState::default(),
            radar_model: LiveRadarModel::default(),
            app_mode: AppMode::default(),
            engine: new_shared_engine(),
            frame_projection: None,
        }
    }

    /// Stop live streaming: reset the state machine and the engine's volume
    /// observations together (the engine owns the observations now, so the old
    /// `LiveModeState::stop` field-clear must also clear them).
    pub fn stop(&mut self, reason: crate::state::LiveExitReason) {
        self.mode_state.stop(reason);
        self.engine.borrow_mut().reset_volume_observations();
    }

    /// Detach the playhead from the live edge.
    ///
    /// This is what every seek/jog/jump gesture does while live: the user goes
    /// browsing. By default (`pause_stream_while_reviewing == false`) the stream
    /// keeps ingesting at the right edge (the timeline keeps growing) and the
    /// now-cap offers an instant return; it only stops on an explicit stop, an
    /// error, a site change, or the detached idle timeout.
    ///
    /// When the data-saver policy (`pause_stream_while_reviewing`) is on,
    /// detaching stops the background stream immediately — this is the ONE
    /// place that policy is checked, so every seek/jog/jump call site routes
    /// through it. No-op on the stream when already detached or not streaming.
    pub fn detach_playhead(
        &mut self,
        playback: &mut PlaybackState,
        now: f64,
        pause_stream_while_reviewing: bool,
    ) {
        playback.exit_live(crate::state::FreezeAt::Keep);
        if !self.mode_state.is_active() {
            return;
        }
        if pause_stream_while_reviewing {
            // Data-saver: stop the moment the user starts reviewing.
            self.stop(crate::state::LiveExitReason::UserStopped);
            self.channel.stop();
            return;
        }
        if self.mode_state.detached_since.is_none() {
            self.mode_state.detached_since = Some(now);
        }
    }

    /// Whether the stream is running but the playhead has detached from the
    /// live edge (the user is browsing while ingestion continues).
    pub fn is_detached(&self, playback: &PlaybackState) -> bool {
        self.mode_state.is_active()
            && !playback.time_model.is_pinned()
            && !playback.time_model.is_lookback()
    }

    /// Seconds until the next chunk is expected to be available in S3 — drives
    /// the "next in Xs" countdown. `Some` only while waiting for a chunk, read
    /// from this frame's projection (no `LiveModeState.plan`).
    pub fn countdown_remaining_secs(&self, now: f64) -> Option<f64> {
        if self.mode_state.phase != crate::state::LivePhase::WaitingForChunk {
            return None;
        }
        self.frame_projection
            .as_ref()
            .and_then(|p| p.next_available_in_secs(now))
    }

    /// Recompute the derived `radar_model` and `app_mode` for this
    /// frame.
    ///
    /// Call once at the start of each UI frame so all consumers see
    /// consistent state derived from the same `now` timestamp.
    pub fn refresh(&mut self, inputs: LiveRefreshInputs<'_>) {
        let now = inputs.now.secs();
        // Adopt the engine's latest projection each frame while streaming, so
        // re-anchors and listing updates the loop fed between chunk arrivals
        // (e.g. during a long cross-volume wait) propagate to every surface —
        // not just on the next `ChunkReceived`. The engine is the single
        // producer; this is the live read that closes the desync.
        let live_scan = if self.mode_state.is_active() {
            self.engine
                .borrow_mut()
                .set_archive_boundaries(inputs.archive_boundaries.to_vec());
            let proj = self.engine.borrow().last_projection().cloned();
            if let Some(ref p) = proj {
                // Idempotent diagnostics snapshot — captured here each frame from
                // the engine's plan (the single producer) the moment VCP +
                // volume-start are both known, so no plan is written back onto
                // `LiveModeState`.
                self.mode_state.try_capture_volume_start_plan(&p.plan);
            }
            let live_scan = proj.as_ref().and_then(|p| p.live_scan.clone());
            self.frame_projection = proj;
            live_scan
        } else {
            self.frame_projection = None;
            None
        };
        self.radar_model = self.mode_state.compute_model(now, live_scan);
        let playhead_live =
            inputs.playback.time_model.is_pinned() || inputs.playback.time_model.is_lookback();
        self.app_mode = crate::state::derive_app_mode(
            self.mode_state.is_active(),
            playhead_live,
            inputs
                .radar_timeline
                .find_scan_at_timestamp(inputs.playback.playback_position())
                .is_some(),
        );
    }
}
