//! Live mode state management.
//!
//! This module handles the state machine for real-time streaming mode,
//! including phase tracking, animation state, and exit conditions.
//!
//! # Timing model invariants
//!
//! `LiveModeState` owns **ACTUAL** state parsed from radial/message headers
//! (`current_volume.confirmed`, `completed_sweep_metas`,
//! `last_radial_time_secs`, `chunk_elev_spans`) plus the streaming state
//! machine. **PROJECTED** timing (the forward-looking [`crate::nexrad::StreamingPlan`])
//! is produced and owned by the shared [`crate::nexrad::projection::ProjectionEngine`];
//! the per-frame `Projection` is held on [`crate::subsystem::Live::frame_projection`]
//! and read from there (countdown, next-target, chunk-in-sweep), never written
//! back onto this struct.
//!
//! Live playhead (`TimeModel::playback_position` when `realtime_lock`
//! is on) deliberately tracks wall clock rather than clamping to the
//! latest received sweep's end. Clamping would cause visible stutter at
//! each chunk boundary; the canvas already resolves whichever sweep has
//! `end ≤ playback_position`, so wall-clock tracking satisfies principle
//! 1 (canvas shows ACTUAL data) without the stutter.
//!
//! # UI consumption convention
//!
//! UI code reads forward-looking timing from the frame `Projection`
//! ([`crate::subsystem::Live::frame_projection`] /
//! [`crate::subsystem::Live::countdown_remaining_secs`]) and frame-cached
//! derivations from [`crate::state::live_radar_model::LiveRadarModel`] (the
//! timeline ghost, VCP panel position, the in-progress sweep), rebuilt once
//! per frame so read sites in the same frame stay consistent. Diagnostics
//! snapshots come from [`LiveModeState::derive_current_volume_forecast`] / the
//! [`crate::state::derive_volume_forecast`] free function.

/// Whether a background (detached) live stream should auto-stop because the
/// playhead has stayed detached longer than `threshold_secs`.
///
/// Pure decision extracted from the egui tick loop ([`crate::app`]'s
/// `tick_live` Detached branch) so the auto-stop rule is testable in isolation.
/// `detached_since` is the wall-clock time (Unix seconds) the playhead detached
/// from the live edge; `None` (never detached) yields `0.0` elapsed and so
/// `false`. A `detached_since` in the future likewise yields a non-positive
/// elapsed and `false`. The comparison is strictly greater-than, so an elapsed
/// exactly equal to the threshold does not yet stop.
pub fn should_stop_for_detached_idle(
    detached_since: Option<f64>,
    now: f64,
    threshold_secs: f64,
) -> bool {
    let detached_for = detached_since.map(|t| now - t).unwrap_or(0.0);
    detached_for > threshold_secs
}

/// Live mode phase - current state in the streaming state machine.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum LivePhase {
    /// Not in live mode.
    #[default]
    Idle,
    /// Initial connection phase (typically 5-10 seconds).
    AcquiringLock,
    /// Actively receiving data.
    Streaming,
    /// Countdown to next chunk (10-15 second intervals).
    WaitingForChunk,
    /// Connection failed or lost.
    #[allow(dead_code)]
    Error,
}

impl LivePhase {
    /// Human-readable label for the phase.
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            LivePhase::Idle => "Idle",
            LivePhase::AcquiringLock => "CONNECTING",
            LivePhase::Streaming => "LIVE",
            LivePhase::WaitingForChunk => "WAITING",
            LivePhase::Error => "ERROR",
        }
    }

    /// Color for the phase indicator (RGB).
    #[allow(dead_code)]
    pub fn color(&self) -> (u8, u8, u8) {
        match self {
            LivePhase::Idle => (100, 100, 100),
            LivePhase::AcquiringLock => (255, 180, 50),
            LivePhase::Streaming => (255, 80, 80),
            LivePhase::WaitingForChunk => (100, 180, 255),
            LivePhase::Error => (255, 50, 50),
        }
    }
}

/// Reason why live mode was exited.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LiveExitReason {
    /// Network or connection error.
    ConnectionError,
    /// User explicitly stopped live mode.
    UserStopped,
    /// Stream auto-stopped after the playhead stayed detached too long.
    DetachedTimeout,
}

impl LiveExitReason {
    /// Human-readable message for the exit reason.
    pub fn message(&self) -> &'static str {
        match self {
            LiveExitReason::ConnectionError => "Live mode error: connection lost",
            LiveExitReason::UserStopped => "Live mode stopped",
            LiveExitReason::DetachedTimeout => {
                "Live stream stopped after extended browsing — GO LIVE to resume"
            }
        }
    }
}

/// Full state container for live mode.
pub struct LiveModeState {
    /// Current phase in the state machine
    pub phase: LivePhase,

    /// Timestamp when the current phase started (Unix seconds)
    pub phase_started_at: Option<f64>,

    /// Error message if in Error phase
    pub error_message: Option<String>,

    /// Reason for the last exit from live mode
    pub last_exit_reason: Option<LiveExitReason>,

    /// Number of chunks received in current session
    pub chunks_received: u32,

    /// Wall-clock time (Unix seconds) when the playhead detached from the
    /// live edge while the stream stayed running. `None` while pinned /
    /// replaying or when not streaming. Drives the detached idle-stop
    /// timeout so a backgrounded stream doesn't poll S3 forever.
    pub detached_since: Option<f64>,

    /// Animation pulse phase (0.0 to 1.0, wraps)
    pub pulse_phase: f32,

    /// Whether to auto-scroll timeline to follow live data.
    #[allow(dead_code)] // Used when auto-scroll feature is implemented
    pub auto_scroll_enabled: bool,

    /// Identity + provisional/confirmed start time for the current
    /// in-progress volume. `None` between volumes; populated when the first
    /// chunk's `ChunkData` is received and the [`LiveVolumeAnchor::confirmed`]
    /// field is filled in by `record_volume_header_time` once the worker
    /// reports the radial-parsed value. UI surfaces should read
    /// [`LiveVolumeAnchor::best_start_secs`] for display and
    /// [`LiveVolumeAnchor::scan_key`] for IDB lookups; both stay correct
    /// across the provisional → confirmed transition.
    pub current_volume: Option<crate::data::LiveVolumeAnchor>,

    /// Starting azimuth of the current in-progress sweep (first radial).
    /// Used to set the sweep compositing start angle for live partial rendering.
    pub sweep_start_azimuth: Option<f32>,

    /// Azimuth range of the last live-decoded partial sweep data.
    /// (first_azimuth, last_azimuth) from the actual sorted radials.
    /// Used for accurate sweep compositing instead of estimation.
    pub live_data_azimuth_range: Option<(f32, f32)>,

    /// Last known radial azimuth in degrees (0-360) from the most recent chunk.
    /// Used to extrapolate sweep line position between chunks.
    pub last_radial_azimuth: Option<f32>,

    /// Timestamp (Unix seconds) of the last known radial. Together with
    /// `last_radial_azimuth`, allows linear extrapolation of sweep line.
    pub last_radial_time_secs: Option<f64>,

    /// Plan as captured at the start of the current live volume — the
    /// moment both the VCP pattern and the volume-start timestamp are
    /// known and the streaming loop has emitted a plan. Preserves
    /// library-projected per-elevation predictions so they survive into
    /// the diagnostics modal even after the corresponding sweeps complete
    /// (their per-chunk `forecast` becomes `None` once they're past).
    ///
    /// The diagnostics modal derives a [`crate::state::VolumeForecastSnapshot`]
    /// on demand from this plan + the rolling observation state below;
    /// nothing in the live state is mutated as sweeps complete.
    pub volume_start_plan: Option<crate::nexrad::StreamingPlan>,

    /// Frozen inputs from the most recently completed volume, retained so
    /// the diagnostics modal can still render predicted-vs-actual data
    /// after the live state has rolled over. Replaces the old pre-derived
    /// `last_volume_forecast`; the snapshot is rebuilt on demand from
    /// this record via [`crate::state::derive_volume_forecast`].
    pub last_completed_volume: Option<crate::state::CompletedVolumeRecord>,

    /// Observed end timestamp of the previous volume (Unix seconds). Survives
    /// the reset in `handle_volume_complete` so the next volume's snapshot
    /// can compute its inter-volume gap.
    pub previous_volume_end_secs: Option<f64>,

    /// Per-chunk arrival diagnostics for the current volume. One entry per
    /// successful fetch, in arrival order. Reset on `handle_volume_complete`
    /// (a trimmed copy is attached to `last_volume_forecast` via the modal).
    pub chunk_arrivals: Vec<crate::state::ChunkArrivalStat>,

    /// Most recent volume's `chunk_arrivals`, preserved for the diagnostics
    /// modal alongside `last_volume_forecast`.
    pub last_chunk_arrivals: Vec<crate::state::ChunkArrivalStat>,
}

impl Default for LiveModeState {
    fn default() -> Self {
        Self {
            phase: LivePhase::Idle,
            phase_started_at: None,
            error_message: None,
            last_exit_reason: None,
            chunks_received: 0,
            detached_since: None,
            pulse_phase: 0.0,
            auto_scroll_enabled: true,
            current_volume: None,
            sweep_start_azimuth: None,
            live_data_azimuth_range: None,
            last_radial_azimuth: None,
            last_radial_time_secs: None,
            volume_start_plan: None,
            last_completed_volume: None,
            previous_volume_end_secs: None,
            chunk_arrivals: Vec::new(),
            last_chunk_arrivals: Vec::new(),
        }
    }
}

impl LiveModeState {
    /// Create a new idle live mode state.
    #[allow(dead_code)] // Convenience constructor
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a state initialized for testing with dummy streaming data.
    #[allow(dead_code)] // Used for testing different live mode states
    pub fn with_dummy_streaming(phase: LivePhase, now: f64) -> Self {
        let mut state = Self::new();
        state.phase = phase;
        state.phase_started_at = Some(now - 5.0); // Started 5 seconds ago

        match phase {
            LivePhase::Streaming => {
                state.chunks_received = 15;
            }
            LivePhase::WaitingForChunk => {
                state.chunks_received = 10;
                // Demo state — no real plan; the WaitingForChunk arm in
                // tests inspects only `phase`, not the countdown timing.
            }
            LivePhase::AcquiringLock => {
                // Just acquiring, no chunks yet
            }
            LivePhase::Error => {
                state.error_message = Some("Connection timeout".to_string());
            }
            LivePhase::Idle => {}
        }

        state
    }

    /// Start live mode - transition to AcquiringLock phase.
    pub fn start(&mut self, now: f64) {
        self.phase = LivePhase::AcquiringLock;
        self.phase_started_at = Some(now);
        self.chunks_received = 0;
        self.detached_since = None;
        self.error_message = None;
        self.last_exit_reason = None;
        self.pulse_phase = 0.0;
    }

    /// Stop live mode - transition to Idle with given reason.
    pub fn stop(&mut self, reason: LiveExitReason) {
        self.phase = LivePhase::Idle;
        self.phase_started_at = None;
        self.last_exit_reason = Some(reason);
        self.detached_since = None;
        self.current_volume = None;
        self.sweep_start_azimuth = None;
        self.live_data_azimuth_range = None;
        self.last_radial_azimuth = None;
        self.last_radial_time_secs = None;
        self.volume_start_plan = None;
        self.last_completed_volume = None;
        self.previous_volume_end_secs = None;
        self.chunk_arrivals.clear();
        self.last_chunk_arrivals.clear();
    }

    /// Set error state with message.
    #[allow(dead_code)]
    pub fn set_error(&mut self, message: String) {
        self.phase = LivePhase::Error;
        self.error_message = Some(message);
        self.last_exit_reason = Some(LiveExitReason::ConnectionError);
    }

    /// Transition to Streaming phase (lock acquired, receiving data).
    pub fn start_streaming(&mut self, now: f64) {
        self.phase = LivePhase::Streaming;
        self.phase_started_at = Some(now);
    }

    /// Transition to WaitingForChunk phase. The countdown displayed downstream
    /// is driven by the frame projection's next target if present.
    #[allow(dead_code)]
    pub fn wait_for_next_chunk(&mut self, now: f64) {
        self.phase = LivePhase::WaitingForChunk;
        self.phase_started_at = Some(now);
        self.chunks_received += 1;
    }

    /// Check if live mode is active (not Idle or Error).
    pub fn is_active(&self) -> bool {
        matches!(
            self.phase,
            LivePhase::AcquiringLock | LivePhase::Streaming | LivePhase::WaitingForChunk
        )
    }

    /// Get elapsed time in current phase.
    pub fn phase_elapsed_secs(&self, now: f64) -> f64 {
        self.phase_started_at
            .map(|start| now - start)
            .unwrap_or(0.0)
    }

    /// Update pulse animation state.
    pub fn update_pulse(&mut self, dt: f32) {
        if self.is_active() {
            // Pulse at ~1 Hz
            self.pulse_phase = (self.pulse_phase + dt) % 1.0;
        }
    }

    /// Get current pulse alpha value (0.0 to 1.0) for animation.
    pub fn pulse_alpha(&self) -> f32 {
        if !self.is_active() {
            return 0.0;
        }
        // Smooth sine wave pulse: 0.5 + 0.5 * sin(2π * phase)
        0.5 + 0.5 * (self.pulse_phase * std::f32::consts::TAU).sin()
    }

    /// Format status text for display.
    #[allow(dead_code)]
    pub fn status_text(&self, now: f64) -> String {
        match self.phase {
            LivePhase::Idle => String::new(),
            LivePhase::AcquiringLock => {
                let elapsed = self.phase_elapsed_secs(now) as i32;
                format!("Acquiring lock... {}s", elapsed)
            }
            LivePhase::Streaming => {
                format!("LIVE ({} chunks)", self.chunks_received)
            }
            LivePhase::WaitingForChunk => "Waiting for chunk...".to_string(),
            LivePhase::Error => self
                .error_message
                .clone()
                .unwrap_or_else(|| "Unknown error".to_string()),
        }
    }

    /// Handle a realtime streaming result and update state accordingly.
    ///
    /// This is the main integration point between the RealtimeChannel and
    /// the live mode state machine.
    pub fn handle_realtime_chunk(
        &mut self,
        chunks_in_volume: u32,
        now: f64,
        plan: Option<&crate::nexrad::StreamingPlan>,
    ) {
        self.chunks_received = chunks_in_volume;
        // Phase is gated by whether the plan has a `next_target`: when it
        // does, we're waiting for a known future chunk (the timeline shows
        // a countdown derived from the same plan); when it doesn't, we're
        // mid-receipt and the UI shows "receiving…".
        let has_target = plan.and_then(|p| p.next_target()).is_some();
        if has_target {
            self.phase = LivePhase::WaitingForChunk;
        } else {
            self.phase = LivePhase::Streaming;
        }
        self.phase_started_at = Some(now);
    }

    /// Snapshot `plan` as the volume's starting plan iff all three
    /// preconditions have just become satisfied: a plan exists (passed in from
    /// the engine), the VCP pattern is parsed, and the volume's start timestamp
    /// is known. Called once per frame from [`crate::subsystem::Live::refresh`]
    /// with the engine's plan. Idempotent: once captured, never overwrites
    /// until the next `handle_volume_complete` clears it.
    pub fn try_capture_volume_start_plan(&mut self, plan: &crate::nexrad::StreamingPlan) {
        if self.volume_start_plan.is_some() {
            return;
        }
        // A built plan implies the engine has a VCP; gate only on a known
        // volume start (the VCP pattern lives on the engine now).
        if self
            .current_volume
            .as_ref()
            .map(|a| a.best_start_secs())
            .is_none()
        {
            return;
        }
        self.volume_start_plan = Some(plan.clone());
    }

    /// Handle streaming started event.
    pub fn handle_streaming_started(&mut self, now: f64) {
        if self.phase == LivePhase::AcquiringLock {
            self.start_streaming(now);
        }
    }

    /// Handle volume complete event — seal the diagnostics record from the
    /// engine's `obs` and reset the live state. The caller resets the engine
    /// observations after this (seal-before-reset).
    pub fn handle_volume_complete(
        &mut self,
        now: f64,
        obs: &crate::nexrad::projection::VolumeObservations,
    ) {
        let volume_start_secs = self.current_volume.as_ref().map(|v| v.best_start_secs());

        // Package the just-completed volume's inputs into a record so the
        // diagnostics modal can derive a forecast snapshot for it after
        // the live state has rolled over. Requires the volume_start_plan
        // (captured at volume start), the VCP pattern, and a known
        // volume start; falls back to dropping the record if any are
        // missing.
        let prev_end = self.previous_volume_end_secs;
        if let (Some(start), Some(plan), Some(vcp)) = (
            volume_start_secs,
            self.volume_start_plan.take(),
            obs.current_vcp_pattern.clone(),
        ) {
            self.last_completed_volume = Some(crate::state::CompletedVolumeRecord {
                vcp,
                volume_start_plan: plan,
                volume_start_secs: start,
                volume_end_secs: now,
                previous_volume_end_secs: prev_end,
                completed_sweep_metas: obs.completed_sweep_metas.clone(),
                chunk_elev_spans: obs.chunk_elev_spans.clone(),
                chunk_arrivals: std::mem::take(&mut self.chunk_arrivals),
            });
        } else {
            // Couldn't seal a record; still drain chunk_arrivals so it
            // doesn't bleed into the next volume.
            self.chunk_arrivals.clear();
        }
        self.previous_volume_end_secs = Some(now);

        // Preserve the just-completed volume's per-chunk arrival stats for the
        // diagnostics modal alongside `last_completed_volume`'s record.
        self.last_chunk_arrivals = self
            .last_completed_volume
            .as_ref()
            .map(|r| r.chunk_arrivals.clone())
            .unwrap_or_default();

        self.phase = LivePhase::Streaming;
        self.phase_started_at = Some(now);
        self.current_volume = None;
        self.sweep_start_azimuth = None;
        self.live_data_azimuth_range = None;
        self.last_radial_azimuth = None;
        self.last_radial_time_secs = None;
    }

    /// Adopt or refresh the live volume anchor.
    ///
    /// When `scan_key` matches the current anchor this only fills in a
    /// confirmed start time if one has just been parsed; otherwise it
    /// replaces the anchor for a new volume. Either path triggers
    /// `try_capture_forecast` so a snapshot lands as soon as both the
    /// volume start and the VCP pattern are known, regardless of the order
    /// they arrive.
    pub fn set_or_confirm_volume(
        &mut self,
        scan_key: crate::data::ScanKey,
        provisional_secs: f64,
        confirmed_secs: Option<f64>,
    ) {
        use crate::data::{ConfirmedStart, LiveVolumeAnchor, ProvisionalStart};
        let same_volume = matches!(
            self.current_volume.as_ref(),
            Some(a) if a.scan_key == scan_key
        );
        if same_volume {
            if let Some(c) = confirmed_secs {
                let anchor = self
                    .current_volume
                    .as_mut()
                    .expect("same_volume implies Some");
                if anchor.confirmed.is_none() {
                    anchor.confirm(ConfirmedStart(c));
                }
            }
            return;
        }
        let mut anchor = LiveVolumeAnchor::new(scan_key, ProvisionalStart(provisional_secs));
        if let Some(c) = confirmed_secs {
            anchor.confirm(ConfirmedStart(c));
        }
        self.current_volume = Some(anchor);
    }

    /// Reset the decoder-side sweep-start azimuth on an elevation change. The
    /// engine's `VolumeObservations::record_in_progress_elevation` reports the
    /// change (it clears its own per-chunk az list); this clears the live field.
    /// `live_data_azimuth_range` is intentionally kept until the next
    /// `LiveDecoded` result arrives, to avoid a 1–2 frame compositing flash.
    pub fn on_in_progress_elevation_changed(&mut self) {
        self.sweep_start_azimuth = None;
    }

    /// Append a chunk arrival diagnostic sample for the current volume.
    pub fn record_chunk_arrival(&mut self, stat: crate::state::ChunkArrivalStat) {
        // Bound memory — clamp to 1024 per volume; anything beyond that is
        // pathological and unhelpful to the diagnostics modal.
        if self.chunk_arrivals.len() < 1024 {
            self.chunk_arrivals.push(stat);
        }
    }

    /// Back-fill the per-chunk collection-end time and (when available)
    /// empirical availability lag onto the most recent chunk arrival record.
    /// Both quantities come from the worker ingest (which parses the chunk's
    /// last-radial timestamp) and are dispatched together from `main.rs`.
    pub fn attach_collection_data_to_last_arrival(
        &mut self,
        collection_time_secs: f64,
        availability_lag_ms: Option<i64>,
    ) {
        if let Some(last) = self.chunk_arrivals.last_mut() {
            last.collection_time_secs = Some(collection_time_secs);
            if let Some(lag_ms) = availability_lag_ms {
                last.availability_lag_ms = Some(lag_ms);
            }
        }
    }

    /// Record last radial azimuth and timestamp from a chunk.
    pub fn record_last_radial(&mut self, azimuth: Option<f32>, time_secs: Option<f64>) {
        if let Some(az) = azimuth {
            self.last_radial_azimuth = Some(az);
        }
        if let Some(t) = time_secs {
            self.last_radial_time_secs = Some(t);
        }
    }

    /// Derive a forecast snapshot for the current in-progress volume from the
    /// captured `volume_start_plan` plus the engine's observations (`obs`) and
    /// the diagnostics state held here (chunk arrivals, previous volume end).
    /// Returns `None` when prerequisites aren't met. Called by the modal.
    pub fn derive_current_volume_forecast(
        &self,
        obs: &crate::nexrad::projection::VolumeObservations,
    ) -> Option<crate::state::VolumeForecastSnapshot> {
        let vcp = obs.current_vcp_pattern.as_ref()?;
        let plan = self.volume_start_plan.as_ref()?;
        let volume_start_secs = self.current_volume.as_ref().map(|a| a.best_start_secs())?;
        if vcp.elevations.is_empty() {
            return None;
        }
        Some(crate::state::derive_volume_forecast(
            vcp,
            plan,
            volume_start_secs,
            &obs.completed_sweep_metas,
            &obs.chunk_elev_spans,
            self.previous_volume_end_secs,
            &self.chunk_arrivals,
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{ScanKey, UnixMillis};
    use crate::nexrad::projection::VolumeObservations;
    use crate::nexrad::StreamingPlan;
    use crate::state::ChunkArrivalStat;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn scan_key(start_ms: i64) -> ScanKey {
        ScanKey::new("KDMX", UnixMillis(start_ms))
    }

    /// A `StreamingPlan` whose `next_target()` resolves to a current-volume
    /// chunk (drives the WaitingForChunk branch).
    fn plan_with_target() -> StreamingPlan {
        StreamingPlan::with_next_target_key_for_test(Some((0, 1)))
    }

    /// A `StreamingPlan` with no `next_target` (drives the Streaming branch).
    fn plan_without_target() -> StreamingPlan {
        StreamingPlan::with_next_target_key_for_test(None)
    }

    fn vcp_with_one_elevation() -> crate::data::keys::ExtractedVcp {
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};
        ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle: 0.5,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(20.0),
            }],
        }
    }

    // ── should_stop_for_detached_idle (extracted predicate) ──

    #[wasm_bindgen_test]
    fn detached_idle_none_is_never_stop() {
        // Never detached → 0 elapsed → false regardless of threshold.
        assert!(!should_stop_for_detached_idle(None, 10_000.0, 60.0));
    }

    #[wasm_bindgen_test]
    fn detached_idle_just_under_threshold_does_not_stop() {
        // 59.9s detached, 60s threshold → false.
        assert!(!should_stop_for_detached_idle(
            Some(1000.0),
            1000.0 + 59.9,
            60.0
        ));
    }

    #[wasm_bindgen_test]
    fn detached_idle_exactly_at_threshold_does_not_stop() {
        // Strictly greater-than: == threshold is not yet a stop.
        assert!(!should_stop_for_detached_idle(
            Some(1000.0),
            1000.0 + 60.0,
            60.0
        ));
    }

    #[wasm_bindgen_test]
    fn detached_idle_just_over_threshold_stops() {
        assert!(should_stop_for_detached_idle(
            Some(1000.0),
            1000.0 + 60.1,
            60.0
        ));
    }

    #[wasm_bindgen_test]
    fn detached_idle_future_detached_since_does_not_stop() {
        // detached_since in the future → negative elapsed → false.
        assert!(!should_stop_for_detached_idle(Some(2000.0), 1000.0, 60.0));
    }

    // ── start() ──

    #[wasm_bindgen_test]
    fn start_resets_session_fields_and_lands_acquiring_lock() {
        for phase in [
            LivePhase::Idle,
            LivePhase::AcquiringLock,
            LivePhase::Streaming,
            LivePhase::WaitingForChunk,
            LivePhase::Error,
        ] {
            let mut s = LiveModeState::with_dummy_streaming(phase, 100.0);
            // Dirty the session fields so we can prove start() clears them.
            s.chunks_received = 42;
            s.detached_since = Some(50.0);
            s.error_message = Some("boom".to_string());
            s.last_exit_reason = Some(LiveExitReason::ConnectionError);
            s.pulse_phase = 0.7;

            s.start(200.0);

            assert_eq!(s.phase, LivePhase::AcquiringLock, "phase from {:?}", phase);
            assert_eq!(s.phase_started_at, Some(200.0));
            assert_eq!(s.chunks_received, 0);
            assert_eq!(s.detached_since, None);
            assert_eq!(s.error_message, None);
            assert_eq!(s.last_exit_reason, None);
            assert_eq!(s.pulse_phase, 0.0);
        }
    }

    // ── stop() ──

    #[wasm_bindgen_test]
    fn stop_zeroes_volume_and_records_exit_reason() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Streaming;
        s.phase_started_at = Some(10.0);
        s.detached_since = Some(20.0);
        s.current_volume = Some(crate::data::LiveVolumeAnchor::new(
            scan_key(1_700_000_000_000),
            crate::data::ProvisionalStart(1.0),
        ));
        s.sweep_start_azimuth = Some(30.0);
        s.live_data_azimuth_range = Some((0.0, 90.0));
        s.last_radial_azimuth = Some(45.0);
        s.last_radial_time_secs = Some(99.0);
        s.volume_start_plan = Some(plan_without_target());
        s.previous_volume_end_secs = Some(80.0);
        s.chunk_arrivals
            .push(ChunkArrivalStat::minimal_for_test(1, 5.0));
        s.last_chunk_arrivals
            .push(ChunkArrivalStat::minimal_for_test(2, 6.0));

        s.stop(LiveExitReason::UserStopped);

        assert_eq!(s.phase, LivePhase::Idle);
        assert_eq!(s.phase_started_at, None);
        assert_eq!(s.last_exit_reason, Some(LiveExitReason::UserStopped));
        assert_eq!(s.detached_since, None);
        assert!(s.current_volume.is_none());
        assert_eq!(s.sweep_start_azimuth, None);
        assert_eq!(s.live_data_azimuth_range, None);
        assert_eq!(s.last_radial_azimuth, None);
        assert_eq!(s.last_radial_time_secs, None);
        assert!(s.volume_start_plan.is_none());
        assert!(s.last_completed_volume.is_none());
        assert_eq!(s.previous_volume_end_secs, None);
        assert!(s.chunk_arrivals.is_empty());
        assert!(s.last_chunk_arrivals.is_empty());
    }

    // ── start_streaming() ──

    #[wasm_bindgen_test]
    fn start_streaming_sets_phase_and_timestamp() {
        let mut s = LiveModeState::new();
        s.start_streaming(123.0);
        assert_eq!(s.phase, LivePhase::Streaming);
        assert_eq!(s.phase_started_at, Some(123.0));
    }

    // ── handle_streaming_started() ──

    #[wasm_bindgen_test]
    fn handle_streaming_started_only_promotes_acquiring_lock() {
        // From AcquiringLock → Streaming, stamping now.
        let mut s = LiveModeState::new();
        s.phase = LivePhase::AcquiringLock;
        s.phase_started_at = Some(1.0);
        s.handle_streaming_started(50.0);
        assert_eq!(s.phase, LivePhase::Streaming);
        assert_eq!(s.phase_started_at, Some(50.0));

        // No-op from any other phase (phase + timestamp untouched).
        for phase in [
            LivePhase::Idle,
            LivePhase::Streaming,
            LivePhase::WaitingForChunk,
            LivePhase::Error,
        ] {
            let mut s = LiveModeState::new();
            s.phase = phase;
            s.phase_started_at = Some(7.0);
            s.handle_streaming_started(50.0);
            assert_eq!(s.phase, phase, "no-op from {:?}", phase);
            assert_eq!(s.phase_started_at, Some(7.0), "timestamp from {:?}", phase);
        }
    }

    // ── is_active() truth table ──

    #[wasm_bindgen_test]
    fn is_active_truth_table() {
        let active = |p: LivePhase| {
            let mut s = LiveModeState::new();
            s.phase = p;
            s.is_active()
        };
        assert!(!active(LivePhase::Idle));
        assert!(active(LivePhase::AcquiringLock));
        assert!(active(LivePhase::Streaming));
        assert!(active(LivePhase::WaitingForChunk));
        assert!(!active(LivePhase::Error));
    }

    // ── handle_realtime_chunk phase gating ──

    #[wasm_bindgen_test]
    fn handle_realtime_chunk_plan_none_goes_streaming() {
        let mut s = LiveModeState::new();
        s.chunks_received = 99;
        s.handle_realtime_chunk(3, 200.0, None);
        assert_eq!(s.phase, LivePhase::Streaming);
        assert_eq!(s.chunks_received, 3, "overwritten, not incremented");
        assert_eq!(s.phase_started_at, Some(200.0));
    }

    #[wasm_bindgen_test]
    fn handle_realtime_chunk_plan_without_target_goes_streaming() {
        let mut s = LiveModeState::new();
        s.chunks_received = 99;
        let plan = plan_without_target();
        s.handle_realtime_chunk(5, 200.0, Some(&plan));
        assert_eq!(s.phase, LivePhase::Streaming);
        assert_eq!(s.chunks_received, 5);
        assert_eq!(s.phase_started_at, Some(200.0));
    }

    #[wasm_bindgen_test]
    fn handle_realtime_chunk_plan_with_target_waits_for_chunk() {
        let mut s = LiveModeState::new();
        s.chunks_received = 99;
        let plan = plan_with_target();
        assert!(plan.next_target().is_some(), "fixture sanity");
        s.handle_realtime_chunk(7, 200.0, Some(&plan));
        assert_eq!(s.phase, LivePhase::WaitingForChunk);
        assert_eq!(s.chunks_received, 7);
        assert_eq!(s.phase_started_at, Some(200.0));
    }

    // ── set_or_confirm_volume ──

    #[wasm_bindgen_test]
    fn set_or_confirm_volume_new_key_replaces_and_best_start_tracks() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, None);
        let a = s.current_volume.as_ref().unwrap();
        assert_eq!(a.scan_key, scan_key(1000));
        assert_eq!(a.best_start_secs(), 100.0, "provisional pre-confirm");
        assert!(a.confirmed.is_none());

        // A different key fully replaces the anchor.
        s.set_or_confirm_volume(scan_key(2000), 200.0, Some(205.0));
        let a = s.current_volume.as_ref().unwrap();
        assert_eq!(a.scan_key, scan_key(2000));
        assert_eq!(a.best_start_secs(), 205.0, "confirmed wins on new anchor");
    }

    #[wasm_bindgen_test]
    fn set_or_confirm_volume_same_key_confirm_flips_best_start() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, None);
        assert_eq!(s.current_volume.as_ref().unwrap().best_start_secs(), 100.0);

        // Same key + confirmed fills confirmed (was None) → best_start flips.
        s.set_or_confirm_volume(scan_key(1000), 100.0, Some(102.0));
        let a = s.current_volume.as_ref().unwrap();
        assert_eq!(a.confirmed.map(|c| c.0), Some(102.0));
        assert_eq!(a.best_start_secs(), 102.0, "confirmed post-confirm");
        // scan_key stays stable across the provisional→confirmed transition.
        assert_eq!(a.scan_key, scan_key(1000));
    }

    #[wasm_bindgen_test]
    fn set_or_confirm_volume_does_not_overwrite_existing_confirmed() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, Some(102.0));
        // A second confirm with a different value must NOT overwrite.
        s.set_or_confirm_volume(scan_key(1000), 100.0, Some(999.0));
        assert_eq!(
            s.current_volume.as_ref().unwrap().confirmed.map(|c| c.0),
            Some(102.0),
            "idempotent confirm preserves first value"
        );
    }

    #[wasm_bindgen_test]
    fn set_or_confirm_volume_same_key_no_confirm_is_noop() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, None);
        // Same key, no confirmed → no-op; provisional stays as first set.
        s.set_or_confirm_volume(scan_key(1000), 555.0, None);
        let a = s.current_volume.as_ref().unwrap();
        assert!(a.confirmed.is_none());
        assert_eq!(a.provisional.0, 100.0, "provisional not re-set on same key");
        assert_eq!(a.best_start_secs(), 100.0);
    }

    // ── try_capture_volume_start_plan ──

    #[wasm_bindgen_test]
    fn try_capture_volume_start_plan_gated_on_current_volume() {
        let mut s = LiveModeState::new();
        // No current_volume → cannot capture.
        s.try_capture_volume_start_plan(&plan_without_target());
        assert!(s.volume_start_plan.is_none());
    }

    #[wasm_bindgen_test]
    fn try_capture_volume_start_plan_captures_once_then_is_idempotent() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, None);

        // First call (with a volume present) captures, revision 0.
        let first = StreamingPlan::with_next_target_key_for_test(None);
        s.try_capture_volume_start_plan(&first);
        assert!(s.volume_start_plan.is_some());
        let captured_rev = s.volume_start_plan.as_ref().unwrap().revision;

        // A second call with a *different* plan must NOT overwrite.
        let mut second = StreamingPlan::with_next_target_key_for_test(None);
        second.revision = 7;
        s.try_capture_volume_start_plan(&second);
        assert_eq!(
            s.volume_start_plan.as_ref().unwrap().revision,
            captured_rev,
            "capture is idempotent; second plan ignored"
        );
    }

    // ── handle_volume_complete ──

    #[wasm_bindgen_test]
    fn handle_volume_complete_seal_path() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, Some(101.0));
        s.volume_start_plan = Some(plan_without_target());
        s.previous_volume_end_secs = Some(50.0);
        s.chunk_arrivals
            .push(ChunkArrivalStat::minimal_for_test(1, 5.0));
        s.chunk_arrivals
            .push(ChunkArrivalStat::minimal_for_test(2, 6.0));
        // Dirty the azimuth fields to prove they're nulled on rollover.
        s.sweep_start_azimuth = Some(10.0);
        s.last_radial_azimuth = Some(20.0);
        s.last_radial_time_secs = Some(30.0);

        let mut obs = VolumeObservations::default();
        obs.current_vcp_pattern = Some(vcp_with_one_elevation());

        s.handle_volume_complete(300.0, &obs);

        // Record sealed with the moved chunk arrivals.
        let rec = s.last_completed_volume.as_ref().expect("sealed record");
        assert_eq!(rec.chunk_arrivals.len(), 2);
        assert_eq!(rec.volume_start_secs, 101.0);
        assert_eq!(rec.volume_end_secs, 300.0);
        assert_eq!(rec.previous_volume_end_secs, Some(50.0));
        // chunk_arrivals moved out (now empty); copy preserved in last_chunk_arrivals.
        assert!(s.chunk_arrivals.is_empty());
        assert_eq!(s.last_chunk_arrivals.len(), 2);
        // Rollover bookkeeping.
        assert_eq!(s.previous_volume_end_secs, Some(300.0));
        assert_eq!(s.phase, LivePhase::Streaming);
        assert_eq!(s.phase_started_at, Some(300.0));
        assert!(s.current_volume.is_none());
        assert_eq!(s.sweep_start_azimuth, None);
        assert_eq!(s.live_data_azimuth_range, None);
        assert_eq!(s.last_radial_azimuth, None);
        assert_eq!(s.last_radial_time_secs, None);
        // volume_start_plan was taken into the record.
        assert!(s.volume_start_plan.is_none());
    }

    #[wasm_bindgen_test]
    fn handle_volume_complete_drop_path_still_clears_arrivals() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, Some(101.0));
        // volume_start_plan ABSENT → cannot seal → drop path.
        s.previous_volume_end_secs = Some(50.0);
        s.chunk_arrivals
            .push(ChunkArrivalStat::minimal_for_test(1, 5.0));
        s.chunk_arrivals
            .push(ChunkArrivalStat::minimal_for_test(2, 6.0));

        let mut obs = VolumeObservations::default();
        obs.current_vcp_pattern = Some(vcp_with_one_elevation());

        s.handle_volume_complete(300.0, &obs);

        // No record sealed, but arrivals MUST still be drained (the prior bug).
        assert!(s.last_completed_volume.is_none());
        assert!(
            s.chunk_arrivals.is_empty(),
            "drop path still clears arrivals"
        );
        assert!(s.last_chunk_arrivals.is_empty());
        // The volume still rolls over.
        assert_eq!(s.previous_volume_end_secs, Some(300.0));
        assert_eq!(s.phase, LivePhase::Streaming);
        assert_eq!(s.phase_started_at, Some(300.0));
        assert!(s.current_volume.is_none());
    }

    // ── record_chunk_arrival cap ──

    #[wasm_bindgen_test]
    fn record_chunk_arrival_caps_at_1024() {
        let mut s = LiveModeState::new();
        for i in 0..1025u32 {
            s.record_chunk_arrival(ChunkArrivalStat::minimal_for_test(i, i as f64));
        }
        assert_eq!(s.chunk_arrivals.len(), 1024);
        // The 1025th (sequence 1024) was dropped; the last kept is sequence 1023.
        assert_eq!(s.chunk_arrivals.last().unwrap().sequence, 1023);
    }

    // ── attach_collection_data_to_last_arrival ──

    #[wasm_bindgen_test]
    fn attach_collection_data_noop_on_empty() {
        let mut s = LiveModeState::new();
        // No arrivals → no-op (no panic).
        s.attach_collection_data_to_last_arrival(123.0, Some(456));
        assert!(s.chunk_arrivals.is_empty());
    }

    #[wasm_bindgen_test]
    fn attach_collection_data_sets_fields_and_lag_only_when_some() {
        let mut s = LiveModeState::new();
        s.record_chunk_arrival(ChunkArrivalStat::minimal_for_test(1, 5.0));

        // Some(lag) sets both collection time and lag.
        s.attach_collection_data_to_last_arrival(123.0, Some(456));
        let last = s.chunk_arrivals.last().unwrap();
        assert_eq!(last.collection_time_secs, Some(123.0));
        assert_eq!(last.availability_lag_ms, Some(456));

        // A later None refreshes collection time but preserves the prior lag.
        s.attach_collection_data_to_last_arrival(200.0, None);
        let last = s.chunk_arrivals.last().unwrap();
        assert_eq!(last.collection_time_secs, Some(200.0));
        assert_eq!(
            last.availability_lag_ms,
            Some(456),
            "None must not clear an existing lag"
        );
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::data::{ScanKey, UnixMillis};
    use crate::nexrad::projection::VolumeObservations;
    use crate::nexrad::StreamingPlan;
    use crate::state::ChunkArrivalStat;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn scan_key(start_ms: i64) -> ScanKey {
        ScanKey::new("KDMX", UnixMillis(start_ms))
    }

    fn plan_without_target() -> StreamingPlan {
        StreamingPlan::with_next_target_key_for_test(None)
    }

    fn vcp_with_one_elevation() -> crate::data::keys::ExtractedVcp {
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};
        ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle: 0.5,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(20.0),
            }],
        }
    }

    fn vcp_empty() -> crate::data::keys::ExtractedVcp {
        use crate::data::keys::ExtractedVcp;
        ExtractedVcp {
            number: 215,
            elevations: vec![],
        }
    }

    // ── LivePhase::default / label / color ──

    #[wasm_bindgen_test]
    fn live_phase_default_is_idle() {
        assert_eq!(LivePhase::default(), LivePhase::Idle);
    }

    #[wasm_bindgen_test]
    fn live_phase_labels_each_variant() {
        assert_eq!(LivePhase::Idle.label(), "Idle");
        assert_eq!(LivePhase::AcquiringLock.label(), "CONNECTING");
        assert_eq!(LivePhase::Streaming.label(), "LIVE");
        assert_eq!(LivePhase::WaitingForChunk.label(), "WAITING");
        assert_eq!(LivePhase::Error.label(), "ERROR");
    }

    #[wasm_bindgen_test]
    fn live_phase_colors_each_variant() {
        assert_eq!(LivePhase::Idle.color(), (100, 100, 100));
        assert_eq!(LivePhase::AcquiringLock.color(), (255, 180, 50));
        assert_eq!(LivePhase::Streaming.color(), (255, 80, 80));
        assert_eq!(LivePhase::WaitingForChunk.color(), (100, 180, 255));
        assert_eq!(LivePhase::Error.color(), (255, 50, 50));
    }

    // ── LiveExitReason::message ──

    #[wasm_bindgen_test]
    fn live_exit_reason_messages() {
        assert_eq!(
            LiveExitReason::ConnectionError.message(),
            "Live mode error: connection lost"
        );
        assert_eq!(LiveExitReason::UserStopped.message(), "Live mode stopped");
        assert_eq!(
            LiveExitReason::DetachedTimeout.message(),
            "Live stream stopped after extended browsing — GO LIVE to resume"
        );
    }

    // ── Default / new() field invariants not asserted by sibling tests ──

    #[wasm_bindgen_test]
    fn new_state_is_idle_with_clean_defaults() {
        let s = LiveModeState::new();
        assert_eq!(s.phase, LivePhase::Idle);
        assert_eq!(s.phase_started_at, None);
        assert_eq!(s.chunks_received, 0);
        assert_eq!(s.detached_since, None);
        assert_eq!(s.pulse_phase, 0.0);
        assert!(s.auto_scroll_enabled);
        assert!(s.current_volume.is_none());
        assert!(s.chunk_arrivals.is_empty());
        assert!(s.last_chunk_arrivals.is_empty());
        assert!(!s.is_active());
    }

    // ── phase_elapsed_secs ──

    #[wasm_bindgen_test]
    fn phase_elapsed_none_start_is_zero() {
        let mut s = LiveModeState::new();
        s.phase_started_at = None;
        assert_eq!(s.phase_elapsed_secs(500.0), 0.0);
    }

    #[wasm_bindgen_test]
    fn phase_elapsed_some_start_subtracts() {
        let mut s = LiveModeState::new();
        s.phase_started_at = Some(100.0);
        assert!((s.phase_elapsed_secs(137.5) - 37.5).abs() < 1e-9);
    }

    // ── set_error ──

    #[wasm_bindgen_test]
    fn set_error_enters_error_phase_with_message_and_reason() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Streaming;
        s.set_error("boom".to_string());
        assert_eq!(s.phase, LivePhase::Error);
        assert_eq!(s.error_message.as_deref(), Some("boom"));
        assert_eq!(s.last_exit_reason, Some(LiveExitReason::ConnectionError));
    }

    // ── wait_for_next_chunk ──

    #[wasm_bindgen_test]
    fn wait_for_next_chunk_sets_phase_and_increments_count() {
        let mut s = LiveModeState::new();
        s.chunks_received = 4;
        s.wait_for_next_chunk(250.0);
        assert_eq!(s.phase, LivePhase::WaitingForChunk);
        assert_eq!(s.phase_started_at, Some(250.0));
        assert_eq!(s.chunks_received, 5, "incremented, not overwritten");
    }

    // ── update_pulse ──

    #[wasm_bindgen_test]
    fn update_pulse_inactive_phase_is_noop() {
        for phase in [LivePhase::Idle, LivePhase::Error] {
            let mut s = LiveModeState::new();
            s.phase = phase;
            s.pulse_phase = 0.3;
            s.update_pulse(0.4);
            assert_eq!(s.pulse_phase, 0.3, "no advance while inactive ({phase:?})");
        }
    }

    #[wasm_bindgen_test]
    fn update_pulse_active_advances_by_dt() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Streaming;
        s.pulse_phase = 0.2;
        s.update_pulse(0.3);
        assert!((s.pulse_phase - 0.5).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn update_pulse_active_wraps_past_one() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::WaitingForChunk;
        s.pulse_phase = 0.9;
        s.update_pulse(0.2);
        // (0.9 + 0.2) % 1.0 == 0.1
        assert!((s.pulse_phase - 0.1).abs() < 1e-6, "got {}", s.pulse_phase);
    }

    // ── pulse_alpha ──

    #[wasm_bindgen_test]
    fn pulse_alpha_inactive_is_zero() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Idle;
        s.pulse_phase = 0.25; // would be 1.0 if active; inactive forces 0.0
        assert_eq!(s.pulse_alpha(), 0.0);
    }

    #[wasm_bindgen_test]
    fn pulse_alpha_active_sine_endpoints() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Streaming;
        // phase 0 → 0.5 + 0.5*sin(0) = 0.5
        s.pulse_phase = 0.0;
        assert!((s.pulse_alpha() - 0.5).abs() < 1e-6);
        // phase 0.25 → 0.5 + 0.5*sin(π/2) = 1.0
        s.pulse_phase = 0.25;
        assert!((s.pulse_alpha() - 1.0).abs() < 1e-6);
        // phase 0.75 → 0.5 + 0.5*sin(3π/2) = 0.0
        s.pulse_phase = 0.75;
        assert!(s.pulse_alpha().abs() < 1e-6);
    }

    // ── status_text ──

    #[wasm_bindgen_test]
    fn status_text_idle_is_empty() {
        let s = LiveModeState::new();
        assert_eq!(s.status_text(999.0), "");
    }

    #[wasm_bindgen_test]
    fn status_text_acquiring_lock_shows_truncated_elapsed() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::AcquiringLock;
        s.phase_started_at = Some(100.0);
        // elapsed 5.9s truncates to 5
        assert_eq!(s.status_text(105.9), "Acquiring lock... 5s");
    }

    #[wasm_bindgen_test]
    fn status_text_streaming_shows_chunk_count() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Streaming;
        s.chunks_received = 42;
        assert_eq!(s.status_text(0.0), "LIVE (42 chunks)");
    }

    #[wasm_bindgen_test]
    fn status_text_waiting_is_fixed_string() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::WaitingForChunk;
        assert_eq!(s.status_text(0.0), "Waiting for chunk...");
    }

    #[wasm_bindgen_test]
    fn status_text_error_uses_message_or_fallback() {
        let mut s = LiveModeState::new();
        s.phase = LivePhase::Error;
        // With a message.
        s.error_message = Some("connection timeout".to_string());
        assert_eq!(s.status_text(0.0), "connection timeout");
        // Without a message → fallback.
        s.error_message = None;
        assert_eq!(s.status_text(0.0), "Unknown error");
    }

    // ── record_last_radial selective updates ──

    #[wasm_bindgen_test]
    fn record_last_radial_only_overwrites_provided_fields() {
        let mut s = LiveModeState::new();
        s.last_radial_azimuth = Some(10.0);
        s.last_radial_time_secs = Some(20.0);

        // None args must leave both fields untouched.
        s.record_last_radial(None, None);
        assert_eq!(s.last_radial_azimuth, Some(10.0));
        assert_eq!(s.last_radial_time_secs, Some(20.0));

        // Some azimuth, None time → only azimuth updates.
        s.record_last_radial(Some(33.0), None);
        assert_eq!(s.last_radial_azimuth, Some(33.0));
        assert_eq!(s.last_radial_time_secs, Some(20.0));

        // Some time, None azimuth → only time updates.
        s.record_last_radial(None, Some(44.0));
        assert_eq!(s.last_radial_azimuth, Some(33.0));
        assert_eq!(s.last_radial_time_secs, Some(44.0));
    }

    // ── on_in_progress_elevation_changed ──

    #[wasm_bindgen_test]
    fn on_in_progress_elevation_changed_clears_only_sweep_start() {
        let mut s = LiveModeState::new();
        s.sweep_start_azimuth = Some(90.0);
        s.live_data_azimuth_range = Some((0.0, 180.0));
        s.on_in_progress_elevation_changed();
        assert_eq!(s.sweep_start_azimuth, None);
        assert_eq!(
            s.live_data_azimuth_range,
            Some((0.0, 180.0)),
            "az range kept to avoid compositing flash"
        );
    }

    // ── derive_current_volume_forecast None paths ──

    #[wasm_bindgen_test]
    fn derive_forecast_none_when_no_vcp_pattern() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, None);
        s.volume_start_plan = Some(plan_without_target());
        let obs = VolumeObservations::default(); // current_vcp_pattern is None
        assert!(s.derive_current_volume_forecast(&obs).is_none());
    }

    #[wasm_bindgen_test]
    fn derive_forecast_none_when_no_volume_start_plan() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, None);
        // volume_start_plan stays None.
        let mut obs = VolumeObservations::default();
        obs.current_vcp_pattern = Some(vcp_with_one_elevation());
        assert!(s.derive_current_volume_forecast(&obs).is_none());
    }

    #[wasm_bindgen_test]
    fn derive_forecast_none_when_no_current_volume() {
        let mut s = LiveModeState::new();
        s.volume_start_plan = Some(plan_without_target());
        // current_volume stays None.
        let mut obs = VolumeObservations::default();
        obs.current_vcp_pattern = Some(vcp_with_one_elevation());
        assert!(s.derive_current_volume_forecast(&obs).is_none());
    }

    #[wasm_bindgen_test]
    fn derive_forecast_none_when_vcp_elevations_empty() {
        let mut s = LiveModeState::new();
        s.set_or_confirm_volume(scan_key(1000), 100.0, None);
        s.volume_start_plan = Some(plan_without_target());
        let mut obs = VolumeObservations::default();
        obs.current_vcp_pattern = Some(vcp_empty()); // empty elevations → None
        assert!(s.derive_current_volume_forecast(&obs).is_none());
    }

    // ── try_capture_volume_start_plan early-return when already captured ──

    #[wasm_bindgen_test]
    fn try_capture_volume_start_plan_already_some_returns_early_even_without_volume() {
        let mut s = LiveModeState::new();
        // No current_volume, but a plan is already captured (revision 3).
        let mut existing = plan_without_target();
        existing.revision = 3;
        s.volume_start_plan = Some(existing);
        // current_volume is None — would normally also block — but the
        // already-some guard fires first; the new plan is ignored.
        let mut incoming = plan_without_target();
        incoming.revision = 9;
        s.try_capture_volume_start_plan(&incoming);
        assert_eq!(s.volume_start_plan.as_ref().unwrap().revision, 3);
    }

    // ── chunk arrival cap boundary: exactly 1024 still accepts the 1024th ──

    #[wasm_bindgen_test]
    fn record_chunk_arrival_accepts_up_to_cap_then_rejects() {
        let mut s = LiveModeState::new();
        for i in 0..1024u32 {
            s.record_chunk_arrival(ChunkArrivalStat::minimal_for_test(i, i as f64));
        }
        assert_eq!(s.chunk_arrivals.len(), 1024);
        // At cap, further pushes are rejected and last stays sequence 1023.
        s.record_chunk_arrival(ChunkArrivalStat::minimal_for_test(9999, 0.0));
        assert_eq!(s.chunk_arrivals.len(), 1024);
        assert_eq!(s.chunk_arrivals.last().unwrap().sequence, 1023);
    }
}
