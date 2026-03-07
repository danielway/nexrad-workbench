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
    #[allow(dead_code)] // Used when networking is implemented
    Error,
}

impl LivePhase {
    /// Human-readable label for the phase.
    #[allow(dead_code)] // Used when status bar shows phase name
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
    #[allow(dead_code)] // Alternative to ui::colors module
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
    /// User pressed pause.
    #[allow(dead_code)] // Used when pause behavior differs from stop
    UserPaused,
    /// User clicked on timeline or used seek controls.
    UserSeeked,
    /// User used jog forward/backward buttons.
    UserJogged,
    /// Network or connection error.
    #[allow(dead_code)] // Used when networking is implemented
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
    /// actual radial collection timestamps. A single chunk that spans two
    /// elevations produces two entries.
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,

    /// Actual sweep metadata (with real timestamps) for completed elevations
    /// in the current volume. Used for accurate sweep positioning on the timeline
    /// instead of even-distribution estimates.
    pub completed_sweep_metas: Vec<crate::data::SweepMeta>,

    /// Last known radial azimuth in degrees (0-360) from the most recent chunk.
    /// Used to extrapolate sweep line position between chunks.
    pub last_radial_azimuth: Option<f32>,

    /// Timestamp (Unix seconds) of the last known radial. Together with
    /// `last_radial_azimuth`, allows linear extrapolation of sweep line.
    pub last_radial_time_secs: Option<f64>,
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
            last_volume_duration_secs: None,
            current_volume_start: None,
            current_scan_key: None,
            current_in_progress_elevation: None,
            current_in_progress_radials: None,
            chunk_elev_spans: Vec::new(),
            completed_sweep_metas: Vec::new(),
            last_radial_azimuth: None,
            last_radial_time_secs: None,
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
        self.last_radial_azimuth = None;
        self.last_radial_time_secs = None;
    }

    /// Set error state with message.
    #[allow(dead_code)] // Used when networking is implemented
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
    #[allow(dead_code)] // Used by realtime streaming integration
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
    #[allow(dead_code)] // Alternative to inline formatting in UI
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
    ) {
        self.chunks_received = chunks_in_volume;

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
        self.phase = LivePhase::Streaming;
        self.phase_started_at = Some(now);
        self.elevations_received.clear();
        self.current_volume_start = None;
        self.current_scan_key = None;
        self.current_in_progress_elevation = None;
        self.current_in_progress_radials = None;
        self.chunk_elev_spans.clear();
        self.completed_sweep_metas.clear();
        self.last_radial_azimuth = None;
        self.last_radial_time_secs = None;
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
    }

    /// Record which elevation is currently being accumulated (partial sweep).
    pub fn record_in_progress_elevation(&mut self, elevation: Option<u8>, radials: Option<u32>) {
        self.current_in_progress_elevation = elevation;
        self.current_in_progress_radials = radials;
    }

    /// Record VCP info from an ingest result.
    pub fn record_vcp(&mut self, vcp_number: u16, elevation_count: u8) {
        self.current_vcp_number = Some(vcp_number);
        self.expected_elevation_count = Some(elevation_count);
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

    /// Estimate the current sweep line azimuth by extrapolating from the last
    /// known radial position. Uses the estimated sweep duration (volume duration
    /// / elevation count) to compute rotation rate.
    ///
    /// Returns `None` if insufficient data is available.
    pub fn estimated_azimuth(&self, now_secs: f64) -> Option<f32> {
        let last_az = self.last_radial_azimuth?;
        let last_t = self.last_radial_time_secs?;
        let vol_dur = self.last_volume_duration_secs?;
        let elev_count = self.expected_elevation_count? as f64;
        if elev_count <= 0.0 || vol_dur <= 0.0 {
            return None;
        }

        // Approximate sweep duration and rotation rate
        let sweep_dur = vol_dur / elev_count;
        if sweep_dur <= 0.0 {
            return None;
        }
        let degrees_per_sec = 360.0 / sweep_dur;

        let dt = now_secs - last_t;
        // Don't extrapolate more than one sweep duration ahead
        if dt < 0.0 || dt > sweep_dur {
            return None;
        }

        let estimated = last_az as f64 + dt * degrees_per_sec;
        Some(((estimated % 360.0 + 360.0) % 360.0) as f32)
    }

    /// Estimate which elevation index (0-based) the radar is currently scanning,
    /// based on volume progress. Returns `None` if insufficient data.
    pub fn estimated_elevation_index(&self, now_secs: f64) -> Option<usize> {
        let vol_start = self.current_volume_start?;
        let vol_dur = self.last_volume_duration_secs?;
        let elev_count = self.expected_elevation_count? as usize;
        if vol_dur <= 0.0 || elev_count == 0 {
            return None;
        }

        let elapsed = now_secs - vol_start;
        if elapsed < 0.0 {
            return Some(0);
        }

        let progress = elapsed / vol_dur;
        let idx = (progress * elev_count as f64).floor() as usize;
        Some(idx.min(elev_count - 1))
    }
}
