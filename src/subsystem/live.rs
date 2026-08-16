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

use crate::core::projection::{new_shared_engine, Projection, SharedProjectionEngine};
use crate::core::PlaybackState;
use crate::core::RadarTimeline;
use crate::core::{ElevationSelection, StreamingFilter};
use crate::core::{ExpectedSweep, LiveModeState, LiveRadarModel};
use crate::nexrad::RealtimeChannel;
use crate::state::AppMode;

/// Inputs the per-frame `refresh` call reads from outside the
/// subsystem. Kept explicit so [`Live`] doesn't take a back-reference
/// to [`AppState`].
pub(crate) struct LiveRefreshInputs<'a> {
    pub radar_timeline: &'a RadarTimeline,
    pub playback: &'a PlaybackState,
    /// Available archive scan boundaries — fed to the engine for authoritative
    /// next-scan extent.
    pub archive_boundaries: &'a [crate::core::ScanBoundary],
    /// This frame's wall clock (from `AppState::frame_now`).
    pub now: crate::core::FrameNow,
    /// Current elevation selection — compared against the plan filter so a
    /// just-changed cut cannot keep the previous sweep's stall.
    pub elevation_selection: &'a ElevationSelection,
}

/// Owner of the real-time streaming pipeline and the derived
/// app-mode model.
pub(crate) struct Live {
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
    /// This frame's live-status view-model — the single snapshot every live
    /// surface (LIVE button, activity chip, now-cap, mode badge, mobile)
    /// projects from. Rebuilt at the end of [`Live::refresh`].
    pub frame_status: crate::core::LiveStatus,
}

impl Live {
    pub(crate) fn new(channel: RealtimeChannel) -> Self {
        Self {
            channel,
            mode_state: LiveModeState::default(),
            radar_model: LiveRadarModel::default(),
            app_mode: AppMode::default(),
            engine: new_shared_engine(),
            frame_projection: None,
            frame_status: crate::core::LiveStatus::default(),
        }
    }

    /// Stop live streaming: reset the state machine and the engine's volume
    /// observations together (the engine owns the observations now, so the old
    /// `LiveModeState::stop` field-clear must also clear them).
    pub(crate) fn stop(&mut self, reason: crate::core::LiveExitReason) {
        self.mode_state.stop(reason);
        self.engine.borrow_mut().reset_volume_observations();
    }

    /// Detach the playhead from the live edge — the subsystem wrapper around
    /// the pure [`crate::core::transport::detach_playhead`], which owns the
    /// decision (including the data-saver policy). This layer only executes the
    /// one described effect the core can't reach: stopping the worker channel.
    ///
    /// This is what every seek/jog/jump gesture does while live: the user goes
    /// browsing.
    pub(crate) fn detach_playhead(
        &mut self,
        playback: &mut PlaybackState,
        now: f64,
        pause_stream_while_reviewing: bool,
    ) {
        let actions = crate::core::transport::detach_playhead(
            &crate::core::transport::TransportEnv {
                now_secs: now,
                pause_stream_while_reviewing,
            },
            crate::core::transport::TransportSlices {
                live_mode: &mut self.mode_state,
                engine: &mut self.engine.borrow_mut(),
                playback,
            },
        );
        if actions.stop_channel {
            self.channel.stop();
        }
    }

    /// Whether the stream is running but the playhead has detached from the
    /// live edge (the user is browsing while ingestion continues).
    pub(crate) fn is_detached(&self, playback: &PlaybackState) -> bool {
        self.mode_state.is_active()
            && !playback.time_model.is_pinned()
            && !playback.time_model.is_lookback()
    }

    /// Seconds until the next chunk is expected to be available in S3 — drives
    /// the "next in Xs" countdown. `Some` only while waiting for a chunk, read
    /// from this frame's projection (no `LiveModeState.plan`).
    pub(crate) fn countdown_remaining_secs(&self, now: f64) -> Option<f64> {
        if self.mode_state.phase != crate::core::LivePhase::WaitingForChunk {
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
    pub(crate) fn refresh(&mut self, inputs: LiveRefreshInputs<'_>) {
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
        // Last: the status snapshot reads the projection adopted above.
        let active_filter = StreamingFilter::from(inputs.elevation_selection);
        let mut expected_sweep = self
            .frame_projection
            .as_ref()
            .and_then(|p| ExpectedSweep::from_plan(&p.plan, active_filter));
        if let (Some(exp), Some(scan)) = (
            expected_sweep.as_mut(),
            self.frame_projection
                .as_ref()
                .and_then(|p| p.live_scan.as_ref()),
        ) {
            if let Some(n) = exp.elevation_number {
                exp.elevation_angle = scan
                    .sweeps
                    .iter()
                    .chain(scan.next_scan_ghost.iter().flat_map(|g| g.sweeps.iter()))
                    .find(|s| s.elevation_number == n)
                    .map(|s| s.elevation_angle);
            }
        }
        self.frame_status = crate::core::derive_live_status(crate::core::LiveStatusInputs {
            mode_state: &self.mode_state,
            countdown_secs: self.countdown_remaining_secs(now),
            expected_sweep,
            tethered: playhead_live,
            playback_position_secs: inputs.playback.playback_position(),
            now_secs: now,
        });
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use crate::core::{LiveExitReason, LivePhase};
    use crate::core::{LoopBasis, PlaybackState};
    use crate::nexrad::RealtimeChannel;

    fn live() -> Live {
        Live::new(RealtimeChannel::new())
    }

    // ---- Live::new defaults -------------------------------------------

    #[wasm_bindgen_test]
    fn new_starts_idle_and_unprojected() {
        let l = live();
        assert_eq!(l.app_mode, crate::state::AppMode::Idle);
        assert!(l.frame_projection.is_none());
        assert!(!l.mode_state.is_active());
        assert_eq!(l.mode_state.phase, LivePhase::Idle);
        assert!(!l.channel.is_active());
        assert_eq!(l.mode_state.detached_since, None);
    }

    // ---- Live::stop ----------------------------------------------------

    #[wasm_bindgen_test]
    fn stop_resets_mode_state_and_records_reason() {
        let mut l = live();
        // Put it in an active, detached state first.
        l.mode_state.phase = LivePhase::Streaming;
        l.mode_state.detached_since = Some(12.0);
        assert!(l.mode_state.is_active());

        l.stop(LiveExitReason::ConnectionError);

        assert_eq!(l.mode_state.phase, LivePhase::Idle);
        assert!(!l.mode_state.is_active());
        assert_eq!(l.mode_state.detached_since, None);
        assert_eq!(
            l.mode_state.last_exit_reason,
            Some(LiveExitReason::ConnectionError)
        );
    }

    #[wasm_bindgen_test]
    fn stop_preserves_exact_reason_variant() {
        let mut l = live();
        l.stop(LiveExitReason::DetachedTimeout);
        assert_eq!(
            l.mode_state.last_exit_reason,
            Some(LiveExitReason::DetachedTimeout)
        );
    }

    // ---- Live::is_detached --------------------------------------------

    #[wasm_bindgen_test]
    fn is_detached_false_when_stream_inactive() {
        let l = live(); // mode_state Idle => not active
        let pb = PlaybackState::default(); // mode defaults to Free
        assert!(!l.is_detached(&pb));
    }

    #[wasm_bindgen_test]
    fn is_detached_true_when_active_and_playhead_free() {
        let mut l = live();
        l.mode_state.phase = LivePhase::Streaming;
        let pb = PlaybackState::default(); // Free
        assert!(l.is_detached(&pb));
    }

    #[wasm_bindgen_test]
    fn is_detached_false_when_pinned() {
        let mut l = live();
        l.mode_state.phase = LivePhase::Streaming;
        let mut pinned = PlaybackState::default();
        pinned.enter_pinned_live(1000.0);
        assert!(!l.is_detached(&pinned));
    }

    #[wasm_bindgen_test]
    fn is_detached_false_when_lookback() {
        let mut l = live();
        l.mode_state.phase = LivePhase::Streaming;
        let mut lookback = PlaybackState::default();
        lookback.enter_pinned_live(1000.0);
        lookback.enter_lookback(None, LoopBasis::default());
        assert!(!l.is_detached(&lookback));
    }

    // ---- Live::countdown_remaining_secs -------------------------------

    #[wasm_bindgen_test]
    fn countdown_none_when_not_waiting_for_chunk() {
        let mut l = live();
        // Streaming (active) but not WaitingForChunk -> None regardless of proj.
        l.mode_state.phase = LivePhase::Streaming;
        assert_eq!(l.countdown_remaining_secs(1000.0), None);
    }

    #[wasm_bindgen_test]
    fn countdown_none_when_waiting_but_no_frame_projection() {
        let mut l = live();
        l.mode_state.phase = LivePhase::WaitingForChunk;
        // frame_projection defaults to None at construction.
        assert!(l.frame_projection.is_none());
        assert_eq!(l.countdown_remaining_secs(1000.0), None);
    }

    // ---- Live::detach_playhead ----------------------------------------

    #[wasm_bindgen_test]
    fn detach_playhead_inactive_only_exits_live() {
        let mut l = live(); // mode_state Idle => inactive
        let mut pb = PlaybackState::default();
        pb.enter_pinned_live(1000.0);

        l.detach_playhead(&mut pb, 500.0, false);

        // exit_live(Keep) flips the playhead out of pinned-live...
        assert!(!pb.time_model.is_pinned());
        assert!(!pb.time_model.is_lookback());
        // ...but the inactive stream/state is untouched.
        assert_eq!(l.mode_state.phase, LivePhase::Idle);
        assert_eq!(l.mode_state.detached_since, None);
        assert!(!l.channel.is_active());
    }

    #[wasm_bindgen_test]
    fn detach_playhead_data_saver_stops_stream() {
        let mut l = live();
        l.mode_state.phase = LivePhase::Streaming;
        let mut pb = PlaybackState::default();
        pb.enter_pinned_live(1000.0);

        l.detach_playhead(&mut pb, 700.0, /* pause_stream_while_reviewing */ true);

        assert!(!pb.time_model.is_pinned());
        assert_eq!(l.mode_state.phase, LivePhase::Idle);
        assert!(!l.mode_state.is_active());
        assert_eq!(
            l.mode_state.last_exit_reason,
            Some(LiveExitReason::UserStopped)
        );
        assert!(!l.channel.is_active());
        // Data-saver path never latches detached_since (it stopped instead).
        assert_eq!(l.mode_state.detached_since, None);
    }

    #[wasm_bindgen_test]
    fn detach_playhead_default_latches_detached_since_once() {
        let mut l = live();
        l.mode_state.phase = LivePhase::Streaming;
        let mut pb = PlaybackState::default();
        pb.enter_pinned_live(1000.0);

        l.detach_playhead(&mut pb, 123.0, false);
        assert!(!pb.time_model.is_pinned());
        assert_eq!(l.mode_state.detached_since, Some(123.0));
        // Stream keeps running in the non-data-saver path.
        assert!(l.mode_state.is_active());

        // Second detach at a later time does NOT overwrite the latch.
        l.detach_playhead(&mut pb, 999.0, false);
        assert_eq!(l.mode_state.detached_since, Some(123.0));
    }
}
