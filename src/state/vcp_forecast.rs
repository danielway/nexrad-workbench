//! Diagnostic snapshot of VCP-based sweep forecasts vs. observed reality.
//!
//! A `VolumeForecastSnapshot` is captured at the start of a live volume — the
//! moment both the VCP pattern (Message Type 5) and the volume-start timestamp
//! are known — and then mutated as sweeps complete. The stored shape is a
//! predicted/actual side-by-side for every elevation, designed to be
//! serialized to plain text and pasted into a chat message so the forecasting
//! algorithms can be iterated on from real session data.

use super::{SweepStatus, SweepTiming};

/// Where the azimuth-rate value driving the prediction came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateSource {
    /// Rate came straight from the VCP message (`ExtractedVcpElevation.azimuth_rate`).
    VcpMessage,
    /// VCP message had no rate — used `fallback_azimuth_rate(...)`.
    MethodBFallback,
    /// Used `ChunkProjectionInfo.azimuth_rate_dps` from the nexrad-data projection library.
    ProjectionLibrary,
}

impl RateSource {
    pub fn short(&self) -> &'static str {
        match self {
            RateSource::VcpMessage => "VCP",
            RateSource::MethodBFallback => "FB",
            RateSource::ProjectionLibrary => "LIB",
        }
    }
}

/// Per-elevation predicted values captured at volume start, with slots for
/// actuals filled in as the sweep completes.
#[derive(Clone, Debug)]
pub struct SweepForecast {
    pub elev_number: u8,
    pub elev_angle: f32,
    pub waveform: String,
    pub prf_number: u8,
    pub is_sails: bool,
    pub is_mrle: bool,
    pub is_base_tilt: bool,

    /// Raw rate from the VCP message, if present.
    pub vcp_azimuth_rate: Option<f32>,
    /// Fallback rate computed from (waveform, prf, clear_air).
    pub fallback_azimuth_rate: f64,
    /// Rate actually used for the prediction (from VCP, fallback, or library).
    pub azimuth_rate_used: f64,
    pub rate_source: RateSource,

    pub predicted_start: f64,
    #[allow(dead_code)] // stored for future use; rendered via predicted_duration today
    pub predicted_end: f64,
    pub predicted_duration: f64,
    /// `None` when no `ChunkProjectionInfo` was available at snapshot time
    /// (we didn't guess a chunk count then).
    pub predicted_chunks: Option<u32>,

    /// Re-projected start when this elevation first becomes in-progress —
    /// what the forecaster would predict once a chunk for it has arrived.
    pub mid_predicted_start: Option<f64>,
    pub mid_predicted_end: Option<f64>,

    pub actual_start: Option<f64>,
    pub actual_end: Option<f64>,
    pub actual_chunks: Option<u32>,
    /// `360 / actual_duration` for completed sweeps — comparable to `azimuth_rate_used`.
    pub observed_rate_dps: Option<f64>,

    pub timing_source: Option<SweepTiming>,
    pub status: SweepStatus,
}

impl SweepForecast {
    pub fn actual_duration(&self) -> Option<f64> {
        match (self.actual_start, self.actual_end) {
            (Some(s), Some(e)) if e > s => Some(e - s),
            _ => None,
        }
    }
}

/// Volume-level snapshot. Serialized into the clipboard text.
#[derive(Clone, Debug)]
pub struct VolumeForecastSnapshot {
    pub vcp_number: u16,
    /// Name from the static `get_vcp_definition` table; `None` for unknown VCPs.
    pub vcp_name: Option<&'static str>,
    pub is_clear_air: bool,
    pub volume_start: f64,
    /// Predicted volume-end — the library projection if available, otherwise
    /// `volume_start + ExtractedVcp::estimated_volume_duration()`.
    pub predicted_volume_end: f64,
    pub actual_volume_end: Option<f64>,
    pub expected_elevation_count: u8,
    pub sweeps: Vec<SweepForecast>,
    /// When the snapshot was taken (Unix seconds).
    pub captured_at: f64,
    /// Whether `chunk_projections` were present at snapshot time — useful
    /// context when reading the `predicted_chunks` column.
    pub chunk_projections_available_at_start: bool,

    /// Prior volume's observed end timestamp (Unix seconds), if we saw one.
    pub previous_volume_end: Option<f64>,
    /// `volume_start - previous_volume_end` when both are known.
    pub inter_volume_gap_secs: Option<f64>,
    /// Reserved for when the forecaster predicts the gap; always `None` today.
    pub predicted_inter_volume_gap_secs: Option<f64>,
}
