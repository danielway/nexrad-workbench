//! Consolidated tuning constants for chunk-timing estimation.
//!
//! Every knob the estimation pipeline consults lives here so their
//! relationships are visible in one place. Which time axis each knob
//! biases is noted per field — see `docs/TIMING.md` for the axis
//! definitions (COLLECTION vs AVAILABILITY vs poll).

/// Tuning knobs for interval estimation and projection.
///
/// The [`crate::nexrad::Projector`] owns an instance (default values in
/// production); tests construct non-default tunings to probe the blend.
/// `max_timing_samples` and `default_volume_duration_secs` are read as
/// global constants (`TimingTuning::DEFAULT`) by types that have no
/// projector at hand (`ChunkTimingStats`, `VolumeObservations`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimingTuning {
    /// Weight of historical samples in the physics/historical blend:
    /// `seconds = (1 − w) * physics + w * historical`. Biases the
    /// COLLECTION axis (and everything downstream of it).
    pub hist_weight: f64,
    /// Bias between expected availability and the scheduler's first poll,
    /// so the first attempt lands slightly after the upload rather than
    /// racing it. Biases only the poll axis.
    pub poll_bias_secs: f64,
    /// Fallback NEXRAD ingest lag used when no lag sample has been
    /// observed yet — roughly matches typical S3 upload latencies
    /// (~5-15 s). Biases the AVAILABILITY axis until real samples land.
    pub default_availability_lag_secs: f64,
    /// Rolling-window size per characteristics bucket in
    /// `ChunkTimingStats`. Larger windows smooth more but adapt slower
    /// to radar-mode changes.
    pub max_timing_samples: usize,
    /// Expected volume duration (seconds) when neither a completed volume
    /// nor a VCP estimate is available (pre-VCP cold start).
    pub default_volume_duration_secs: f64,
}

impl TimingTuning {
    pub(crate) const DEFAULT: TimingTuning = TimingTuning {
        hist_weight: 0.3,
        poll_bias_secs: 0.750,
        default_availability_lag_secs: 5.0,
        max_timing_samples: 10,
        default_volume_duration_secs: 300.0,
    };
}

impl Default for TimingTuning {
    fn default() -> Self {
        Self::DEFAULT
    }
}
