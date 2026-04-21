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
    /// Whether `chunk_projections` were present at snapshot time — useful
    /// context when reading the `predicted_chunks` column.
    pub chunk_projections_available_at_start: bool,

    /// Prior volume's observed end timestamp (Unix seconds), if we saw one.
    pub previous_volume_end: Option<f64>,
    /// `volume_start - previous_volume_end` when both are known.
    pub inter_volume_gap_secs: Option<f64>,
    /// Forecaster's predicted gap: `predicted_available_at` on the new
    /// volume's Start chunk minus the previous volume's observed end,
    /// when both are known.
    pub predicted_inter_volume_gap_secs: Option<f64>,
}

/// Per-chunk arrival diagnostic sample. Captured by the real-time streaming
/// loop on every successful chunk fetch and retained for the current volume.
///
/// The purpose is to answer:
/// * How many empty polls did each chunk take? (wasted S3 requests)
/// * How accurate was `time_until_next()` compared to actual arrival?
/// * For chunks with empty polls, when could the fetch have succeeded
///   earliest? (we know it wasn't there at `last_empty_poll_at` and it was
///   there at `success_at`, so the earliest usable download time lies
///   somewhere in between)
#[derive(Clone, Debug)]
pub struct ChunkArrivalStat {
    /// 1-based sequence number within the volume at the time of success.
    pub sequence: u32,
    /// "Start" / "Intermediate" / "End".
    pub chunk_type: &'static str,
    /// 1-based elevation number the chunk contributes to. `None` for the
    /// volume-start chunk (which carries VCP metadata, not a specific sweep).
    pub elevation_number: Option<u8>,
    /// 0-based index of this chunk within its sweep (e.g. 0, 1, 2 for a
    /// standard sweep; 0–5 for super-res).
    pub chunk_index_in_sweep: Option<u32>,
    /// Total chunks expected in this sweep (3 for standard, 6 for super-res).
    pub chunks_in_sweep: Option<u32>,
    /// What the iterator's `time_until_next()` said the chunk would be
    /// available at (Unix seconds). `None` if the iterator had no prediction.
    pub predicted_available_at: Option<f64>,
    /// Time the first poll for this chunk was issued (end of the predicted
    /// sleep), Unix seconds. The gap `scheduled_at - predicted_available_at`
    /// is the scheduler slop (how precisely we woke on the predicted time).
    pub scheduled_at: f64,
    /// Number of empty `Ok(None)` polls before the successful fetch.
    pub empty_polls: u32,
    /// Time of the first empty poll for this chunk (Unix seconds). `None`
    /// when `empty_polls == 0`. Together with `last_empty_poll_at` bounds
    /// the retry cluster.
    pub first_empty_poll_at: Option<f64>,
    /// Time of the most recent empty poll (Unix seconds). `None` when
    /// `empty_polls == 0`. Crude lower bound on when the chunk actually
    /// became available on S3 — we only learn it via polling.
    pub last_empty_poll_at: Option<f64>,
    /// S3's `Last-Modified` header for the object (Unix seconds). When
    /// present this is the authoritative earliest-possible-download time
    /// and strictly tighter than `last_empty_poll_at`. Note: header has
    /// 1-second resolution, so derived `wait_after_s3_publish_ms` values
    /// are ±1s noisy.
    pub s3_last_modified_at: Option<f64>,
    /// Time the successful poll received its response (Unix seconds).
    pub success_at: f64,
    /// HTTP round-trip time for the successful fetch, milliseconds.
    pub fetch_latency_ms: f64,
}

impl ChunkArrivalStat {
    /// Positive values mean the forecaster was too optimistic (we polled
    /// before the chunk was actually available). Negative values mean we
    /// waited longer than necessary.
    pub fn prediction_error_secs(&self) -> Option<f64> {
        self.predicted_available_at.map(|p| self.success_at - p)
    }

    /// Milliseconds between when we intended to wake and when we actually
    /// polled — the scheduler's precision relative to the prediction.
    pub fn scheduler_slop_ms(&self) -> Option<f64> {
        self.predicted_available_at
            .map(|p| (self.scheduled_at - p) * 1000.0)
    }

    /// Time between the last empty poll and the successful download.
    /// Represents wait that could potentially have been avoided if the
    /// poll schedule were better aligned to S3 publishing time.
    pub fn wait_after_last_empty_ms(&self) -> Option<f64> {
        self.last_empty_poll_at
            .map(|t| (self.success_at - t) * 1000.0)
    }

    /// Time between S3's `Last-Modified` and our successful download.
    /// Unlike `wait_after_last_empty_ms`, this tries to be authoritative
    /// (the object was provably available at `s3_last_modified_at`), but
    /// `Last-Modified` has 1-second resolution so individual values carry
    /// ±1 s of quantization noise — including sign flips. Magnitudes
    /// smaller than 1000 ms are indistinguishable from zero; treat only
    /// `|wait| > 1000 ms` as real client-side wait (or real clock skew).
    pub fn wait_after_s3_publish_ms(&self) -> Option<f64> {
        self.s3_last_modified_at
            .map(|t| (self.success_at - t) * 1000.0)
    }
}
