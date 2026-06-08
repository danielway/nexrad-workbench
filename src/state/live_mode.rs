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
    /// User clicked on timeline or used seek controls.
    UserSeeked,
    /// User used jog forward/backward buttons.
    UserJogged,
    /// Network or connection error.
    ConnectionError,
    /// User explicitly stopped live mode.
    UserStopped,
}

impl LiveExitReason {
    /// Human-readable message for the exit reason.
    pub fn message(&self) -> &'static str {
        match self {
            LiveExitReason::UserSeeked => "Live mode exited: timeline seek",
            LiveExitReason::UserJogged => "Live mode exited: manual step",
            LiveExitReason::ConnectionError => "Live mode error: connection lost",
            LiveExitReason::UserStopped => "Live mode stopped",
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
        self.error_message = None;
        self.last_exit_reason = None;
        self.pulse_phase = 0.0;
    }

    /// Stop live mode - transition to Idle with given reason.
    pub fn stop(&mut self, reason: LiveExitReason) {
        self.phase = LivePhase::Idle;
        self.phase_started_at = None;
        self.last_exit_reason = Some(reason);
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
        _is_volume_end: bool,
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
