//! Diagnostic snapshot of VCP-based sweep forecasts vs. observed reality.
//!
//! A `VolumeForecastSnapshot` is captured at the start of a live volume — the
//! moment both the VCP pattern (Message Type 5) and the volume-start timestamp
//! are known — and then mutated as sweeps complete. The stored shape is a
//! predicted/actual side-by-side for every elevation, designed to be
//! serialized to plain text and pasted into a chat message so the forecasting
//! algorithms can be iterated on from real session data.

/// How a forecast sweep's time bounds were derived (diagnostic label).
/// Display vocabulary: the modal renders all variants, but the derive path
/// only produces `Observed` today — `Anchored`/`Estimated` await the
/// accuracy-tuning work that will label partially-derived bounds.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)] // Unproduced variants are display vocabulary (see above).
pub enum ForecastTimingLabel {
    Observed,
    Anchored,
    Estimated,
}

/// Completion status of a forecast sweep (diagnostic label). The modal
/// renders all variants; the derive path currently produces only
/// `Complete`/`Future` (`InProgress` awaits a mid-volume derive pass).
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)] // `InProgress` is display vocabulary (see above).
pub enum SweepStatus {
    Complete,
    InProgress {
        radials_received: u32,
        chunks_received: u32,
        chunks_expected: Option<u32>,
    },
    Future,
}
use crate::nexrad::timing::{AnchorSource, ChunkCharacteristics, PhysicsBreakdown, SchedulerPath};
use nexrad_data::aws::realtime::ChunkType;
use nexrad_decode::messages::volume_coverage_pattern::{ChannelConfiguration, WaveformType};

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
/// actuals filled in as the sweep completes. Carries exactly what the
/// diagnostics modal and clipboard serializer render — structural VCP
/// detail (PRF, SAILS/MRLE flags, raw rates) lives on `ExtractedVcp` and
/// is re-derivable from the [`CompletedVolumeRecord`]'s `vcp` when a
/// future column needs it.
#[derive(Clone, Debug)]
pub struct SweepForecast {
    pub elev_number: u8,
    pub elev_angle: f32,
    pub waveform: String,

    /// Rate actually used for the prediction (from VCP, fallback, or library).
    pub azimuth_rate_used: f64,
    pub rate_source: RateSource,

    pub predicted_start: f64,
    pub predicted_duration: f64,
    /// `None` when no `ChunkProjectionInfo` was available at snapshot time
    /// (we didn't guess a chunk count then).
    pub predicted_chunks: Option<u32>,

    pub actual_start: Option<f64>,
    pub actual_end: Option<f64>,
    pub actual_chunks: Option<u32>,

    pub timing_source: Option<ForecastTimingLabel>,
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
    /// `volume_start - previous_volume_end` when both are known.
    pub inter_volume_gap_secs: Option<f64>,
    /// Forecaster's predicted gap: `predicted_available_at` on the new
    /// volume's Start chunk minus the previous volume's observed end,
    /// when both are known.
    pub predicted_inter_volume_gap_secs: Option<f64>,
}

/// Frozen inputs from a completed volume, retained for the diagnostics
/// modal so it can render predicted-vs-actual data after the live state
/// has rolled over to the next volume.
///
/// Stores raw inputs (the volume-start plan, completed sweep metas, chunk
/// arrivals) rather than a pre-computed [`VolumeForecastSnapshot`]. The
/// snapshot is derived on demand by
/// [`derive_volume_forecast`] when the modal opens. Same shape feeds
/// derivation for the in-progress volume — see
/// [`crate::state::LiveModeState::derive_current_volume_forecast`].
#[derive(Clone, Debug)]
pub struct CompletedVolumeRecord {
    pub vcp: crate::data::keys::ExtractedVcp,
    /// Plan as captured at the start of this volume — preserves the
    /// library-projected predicted times so the diagnostics modal can
    /// reproduce them after sweeps complete (their per-chunk forecast
    /// becomes `None` once they're past).
    pub volume_start_plan: crate::nexrad::StreamingPlan,
    pub volume_start_secs: f64,
    pub volume_end_secs: f64,
    pub previous_volume_end_secs: Option<f64>,
    pub completed_sweep_metas: Vec<crate::data::CachedSweep>,
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    pub chunk_arrivals: Vec<ChunkArrivalStat>,
}

/// Build a [`VolumeForecastSnapshot`] from frozen volume inputs. Used by
/// the diagnostics modal both for the in-progress volume (called with the
/// captured `volume_start_plan` and live state references) and for the
/// last completed volume (called with a [`CompletedVolumeRecord`]'s
/// fields).
///
/// Predicted values come from `volume_start_plan` (the plan as it was
/// when the volume began) — its `current_volume_chunks` still have
/// per-chunk forecasts for every sweep, so even after a sweep completes
/// the predicted column reflects the library projection rather than a
/// VCP cum-offset fallback.
#[allow(clippy::too_many_arguments)]
pub fn derive_volume_forecast(
    vcp: &crate::data::keys::ExtractedVcp,
    volume_start_plan: &crate::nexrad::StreamingPlan,
    volume_start_secs: f64,
    completed_sweep_metas: &[crate::data::CachedSweep],
    chunk_elev_spans: &[(u8, f64, f64, u32)],
    previous_volume_end_secs: Option<f64>,
    chunk_arrivals: &[ChunkArrivalStat],
    actual_volume_end: Option<f64>,
) -> VolumeForecastSnapshot {
    use std::collections::BTreeMap;

    let vcp_number = vcp.number;
    let vcp_name = crate::state::get_vcp_definition(vcp_number).map(|d| d.name);
    let is_clear_air = crate::data::vcp::is_clear_air_vcp(vcp_number);

    let total_vol_dur = vcp.estimated_volume_duration().unwrap_or(300.0);
    let sweep_durations = vcp.sweep_durations(total_vol_dur);

    // Predicted end of volume = COLLECTION time of the final chunk if the
    // captured plan knows it, else volume_start + estimated duration.
    let predicted_volume_end = volume_start_plan
        .current_volume_end_collection_secs
        .unwrap_or(volume_start_secs + total_vol_dur);

    // Group volume-start chunks by elevation: (min_t, max_t, count, rate).
    let projected_per_elev: BTreeMap<u8, (f64, f64, u32, f64)> = {
        let mut map: BTreeMap<u8, (f64, f64, u32, f64)> = BTreeMap::new();
        for chunk in &volume_start_plan.current_volume_chunks {
            if let Some(e) = chunk.elevation_number {
                let entry = map.entry(e as u8).or_insert((
                    f64::MAX,
                    f64::MIN,
                    0u32,
                    chunk.azimuth_rate_dps,
                ));
                entry.2 += 1;
                if let Some(t) = chunk.projected.as_ref().map(|f| f.collection_time_secs) {
                    entry.0 = entry.0.min(t);
                    entry.1 = entry.1.max(t);
                }
                if entry.3 <= 0.0 {
                    entry.3 = chunk.azimuth_rate_dps;
                }
            }
        }
        map
    };

    let mut sweeps: Vec<SweepForecast> = Vec::with_capacity(vcp.elevations.len());
    let mut cum_offset = 0.0f64;
    for (idx, elev) in vcp.elevations.iter().enumerate() {
        let elev_number = (idx + 1) as u8;
        let weighted_dur = sweep_durations
            .get(idx)
            .copied()
            .unwrap_or(total_vol_dur / vcp.elevations.len() as f64);
        let fallback_rate =
            crate::data::vcp::fallback_azimuth_rate(is_clear_air, &elev.waveform, elev.prf_number);

        let proj = projected_per_elev.get(&elev_number).copied();
        let (rate_used, rate_source) = match (proj, elev.azimuth_rate) {
            (Some((_, _, _, r)), _) if r > 0.0 => (r, RateSource::ProjectionLibrary),
            (_, Some(r)) if r > 0.0 => (r as f64, RateSource::VcpMessage),
            _ => (fallback_rate, RateSource::MethodBFallback),
        };

        // Library projection bounds when usable; otherwise cumulative VCP
        // offset. The last chunk publishes at the start of its bucket;
        // the sweep runs for one more bucket after that — add
        // `sweep_dur / N` to max_t.
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
                volume_start_secs + cum_offset,
                volume_start_secs + cum_offset + weighted_dur,
            ),
        };
        let predicted_duration = (predicted_end - predicted_start).max(0.0);
        let predicted_chunks = proj.map(|(_, _, count, _)| count);

        // Actuals from observed sweep metas.
        let actual = completed_sweep_metas
            .iter()
            .find(|m| m.elevation_number == elev_number);
        let (actual_start, actual_end, timing_source, status) = match actual {
            Some(meta) => (
                Some(meta.start),
                Some(meta.end),
                Some(ForecastTimingLabel::Observed),
                SweepStatus::Complete,
            ),
            None => (None, None, None, SweepStatus::Future),
        };
        let actual_chunks = actual.map(|_| {
            chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == elev_number)
                .count() as u32
        });

        sweeps.push(SweepForecast {
            elev_number,
            elev_angle: elev.angle,
            waveform: elev.waveform.clone(),
            azimuth_rate_used: rate_used,
            rate_source,
            predicted_start,
            predicted_duration,
            predicted_chunks,
            actual_start,
            actual_end,
            actual_chunks,
            timing_source,
            status,
        });

        cum_offset += weighted_dur;
    }

    let inter_volume_gap_secs = previous_volume_end_secs.map(|prev| volume_start_secs - prev);
    let predicted_inter_volume_gap_secs = previous_volume_end_secs.and_then(|prev| {
        chunk_arrivals
            .first()
            .filter(|a| a.chunk_type == "Start")
            .and_then(|a| a.predicted_available_at)
            .map(|pred| pred - prev)
    });

    VolumeForecastSnapshot {
        vcp_number,
        vcp_name,
        is_clear_air,
        volume_start: volume_start_secs,
        predicted_volume_end,
        actual_volume_end,
        expected_elevation_count: vcp.elevations.len() as u8,
        sweeps,
        inter_volume_gap_secs,
        predicted_inter_volume_gap_secs,
    }
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
    /// How the wait before this chunk's fetch was resolved. Distinguishes a
    /// plain sleep-to-prediction from the adaptive cross-volume list probe
    /// firing early or re-anchoring the remaining wait. Lets the diagnostics
    /// modal attribute "overshoot capped by list probe" vs. "slept to model
    /// prediction".
    pub wait_resolution: WaitResolution,
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
    /// Number of empty `Ok(None)` polls before the successful fetch.
    pub empty_polls: u32,
    /// Time of the most recent empty poll (Unix seconds). `None` when
    /// `empty_polls == 0`. Used to compute `wait_after_last_empty_ms`,
    /// the time from the last failed poll to success.
    pub last_empty_poll_at: Option<f64>,
    /// S3's `Last-Modified` header for the object (Unix seconds). Used by
    /// `main.rs` to compute the per-chunk availability lag once the worker
    /// ingest yields the chunk's last-radial collection time.
    pub s3_last_modified_at: Option<f64>,
    /// Time the successful poll received its response (Unix seconds).
    pub success_at: f64,

    /// Bucket key used by `ChunkTimingStats` for the chunk that *was* arriving
    /// at the time the prediction was made. `None` for chunks where no
    /// metadata-driven prediction was available (Start chunk, or legacy path).
    pub bucket_key: Option<BucketKey>,
    /// Number of samples in the bucket above at the moment the prediction
    /// was made. Lets us tell "model wrong" from "stats not warm yet" when
    /// the prediction error is large.
    pub stats_n_at_prediction: usize,
    /// Which estimator branch produced `predicted_available_at`. `None` only
    /// when no prediction was made at all (e.g. resume-from-cache emissions).
    pub scheduler_path: Option<SchedulerPath>,
    /// Physics decomposition for the inter-chunk transition that fed the
    /// prediction. Populated whenever both current and next metadata existed
    /// at prediction time.
    pub physics_breakdown: Option<PhysicsBreakdown>,
    /// Which anchor branch the projector was using at prediction time.
    /// Tells us when projections were degraded by a fallback anchor.
    pub anchor_source: Option<AnchorSource>,
    /// `s3_last_modified_at − chunk_max_collection_time_secs`, in milliseconds.
    /// Computed in `main.rs` once worker ingest yields the chunk's last
    /// radial time, then back-filled onto this struct via
    /// `attach_availability_lag_ms`. `None` until that back-fill happens (or
    /// permanently if either timestamp is unavailable).
    pub availability_lag_ms: Option<i64>,
    /// ACTUAL chunk collection-end time (Unix seconds, ms-precise) — the
    /// last radial timestamp parsed by the worker for this chunk. Back-filled
    /// from `main.rs` alongside `availability_lag_ms`. `None` until the
    /// worker ingest completes for this chunk. Together with the previous
    /// arrival's `collection_time_secs`, lets us compute the actual
    /// collection-space interval per chunk free of S3 1-second quantization.
    pub collection_time_secs: Option<f64>,
    /// Wait the scheduler returned for this chunk on its first poll —
    /// `EstimatedChunkProcessing.duration` from the estimator. The interval
    /// the projector/scheduler *actually* used (already includes the 70/30
    /// historical blend when `path == Historical`). Pair with
    /// `collection_time_secs` to compute the per-chunk interval prediction
    /// error in collection space.
    pub predicted_wait_secs: Option<f64>,
    /// Revision number of the [`crate::nexrad::StreamingPlan`] that
    /// produced this chunk's prediction. Bumped monotonically by the
    /// projector on each `build_plan` call. Lets the diagnostics modal
    /// distinguish "model wrong" from "stale prediction" and trace which
    /// plan version was active when each arrival's forecast was captured.
    /// `None` when the prediction was captured from a path that didn't
    /// snapshot the plan revision (e.g. resume-from-cache emissions).
    #[allow(dead_code)] // Wired to display in the diagnostics modal in a follow-up.
    pub predicted_with_plan_revision: Option<u64>,
}

/// How the streaming loop's wait before a chunk fetch was resolved.
///
/// The adaptive cross-volume wait (filtered streaming, target in the next
/// volume) periodically lists the next volume's S3 slot to correct for
/// accumulated timing-prediction error. This records which path the loop took
/// for a given arrival so prediction-error diagnostics can separate
/// model-driven sleeps from probe-corrected ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WaitResolution {
    /// Slept to the projector's predicted poll time with no list probe — the
    /// default path (short / same-volume / unfiltered waits, or a cross-volume
    /// wait whose probes never fired).
    #[default]
    SleptToPrediction,
    /// A list probe found the target chunk already published and broke the
    /// sleep early, capping overshoot at the probe cadence.
    EarlyFired,
    /// A list probe re-anchored the remaining-wait projection on a freshly
    /// published chunk (no early-fire), then slept to the corrected target.
    ReAnchored,
}

impl WaitResolution {
    /// Compact label for the diagnostics arrivals table.
    pub fn short(&self) -> &'static str {
        match self {
            WaitResolution::SleptToPrediction => "sleep",
            WaitResolution::EarlyFired => "early",
            WaitResolution::ReAnchored => "reanchor",
        }
    }
}

/// Compact serialisable bucket key. Mirrors `ChunkCharacteristics` but
/// without depending on the timing crate's enum types — keeps the diagnostics
/// state layer free of upstream imports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BucketKey {
    pub chunk_type: &'static str,
    pub waveform: &'static str,
    pub channel: &'static str,
    pub first_in_sweep: bool,
}

impl BucketKey {
    /// Compact label for diagnostics output (e.g. `"I|CS|RP|F"`).
    pub fn short(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.chunk_type,
            self.waveform,
            self.channel,
            if self.first_in_sweep { "T" } else { "F" }
        )
    }

    pub fn from_characteristics(c: &ChunkCharacteristics) -> Self {
        Self {
            chunk_type: chunk_type_str(c.chunk_type),
            waveform: waveform_str(c.waveform_type),
            channel: channel_str(c.channel_configuration),
            first_in_sweep: c.is_first_in_sweep,
        }
    }
}

fn chunk_type_str(t: ChunkType) -> &'static str {
    match t {
        ChunkType::Start => "S",
        ChunkType::Intermediate => "I",
        ChunkType::End => "E",
    }
}

fn waveform_str(w: WaveformType) -> &'static str {
    match w {
        WaveformType::CS => "CS",
        WaveformType::CDW => "CDW",
        WaveformType::CDWO => "CDWO",
        WaveformType::B => "B",
        WaveformType::SPP => "SPP",
        WaveformType::Unknown => "?",
    }
}

fn channel_str(c: ChannelConfiguration) -> &'static str {
    match c {
        ChannelConfiguration::ConstantPhase => "CP",
        ChannelConfiguration::RandomPhase => "RP",
        ChannelConfiguration::SZ2Phase => "SZ2",
        ChannelConfiguration::UnknownPhase => "?",
    }
}

impl ChunkArrivalStat {
    /// Positive values mean the forecaster was too optimistic (we polled
    /// before the chunk was actually available). Negative values mean we
    /// waited longer than necessary.
    pub fn prediction_error_secs(&self) -> Option<f64> {
        self.predicted_available_at.map(|p| self.success_at - p)
    }

    /// Time between the last empty poll and the successful download.
    /// Represents wait that could potentially have been avoided if the
    /// poll schedule were better aligned to S3 publishing time.
    pub fn wait_after_last_empty_ms(&self) -> Option<f64> {
        self.last_empty_poll_at
            .map(|t| (self.success_at - t) * 1000.0)
    }

    /// Actual collection-space interval between this chunk and `prev` — the
    /// ground truth the predicted wait should be compared against. Returns
    /// `None` if either chunk hasn't had its collection time back-filled.
    pub fn actual_interval_secs(&self, prev: &ChunkArrivalStat) -> Option<f64> {
        match (self.collection_time_secs, prev.collection_time_secs) {
            (Some(c), Some(p)) if c > p => Some(c - p),
            _ => None,
        }
    }

    /// Per-chunk interval prediction error in collection space, milliseconds:
    /// `actual_interval − predicted_wait`. Positive means we underestimated
    /// the gap (the chunk took longer to arrive than predicted), which is
    /// the dominant signal for "sweep too short" / "sweeps shifted earlier"
    /// symptoms.
    pub fn interval_error_ms(&self, prev: &ChunkArrivalStat) -> Option<f64> {
        let actual = self.actual_interval_secs(prev)?;
        let predicted = self.predicted_wait_secs?;
        Some((actual - predicted) * 1000.0)
    }
}
