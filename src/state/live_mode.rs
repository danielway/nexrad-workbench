//! Live mode state management.
//!
//! This module handles the state machine for real-time streaming mode,
//! including phase tracking, animation state, and exit conditions.

use std::time::Duration;

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
    /// User pressed pause (reserved for future pause-vs-stop distinction).
    #[allow(dead_code)]
    UserPaused,
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
            LiveExitReason::UserPaused => "Live mode paused",
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

    /// Typical interval between chunks in seconds (~12s)
    pub chunk_interval_secs: f64,

    /// Expected arrival time of next chunk (Unix seconds)
    pub next_chunk_expected_at: Option<f64>,

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

    // ── Real-time partial scan tracking for timeline visualization ────
    /// Elevation numbers received in the current in-progress volume.
    pub elevations_received: Vec<u8>,

    /// Total expected elevation count from the current VCP.
    pub expected_elevation_count: Option<u8>,

    /// VCP number of the current/last volume (for projecting scan boundaries).
    pub current_vcp_number: Option<u16>,

    /// Full extracted VCP pattern from Message Type 5 (for live panel display).
    pub current_vcp_pattern: Option<crate::data::keys::ExtractedVcp>,

    /// Duration of the last completed volume scan in seconds.
    pub last_volume_duration_secs: Option<f64>,

    /// Start timestamp of the current in-progress volume (Unix seconds).
    pub current_volume_start: Option<f64>,

    /// Scan key of the current in-progress volume (e.g., "KDMX|1700000000000").
    /// Used to identify and skip this scan in normal timeline rendering.
    pub current_scan_key: Option<String>,

    /// Elevation number of the sweep currently being accumulated (partial).
    pub current_in_progress_elevation: Option<u8>,

    /// Number of radials received for the current in-progress elevation.
    pub current_in_progress_radials: Option<u32>,

    /// Per-elevation chunk time spans in the current volume. Each entry is
    /// (elevation_number, start_secs, end_secs, radial_count) derived from
    /// actual radial collection timestamps. Each chunk contains data for
    /// exactly one elevation, so each chunk produces exactly one entry.
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,

    /// Actual sweep metadata (with real timestamps) for completed elevations
    /// in the current volume. Used for accurate sweep positioning on the timeline
    /// instead of even-distribution estimates.
    pub completed_sweep_metas: Vec<crate::data::SweepMeta>,

    /// Per-elevation estimated sweep durations (seconds), computed from VCP
    /// azimuth rates using the weight = 1/rate method. Index corresponds to
    /// elevation index (0-based). Populated when VCP data is received.
    pub estimated_sweep_durations: Vec<f64>,

    /// Per-chunk azimuth ranges for the current in-progress elevation.
    /// Each entry: (first_az, last_az, radial_count). Reset on elevation change.
    pub current_elev_chunks: Vec<(f32, f32, u32)>,

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

    /// Library-projected volume end time (Unix seconds).
    /// From nexrad-data's physics-based model (sweep_duration = 360/rate - 0.67s).
    pub projected_volume_end_secs: Option<f64>,

    /// Per-chunk projection info from the library's physics model.
    /// Structural metadata covers all chunks; projected times only for future chunks.
    /// Updated each time a new chunk arrives.
    pub chunk_projections: Option<Vec<crate::nexrad::ChunkProjectionInfo>>,

    /// Diagnostic snapshot of the current live volume's forecast vs. actuals.
    /// Populated at volume start (when both VCP pattern and volume_start are
    /// known) by `try_capture_forecast`. Used by the VCP forecast diagnostics
    /// modal to let the user compare predicted and observed values.
    pub current_volume_forecast: Option<crate::state::VolumeForecastSnapshot>,

    /// Most recently completed `current_volume_forecast`, moved here by
    /// `handle_volume_complete`. Kept so the diagnostics modal still has
    /// something to display immediately after a volume ends and before the
    /// next one's snapshot is captured.
    pub last_volume_forecast: Option<crate::state::VolumeForecastSnapshot>,

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
            chunk_interval_secs: 12.0,
            next_chunk_expected_at: None,
            error_message: None,
            last_exit_reason: None,
            chunks_received: 0,
            pulse_phase: 0.0,
            auto_scroll_enabled: true,
            elevations_received: Vec::new(),
            expected_elevation_count: None,
            current_vcp_number: None,
            current_vcp_pattern: None,
            last_volume_duration_secs: None,
            current_volume_start: None,
            current_scan_key: None,
            current_in_progress_elevation: None,
            current_in_progress_radials: None,
            chunk_elev_spans: Vec::new(),
            completed_sweep_metas: Vec::new(),
            estimated_sweep_durations: Vec::new(),
            current_elev_chunks: Vec::new(),
            sweep_start_azimuth: None,
            live_data_azimuth_range: None,
            last_radial_azimuth: None,
            last_radial_time_secs: None,
            projected_volume_end_secs: None,
            chunk_projections: None,
            current_volume_forecast: None,
            last_volume_forecast: None,
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
                state.next_chunk_expected_at = Some(now + 8.0); // 8 seconds remaining
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
        self.next_chunk_expected_at = None;
        self.last_exit_reason = Some(reason);
        self.elevations_received.clear();
        self.current_volume_start = None;
        self.current_scan_key = None;
        self.current_in_progress_elevation = None;
        self.current_in_progress_radials = None;
        self.chunk_elev_spans.clear();
        self.completed_sweep_metas.clear();
        self.estimated_sweep_durations.clear();
        self.current_elev_chunks.clear();
        self.sweep_start_azimuth = None;
        self.live_data_azimuth_range = None;
        self.last_radial_azimuth = None;
        self.last_radial_time_secs = None;
        self.projected_volume_end_secs = None;
        self.chunk_projections = None;
        self.current_volume_forecast = None;
        self.last_volume_forecast = None;
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

    /// Transition to WaitingForChunk phase with expected next chunk time.
    #[allow(dead_code)]
    pub fn wait_for_next_chunk(&mut self, now: f64) {
        self.phase = LivePhase::WaitingForChunk;
        self.phase_started_at = Some(now);
        self.next_chunk_expected_at = Some(now + self.chunk_interval_secs);
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

    /// Get remaining countdown for WaitingForChunk phase.
    pub fn countdown_remaining_secs(&self, now: f64) -> Option<f64> {
        if self.phase == LivePhase::WaitingForChunk {
            self.next_chunk_expected_at
                .map(|expected| (expected - now).max(0.0))
        } else {
            None
        }
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
            LivePhase::WaitingForChunk => {
                if let Some(remaining) = self.countdown_remaining_secs(now) {
                    format!("Next chunk in {}s", remaining.ceil() as i32)
                } else {
                    "Waiting for chunk...".to_string()
                }
            }
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
        time_until_next: Option<Duration>,
        is_volume_end: bool,
        now: f64,
        projected_volume_end_secs: Option<f64>,
        chunk_projections: Option<Vec<crate::nexrad::ChunkProjectionInfo>>,
    ) {
        self.chunks_received = chunks_in_volume;
        self.projected_volume_end_secs = projected_volume_end_secs;
        self.chunk_projections = chunk_projections;

        if is_volume_end {
            // Volume complete - transition to Streaming briefly
            self.phase = LivePhase::Streaming;
            self.phase_started_at = Some(now);
        } else if let Some(duration) = time_until_next {
            // Waiting for next chunk
            self.phase = LivePhase::WaitingForChunk;
            self.phase_started_at = Some(now);
            self.next_chunk_expected_at = Some(now + duration.as_secs_f64());
            self.chunk_interval_secs = duration.as_secs_f64();
        } else {
            // Actively receiving
            self.phase = LivePhase::Streaming;
            self.phase_started_at = Some(now);
        }
    }

    /// Handle streaming started event.
    pub fn handle_streaming_started(&mut self, now: f64) {
        if self.phase == LivePhase::AcquiringLock {
            self.start_streaming(now);
        }
    }

    /// Handle volume complete event — compute duration and reset elevation tracking.
    pub fn handle_volume_complete(&mut self, now: f64) {
        // Compute volume duration from the start we tracked
        if let Some(start) = self.current_volume_start {
            let dur = now - start;
            if dur > 0.0 && dur < 1200.0 {
                self.last_volume_duration_secs = Some(dur);
            }
        }

        // Seal the forecast snapshot and preserve it for the diagnostics modal.
        // Move it into `last_volume_forecast` so the modal has something to show
        // while the next volume is spinning up.
        if let Some(mut snap) = self.current_volume_forecast.take() {
            snap.actual_volume_end = Some(now);
            self.last_volume_forecast = Some(snap);
        }
        self.previous_volume_end_secs = Some(now);

        // Preserve the just-completed volume's per-chunk arrival stats for the
        // diagnostics modal, then reset for the next volume.
        self.last_chunk_arrivals = std::mem::take(&mut self.chunk_arrivals);

        self.phase = LivePhase::Streaming;
        self.phase_started_at = Some(now);
        self.elevations_received.clear();
        self.current_volume_start = None;
        self.current_scan_key = None;
        self.current_in_progress_elevation = None;
        self.current_in_progress_radials = None;
        self.chunk_elev_spans.clear();
        self.completed_sweep_metas.clear();
        self.estimated_sweep_durations.clear();
        self.current_elev_chunks.clear();
        self.sweep_start_azimuth = None;
        self.live_data_azimuth_range = None;
        self.last_radial_azimuth = None;
        self.last_radial_time_secs = None;
        self.projected_volume_end_secs = None;
        self.chunk_projections = None;
    }

    /// Record that new elevation cuts were received in the current volume.
    pub fn record_elevations(&mut self, elevations: &[u8], volume_start: f64) {
        if self.current_volume_start.is_none() {
            self.current_volume_start = Some(volume_start);
        }
        for &e in elevations {
            if !self.elevations_received.contains(&e) {
                self.elevations_received.push(e);
            }
        }
        self.elevations_received.sort_unstable();
    }

    /// Record a chunk's per-elevation time spans (from radial collection timestamps).
    pub fn record_chunk_elev_spans(&mut self, spans: &[(u8, f64, f64, u32)]) {
        self.chunk_elev_spans.extend_from_slice(spans);
    }

    /// Update completed sweep metadata from the worker's ingest result.
    /// Replaces the full list each time since the worker returns all completed
    /// sweeps for the current volume.
    pub fn update_sweep_metas(&mut self, metas: Vec<crate::data::SweepMeta>) {
        self.completed_sweep_metas = metas;
        self.fill_forecast_actuals();
    }

    /// Record which elevation is currently being accumulated (partial sweep).
    /// Resets `sweep_start_azimuth` when the elevation changes.
    pub fn record_in_progress_elevation(&mut self, elevation: Option<u8>, radials: Option<u32>) {
        let elevation_changed = elevation != self.current_in_progress_elevation;
        if elevation_changed {
            self.current_elev_chunks.clear();
            self.sweep_start_azimuth = None;
            // Keep live_data_azimuth_range until the LiveDecoded result arrives
            // with the new elevation's data. Clearing it here would disable
            // shader compositing for 1-2 frames, causing a visible flash.
        }
        self.current_in_progress_elevation = elevation;
        self.current_in_progress_radials = radials;

        if elevation_changed {
            if let Some(new_elev) = elevation {
                self.capture_mid_prediction(new_elev);
            }
        }
    }

    /// Record VCP info from an ingest result. When a full `ExtractedVcp` with
    /// elevation data is available, also computes per-elevation sweep durations.
    pub fn record_vcp(&mut self, vcp: &crate::data::keys::ExtractedVcp) {
        self.current_vcp_number = Some(vcp.number);
        self.expected_elevation_count = Some(vcp.elevations.len() as u8);
        if !vcp.elevations.is_empty() {
            self.current_vcp_pattern = Some(vcp.clone());

            // Only compute hand-rolled estimates when the library projection isn't available.
            // The library's physics model is more accurate (includes inter-sweep gaps and
            // the -0.67s correction), so prefer it when we have it.
            if self.chunk_projections.is_none() {
                // Seed volume duration from VCP azimuth rates if we haven't measured one yet.
                if self.last_volume_duration_secs.is_none() {
                    if let Some(estimated) = vcp.estimated_volume_duration() {
                        self.last_volume_duration_secs = Some(estimated);
                    }
                }

                let vol_dur = self.last_volume_duration_secs.unwrap_or(300.0);
                self.estimated_sweep_durations = vcp.sweep_durations(vol_dur);
            }
        }
        self.try_capture_forecast();
    }

    /// Append a chunk arrival diagnostic sample for the current volume.
    pub fn record_chunk_arrival(&mut self, stat: crate::state::ChunkArrivalStat) {
        // Bound memory — clamp to 1024 per volume; anything beyond that is
        // pathological and unhelpful to the diagnostics modal.
        if self.chunk_arrivals.len() < 1024 {
            self.chunk_arrivals.push(stat);
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

    /// Capture a cold-start forecast snapshot for the current volume, if the
    /// prerequisites are met and we don't already have one.
    ///
    /// Called at the end of `record_vcp` and also from the main update loop
    /// right after `current_volume_start` is first set — either one may run
    /// first depending on the order chunks arrive.
    pub fn try_capture_forecast(&mut self) {
        if self.current_volume_forecast.is_some() {
            return;
        }
        let Some(vol_start) = self.current_volume_start else {
            return;
        };
        let Some(vcp) = self.current_vcp_pattern.as_ref() else {
            return;
        };
        if vcp.elevations.is_empty() {
            return;
        }

        let vcp_number = vcp.number;
        let vcp_name = crate::state::get_vcp_definition(vcp_number).map(|d| d.name);
        let is_clear_air = crate::data::vcp::is_clear_air_vcp(vcp_number);

        let total_vol_dur = vcp.estimated_volume_duration().unwrap_or(300.0);
        let predicted_volume_end = self
            .projected_volume_end_secs
            .unwrap_or(vol_start + total_vol_dur);
        let sweep_durations = vcp.sweep_durations(total_vol_dur);

        // Group chunk_projections by elevation for per-sweep predictions.
        let chunk_projections_available = self.chunk_projections.is_some();
        let projected_per_elev: Option<
            std::collections::BTreeMap<u8, (f64, f64, u32, f64)>, // (min_time, max_time, chunk_count, rate)
        > = self.chunk_projections.as_ref().map(|projs| {
            let mut map: std::collections::BTreeMap<u8, (f64, f64, u32, f64)> =
                std::collections::BTreeMap::new();
            for chunk in projs {
                if let Some(e) = chunk.elevation_number {
                    let entry = map.entry(e as u8).or_insert((
                        f64::MAX,
                        f64::MIN,
                        0u32,
                        chunk.azimuth_rate_dps,
                    ));
                    entry.2 += 1;
                    if let Some(t) = chunk.projected_time_secs {
                        entry.0 = entry.0.min(t);
                        entry.1 = entry.1.max(t);
                    }
                    if entry.3 <= 0.0 {
                        entry.3 = chunk.azimuth_rate_dps;
                    }
                }
            }
            map
        });

        let mut sweeps: Vec<crate::state::SweepForecast> = Vec::with_capacity(vcp.elevations.len());
        let mut cum_offset = 0.0f64;

        for (idx, elev) in vcp.elevations.iter().enumerate() {
            let elev_number = (idx + 1) as u8;
            let weighted_dur = sweep_durations
                .get(idx)
                .copied()
                .unwrap_or(total_vol_dur / vcp.elevations.len() as f64);

            let fallback_rate = crate::data::vcp::fallback_azimuth_rate(
                is_clear_air,
                &elev.waveform,
                elev.prf_number,
            );

            // Rate selection priority: library projection > VCP msg > Method B fallback.
            let proj = projected_per_elev
                .as_ref()
                .and_then(|m| m.get(&elev_number))
                .copied();
            let (rate_used, rate_source) = match (proj, elev.azimuth_rate) {
                (Some((_, _, _, r)), _) if r > 0.0 => {
                    (r, crate::state::RateSource::ProjectionLibrary)
                }
                (_, Some(r)) if r > 0.0 => (r as f64, crate::state::RateSource::VcpMessage),
                _ => (fallback_rate, crate::state::RateSource::MethodBFallback),
            };

            // Prefer library projection bounds when usable; otherwise use the
            // VCP-weighted cumulative offset (same cascade as VcpPositionModel).
            // The last chunk publishes at the *start* of its bucket; the sweep
            // runs for one more bucket after that. Add `sweep_dur / N` to max_t.
            let (predicted_start, predicted_end) = match proj {
                Some((min_t, max_t, chunk_count, rate)) if min_t < f64::MAX => {
                    let end = if max_t > f64::MIN && rate > 0.0 && chunk_count > 0 {
                        let sweep_dur = (360.0 / rate - 0.67).max(0.0);
                        let bucket = sweep_dur / chunk_count as f64;
                        max_t + bucket
                    } else if max_t > f64::MIN {
                        max_t
                    } else if rate > 0.0 {
                        min_t + (360.0 / rate - 0.67).max(0.0)
                    } else {
                        min_t + weighted_dur
                    };
                    (min_t, end)
                }
                _ => (
                    vol_start + cum_offset,
                    vol_start + cum_offset + weighted_dur,
                ),
            };

            let predicted_duration = (predicted_end - predicted_start).max(0.0);
            let predicted_chunks = proj.map(|(_, _, count, _)| count);

            sweeps.push(crate::state::SweepForecast {
                elev_number,
                elev_angle: elev.angle,
                waveform: elev.waveform.clone(),
                prf_number: elev.prf_number,
                is_sails: elev.is_sails,
                is_mrle: elev.is_mrle,
                is_base_tilt: elev.is_base_tilt,
                vcp_azimuth_rate: elev.azimuth_rate,
                fallback_azimuth_rate: fallback_rate,
                azimuth_rate_used: rate_used,
                rate_source,
                predicted_start,
                predicted_end,
                predicted_duration,
                predicted_chunks,
                mid_predicted_start: None,
                mid_predicted_end: None,
                actual_start: None,
                actual_end: None,
                actual_chunks: None,
                observed_rate_dps: None,
                timing_source: None,
                status: crate::state::SweepStatus::Future,
            });

            cum_offset += weighted_dur;
        }

        let inter_volume_gap_secs = self
            .previous_volume_end_secs
            .map(|prev_end| vol_start - prev_end);

        // Predicted gap: the iterator's predicted_available_at for the Start
        // chunk of this volume, minus the previous volume's observed end.
        // That's the forecaster's "when will the next volume begin arriving"
        // estimate, which drives whether we start polling for it too early
        // (wasted 404s) or too late (wasted wait).
        let predicted_inter_volume_gap_secs = match self.previous_volume_end_secs {
            Some(prev_end) => self
                .chunk_arrivals
                .first()
                .filter(|a| a.chunk_type == "Start")
                .and_then(|a| a.predicted_available_at)
                .map(|pred| pred - prev_end),
            None => None,
        };

        let snap = crate::state::VolumeForecastSnapshot {
            vcp_number,
            vcp_name,
            is_clear_air,
            volume_start: vol_start,
            predicted_volume_end,
            actual_volume_end: None,
            expected_elevation_count: vcp.elevations.len() as u8,
            sweeps,
            chunk_projections_available_at_start: chunk_projections_available,
            previous_volume_end: self.previous_volume_end_secs,
            inter_volume_gap_secs,
            predicted_inter_volume_gap_secs,
        };
        self.current_volume_forecast = Some(snap);

        // Pre-fill actuals and mid-predictions in case some data arrived before
        // the snapshot could be taken (e.g., VCP message came in after the
        // first sweep of the volume had already been ingested).
        self.fill_forecast_actuals();
        if let Some(cur_elev) = self.current_in_progress_elevation {
            self.capture_mid_prediction(cur_elev);
        }
    }

    /// Walk `completed_sweep_metas` + `chunk_elev_spans` and fill actuals on
    /// the current volume's forecast snapshot, if any.
    fn fill_forecast_actuals(&mut self) {
        let Some(snap) = self.current_volume_forecast.as_mut() else {
            return;
        };
        for meta in &self.completed_sweep_metas {
            let Some(forecast) = snap
                .sweeps
                .iter_mut()
                .find(|s| s.elev_number == meta.elevation_number)
            else {
                continue;
            };
            forecast.actual_start = Some(meta.start);
            forecast.actual_end = Some(meta.end);
            let dur = meta.end - meta.start;
            forecast.observed_rate_dps = if dur > 0.0 { Some(360.0 / dur) } else { None };
            forecast.actual_chunks = Some(
                self.chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == meta.elevation_number)
                    .count() as u32,
            );
            forecast.timing_source = Some(crate::state::SweepTiming::Observed);
            forecast.status = crate::state::SweepStatus::Complete;
        }
    }

    /// Record the "mid" prediction — what the projection library predicts for
    /// the given elevation's bounds *now*, when it has just become in-progress.
    fn capture_mid_prediction(&mut self, elev: u8) {
        let Some(snap) = self.current_volume_forecast.as_mut() else {
            return;
        };
        let Some(forecast) = snap.sweeps.iter_mut().find(|s| s.elev_number == elev) else {
            return;
        };
        if forecast.mid_predicted_start.is_some() {
            return;
        }

        if let Some(ref projs) = self.chunk_projections {
            let mut min_t = f64::MAX;
            let mut max_t = f64::MIN;
            for chunk in projs {
                if chunk.elevation_number == Some(elev as usize) {
                    if let Some(t) = chunk.projected_time_secs {
                        min_t = min_t.min(t);
                        max_t = max_t.max(t);
                    }
                }
            }
            if min_t < f64::MAX {
                forecast.mid_predicted_start = Some(min_t);
                if max_t > f64::MIN {
                    forecast.mid_predicted_end = Some(max_t);
                }
            }
        }
    }
}
