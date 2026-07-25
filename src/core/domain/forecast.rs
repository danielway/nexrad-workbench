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
pub(crate) enum ForecastTimingLabel {
    Observed,
    #[allow(dead_code)] // Doc above: display vocabulary awaiting the accuracy-tuning derive.
    Anchored,
    #[allow(dead_code)] // Doc above: display vocabulary awaiting the accuracy-tuning derive.
    Estimated,
}

/// Completion status of a forecast sweep (diagnostic label). The modal
/// renders all variants; the derive path currently produces only
/// `Complete`/`Future` (`InProgress` awaits a mid-volume derive pass).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SweepStatus {
    Complete,
    #[allow(dead_code)] // Doc above: display vocabulary awaiting a mid-volume derive pass.
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
pub(crate) enum RateSource {
    /// Rate came straight from the VCP message (`ExtractedVcpElevation.azimuth_rate`).
    VcpMessage,
    /// VCP message had no rate — used `fallback_azimuth_rate(...)`.
    MethodBFallback,
    /// Used `ChunkProjectionInfo.azimuth_rate_dps` from the nexrad-data projection library.
    ProjectionLibrary,
}

impl RateSource {
    pub(crate) fn short(&self) -> &'static str {
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
pub(crate) struct SweepForecast {
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
    pub(crate) fn actual_duration(&self) -> Option<f64> {
        match (self.actual_start, self.actual_end) {
            (Some(s), Some(e)) if e > s => Some(e - s),
            _ => None,
        }
    }
}

/// Volume-level snapshot. Serialized into the clipboard text.
#[derive(Clone, Debug)]
pub(crate) struct VolumeForecastSnapshot {
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
/// [`crate::core::LiveModeState::derive_current_volume_forecast`].
#[derive(Clone, Debug)]
pub(crate) struct CompletedVolumeRecord {
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
pub(crate) fn derive_volume_forecast(
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
    let vcp_name = crate::data::vcp::get_vcp_definition(vcp_number).map(|d| d.name);
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
pub(crate) struct ChunkArrivalStat {
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
    #[allow(dead_code)] // Doc above: diagnostics-modal display wiring is the planned consumer.
    pub predicted_with_plan_revision: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};
    use crate::data::CachedSweep;
    use crate::nexrad::{ChunkProjectedTimes, ChunkProjectionInfo, StreamingPlan};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// One elevation cut. `rate` is the VCP-message azimuth rate; `waveform`
    /// drives the Method-B fallback when no projection/VCP rate is available.
    fn elev(angle: f32, waveform: &str, rate: Option<f32>) -> ExtractedVcpElevation {
        ExtractedVcpElevation {
            angle,
            waveform: waveform.to_string(),
            prf_number: 1,
            is_sails: false,
            is_mrle: false,
            is_base_tilt: false,
            azimuth_rate: rate,
        }
    }

    /// A projection chunk mapped to `elevation_number` (1-based) with library
    /// `rate` and, when `collection` is `Some`, a `projected` collection time.
    /// Chunks without a `projected` time exercise the `min_t == f64::MAX`
    /// cum-offset fallback even though they still carry a rate/count.
    fn chunk(
        sequence: usize,
        elevation_number: u8,
        rate: f64,
        collection: Option<f64>,
    ) -> ChunkProjectionInfo {
        ChunkProjectionInfo {
            sequence,
            elevation_number: Some(elevation_number as usize),
            azimuth_rate_dps: rate,
            chunk_index_in_sweep: 0,
            chunks_in_sweep: 3,
            projected: collection.map(|c| ChunkProjectedTimes {
                collection_time_secs: c,
                available_at_secs: c,
                poll_at_secs: c,
                physics_breakdown: test_support::physics_stub(),
                stats_n: 0,
                scheduler_path: crate::nexrad::timing::SchedulerPath::Physics,
                bucket: None,
            }),
        }
    }

    fn cached(elev_number: u8, start: f64, end: f64) -> CachedSweep {
        CachedSweep {
            start,
            end,
            elevation: 0.5,
            elevation_number: elev_number,
            start_azimuth: 0.0,
            cached_products: Vec::new(),
        }
    }

    /// Reconstruct a sweep's predicted end from the stored start + duration —
    /// `SweepForecast` keeps duration, not end.
    fn predicted_end(s: &SweepForecast) -> f64 {
        s.predicted_start + s.predicted_duration
    }

    /// Projection-library branch: a chunk with a positive library rate and a
    /// `projected` collection time selects `RateSource::ProjectionLibrary` and
    /// derives `predicted_end = max_t + sweep_dur/chunk_count`, where
    /// `sweep_dur = (360/rate - 0.67).max(0)`. Hand-derived against a single
    /// one-chunk elevation so `min_t == max_t`.
    #[wasm_bindgen_test]
    fn rate_source_projection_library_and_last_chunk_bucket() {
        let rate = 20.0;
        let collection = 1000.0;
        let vcp = ExtractedVcp {
            number: 212, // precip
            elevations: vec![elev(0.5, "CS", Some(15.0))],
        };
        // One projection chunk for elev 1 at t=1000, library rate 20 dps.
        let plan =
            StreamingPlan::for_test(vec![chunk(1, 1, rate, Some(collection))], Some(collection));
        let snap = derive_volume_forecast(&vcp, &plan, 500.0, &[], &[], None, &[], None);

        assert_eq!(snap.sweeps.len(), 1);
        let s = &snap.sweeps[0];
        // Library rate (20) wins over the VCP-message rate (15).
        assert_eq!(s.rate_source, RateSource::ProjectionLibrary);
        assert!((s.azimuth_rate_used - rate).abs() < 1e-9);
        // min_t == max_t == 1000; sweep_dur = 360/20 - 0.67 = 17.33; one chunk
        // → bucket = 17.33; predicted_end = 1000 + 17.33.
        let sweep_dur = 360.0 / rate - 0.67;
        assert!((s.predicted_start - collection).abs() < 1e-9);
        assert!(
            (predicted_end(s) - (collection + sweep_dur)).abs() < 1e-9,
            "end {}",
            predicted_end(s)
        );
        assert!((s.predicted_duration - sweep_dur).abs() < 1e-9);
        assert_eq!(s.predicted_chunks, Some(1));
    }

    /// Two projection chunks for one elevation: `min_t`/`max_t` span both, the
    /// bucket extension divides by the chunk count (2), and the rate seeds from
    /// the first chunk.
    #[wasm_bindgen_test]
    fn projection_two_chunk_bucket_division() {
        let rate = 18.0;
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(0.5, "CS", None)],
        };
        let plan = StreamingPlan::for_test(
            vec![
                chunk(1, 1, rate, Some(2000.0)),
                chunk(2, 1, rate, Some(2003.0)),
            ],
            Some(2003.0),
        );
        let snap = derive_volume_forecast(&vcp, &plan, 1500.0, &[], &[], None, &[], None);
        let s = &snap.sweeps[0];
        assert_eq!(s.rate_source, RateSource::ProjectionLibrary);
        // min_t=2000, max_t=2003, chunk_count=2.
        // sweep_dur = 360/18 - 0.67 = 19.33; bucket = 19.33/2 = 9.665.
        let bucket = (360.0 / rate - 0.67) / 2.0;
        assert!((s.predicted_start - 2000.0).abs() < 1e-9);
        assert!((predicted_end(s) - (2003.0 + bucket)).abs() < 1e-9);
        assert_eq!(s.predicted_chunks, Some(2));
    }

    /// VCP-message branch: no projection chunk maps to the elevation, but the
    /// VCP message carries a positive `azimuth_rate`, so the rate source is
    /// `VcpMessage` and the bounds fall back to the cumulative VCP offset.
    #[wasm_bindgen_test]
    fn rate_source_vcp_message_with_cum_offset_fallback() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(0.5, "CS", Some(25.0))],
        };
        // No current-volume chunks → no projection → cum-offset path.
        let plan = StreamingPlan::for_test(vec![], None);
        let volume_start = 700.0;
        let snap = derive_volume_forecast(&vcp, &plan, volume_start, &[], &[], None, &[], None);
        let s = &snap.sweeps[0];
        assert_eq!(s.rate_source, RateSource::VcpMessage);
        assert!((s.azimuth_rate_used - 25.0).abs() < 1e-9);
        // Single elevation → weighted_dur == total_vol_dur; cum_offset starts 0.
        let total = vcp.estimated_volume_duration().unwrap();
        assert!((s.predicted_start - volume_start).abs() < 1e-9);
        assert!((predicted_end(s) - (volume_start + total)).abs() < 1e-6);
        // No projection chunk → predicted_chunks is None.
        assert_eq!(s.predicted_chunks, None);
    }

    /// Method-B fallback branch: neither a projection chunk nor a positive
    /// VCP-message rate, so the rate comes from `fallback_azimuth_rate`.
    #[wasm_bindgen_test]
    fn rate_source_method_b_fallback() {
        let vcp = ExtractedVcp {
            number: 212, // precip → fallback_azimuth_rate(false, "CS", 1) == 21.1
            elevations: vec![elev(0.5, "CS", None)],
        };
        let plan = StreamingPlan::for_test(vec![], None);
        let snap = derive_volume_forecast(&vcp, &plan, 0.0, &[], &[], None, &[], None);
        let s = &snap.sweeps[0];
        assert_eq!(s.rate_source, RateSource::MethodBFallback);
        assert!((s.azimuth_rate_used - 21.1).abs() < 1e-9);
    }

    /// The cum-offset path accumulates across elevations: a two-elevation VCP
    /// with no projections places sweep 2's predicted start at the end of
    /// sweep 1's weighted duration.
    #[wasm_bindgen_test]
    fn cum_offset_accumulates_across_elevations() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(0.5, "CS", Some(20.0)), elev(1.5, "CS", Some(10.0))],
        };
        let plan = StreamingPlan::for_test(vec![], None);
        let volume_start = 100.0;
        let snap = derive_volume_forecast(&vcp, &plan, volume_start, &[], &[], None, &[], None);
        let total = vcp.estimated_volume_duration().unwrap();
        let durs = vcp.sweep_durations(total);
        assert_eq!(snap.sweeps.len(), 2);
        // Sweep 1 starts at volume_start, runs durs[0].
        assert!((snap.sweeps[0].predicted_start - volume_start).abs() < 1e-9);
        assert!((snap.sweeps[0].predicted_duration - durs[0]).abs() < 1e-6);
        // Sweep 2 starts where sweep 1 ended.
        assert!((snap.sweeps[1].predicted_start - (volume_start + durs[0])).abs() < 1e-6);
        assert!((snap.sweeps[1].predicted_duration - durs[1]).abs() < 1e-6);
    }

    /// Actuals: a `Complete` sweep with a matching `CachedSweep` carries
    /// observed start/end and an `Observed` timing label, while an elevation
    /// with no matching meta stays `Future` with `None` actuals.
    #[wasm_bindgen_test]
    fn actuals_observed_vs_future() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(0.5, "CS", Some(20.0)), elev(1.5, "CS", Some(20.0))],
        };
        let plan = StreamingPlan::for_test(vec![], None);
        // Only elevation 1 completed; it contributed 3 chunk spans.
        let metas = vec![cached(1, 1000.0, 1015.0)];
        let spans = vec![
            (1u8, 1000.0, 1005.0, 0u32),
            (1u8, 1005.0, 1010.0, 0u32),
            (1u8, 1010.0, 1015.0, 0u32),
        ];
        let snap = derive_volume_forecast(&vcp, &plan, 500.0, &metas, &spans, None, &[], None);

        let s1 = &snap.sweeps[0];
        assert_eq!(s1.status, SweepStatus::Complete);
        assert_eq!(s1.timing_source, Some(ForecastTimingLabel::Observed));
        assert_eq!(s1.actual_start, Some(1000.0));
        assert_eq!(s1.actual_end, Some(1015.0));
        // 3 spans matched elevation 1.
        assert_eq!(s1.actual_chunks, Some(3));

        let s2 = &snap.sweeps[1];
        assert_eq!(s2.status, SweepStatus::Future);
        assert_eq!(s2.timing_source, None);
        assert_eq!(s2.actual_start, None);
        assert_eq!(s2.actual_chunks, None);
    }

    /// `actual_chunks` is `Some(0)` when a sweep is `Complete` (has a matching
    /// meta) but `chunk_elev_spans` has no span for it — the archive path,
    /// where sweeps are observed without per-chunk arrival spans.
    #[wasm_bindgen_test]
    fn actual_chunks_zero_when_complete_but_no_spans() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(0.5, "CS", Some(20.0))],
        };
        let plan = StreamingPlan::for_test(vec![], None);
        let metas = vec![cached(1, 1000.0, 1015.0)];
        // No spans at all.
        let snap = derive_volume_forecast(&vcp, &plan, 500.0, &metas, &[], None, &[], None);
        let s = &snap.sweeps[0];
        assert_eq!(s.status, SweepStatus::Complete);
        assert_eq!(s.actual_chunks, Some(0));
    }

    /// `inter_volume_gap_secs` = `volume_start - previous_volume_end` when the
    /// previous end is known; `None` otherwise.
    #[wasm_bindgen_test]
    fn inter_volume_gap_present_only_with_previous_end() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(0.5, "CS", Some(20.0))],
        };
        let plan = StreamingPlan::for_test(vec![], None);

        let with_prev =
            derive_volume_forecast(&vcp, &plan, 1000.0, &[], &[], Some(990.0), &[], None);
        assert_eq!(with_prev.inter_volume_gap_secs, Some(10.0));

        let no_prev = derive_volume_forecast(&vcp, &plan, 1000.0, &[], &[], None, &[], None);
        assert_eq!(no_prev.inter_volume_gap_secs, None);
    }

    /// `predicted_inter_volume_gap_secs` fires only when the previous volume's
    /// end is known AND the first arrival is a `"Start"` chunk carrying a
    /// `predicted_available_at`. A non-Start first arrival suppresses it.
    #[wasm_bindgen_test]
    fn predicted_inter_volume_gap_requires_start_first_arrival() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(0.5, "CS", Some(20.0))],
        };
        let plan = StreamingPlan::for_test(vec![], None);

        let mut start = ChunkArrivalStat::minimal_for_test(1, 1005.0);
        start.chunk_type = "Start";
        start.predicted_available_at = Some(1002.0);
        let snap = derive_volume_forecast(
            &vcp,
            &plan,
            1000.0,
            &[],
            &[],
            Some(990.0),
            &[start.clone()],
            None,
        );
        // 1002 (predicted available) − 990 (prev end) = 12.
        assert_eq!(snap.predicted_inter_volume_gap_secs, Some(12.0));

        // First arrival is Intermediate → suppressed even with a prediction.
        let mut inter = ChunkArrivalStat::minimal_for_test(1, 1005.0);
        inter.chunk_type = "Intermediate";
        inter.predicted_available_at = Some(1002.0);
        let snap2 =
            derive_volume_forecast(&vcp, &plan, 1000.0, &[], &[], Some(990.0), &[inter], None);
        assert_eq!(snap2.predicted_inter_volume_gap_secs, None);

        // No previous end → suppressed even with a Start arrival.
        let snap3 = derive_volume_forecast(&vcp, &plan, 1000.0, &[], &[], None, &[start], None);
        assert_eq!(snap3.predicted_inter_volume_gap_secs, None);
    }

    /// `predicted_volume_end` uses the plan's captured end-of-volume collection
    /// time when present, else `volume_start + estimated duration`.
    #[wasm_bindgen_test]
    fn predicted_volume_end_prefers_plan_then_estimate() {
        let vcp = ExtractedVcp {
            number: 212,
            elevations: vec![elev(0.5, "CS", Some(20.0))],
        };
        // Plan knows the end-of-volume collection time.
        let plan = StreamingPlan::for_test(vec![], Some(1234.0));
        let snap = derive_volume_forecast(&vcp, &plan, 500.0, &[], &[], None, &[], None);
        assert!((snap.predicted_volume_end - 1234.0).abs() < 1e-9);

        // Plan has no end → volume_start + estimated_volume_duration.
        let plan2 = StreamingPlan::for_test(vec![], None);
        let snap2 = derive_volume_forecast(&vcp, &plan2, 500.0, &[], &[], None, &[], None);
        let total = vcp.estimated_volume_duration().unwrap();
        assert!((snap2.predicted_volume_end - (500.0 + total)).abs() < 1e-6);
    }
}

/// Test-only constructors that need access to upstream timing types but are
/// kept out of the `tests` module so they read clearly.
#[cfg(test)]
mod test_support {
    use crate::nexrad::timing::{IntervalCase, PhysicsBreakdown};

    /// A throwaway `PhysicsBreakdown` for building `ChunkProjectedTimes` in
    /// tests — its fields are never read by `derive_volume_forecast`.
    pub(super) fn physics_stub() -> PhysicsBreakdown {
        PhysicsBreakdown {
            case: IntervalCase::IntraSweep,
            total_secs: 0.0,
            chunk_duration_secs: None,
            inter_sweep_gap_secs: None,
            waveform_penalty_secs: None,
        }
    }
}

/// How the streaming loop's wait before a chunk fetch was resolved.
///
/// The adaptive cross-volume wait (filtered streaming, target in the next
/// volume) periodically lists the next volume's S3 slot to correct for
/// accumulated timing-prediction error. This records which path the loop took
/// for a given arrival so prediction-error diagnostics can separate
/// model-driven sleeps from probe-corrected ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WaitResolution {
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
    pub(crate) fn short(&self) -> &'static str {
        match self {
            WaitResolution::SleptToPrediction => "sleep",
            WaitResolution::EarlyFired => "early",
            WaitResolution::ReAnchored => "reanchor",
        }
    }
}

/// Compact serialisable bucket key. Mirrors `ChunkCharacteristics` but
/// without depending on the timing crate's enum types — keeps the stored
/// diagnostics vocabulary free of upstream imports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BucketKey {
    pub chunk_type: &'static str,
    pub waveform: &'static str,
    pub channel: &'static str,
    pub first_in_sweep: bool,
}

impl BucketKey {
    /// Compact label for diagnostics output (e.g. `"I|CS|RP|F"`).
    pub(crate) fn short(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.chunk_type,
            self.waveform,
            self.channel,
            if self.first_in_sweep { "T" } else { "F" }
        )
    }

    pub(crate) fn from_characteristics(c: &ChunkCharacteristics) -> Self {
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
    pub(crate) fn prediction_error_secs(&self) -> Option<f64> {
        self.predicted_available_at.map(|p| self.success_at - p)
    }

    /// Time between the last empty poll and the successful download.
    /// Represents wait that could potentially have been avoided if the
    /// poll schedule were better aligned to S3 publishing time.
    pub(crate) fn wait_after_last_empty_ms(&self) -> Option<f64> {
        self.last_empty_poll_at
            .map(|t| (self.success_at - t) * 1000.0)
    }

    /// Actual collection-space interval between this chunk and `prev` — the
    /// ground truth the predicted wait should be compared against. Returns
    /// `None` if either chunk hasn't had its collection time back-filled.
    pub(crate) fn actual_interval_secs(&self, prev: &ChunkArrivalStat) -> Option<f64> {
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
    pub(crate) fn interval_error_ms(&self, prev: &ChunkArrivalStat) -> Option<f64> {
        let actual = self.actual_interval_secs(prev)?;
        let predicted = self.predicted_wait_secs?;
        Some((actual - predicted) * 1000.0)
    }

    /// Minimal arrival sample for tests — all diagnostic fields empty except
    /// `sequence` / `success_at`. Lets callers in other modules build a
    /// `ChunkArrivalStat` without spelling out every field.
    #[cfg(test)]
    pub(crate) fn minimal_for_test(sequence: u32, success_at: f64) -> Self {
        Self {
            sequence,
            wait_resolution: WaitResolution::default(),
            chunk_type: "Intermediate",
            elevation_number: None,
            chunk_index_in_sweep: None,
            chunks_in_sweep: None,
            predicted_available_at: None,
            empty_polls: 0,
            last_empty_poll_at: None,
            s3_last_modified_at: None,
            success_at,
            bucket_key: None,
            stats_n_at_prediction: 0,
            scheduler_path: None,
            physics_breakdown: None,
            anchor_source: None,
            availability_lag_ms: None,
            collection_time_secs: None,
            predicted_wait_secs: None,
            predicted_with_plan_revision: None,
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Build a `SweepForecast` with the given actual start/end and otherwise
    /// inert fields. Only `actual_start`/`actual_end` are read by the method
    /// under test.
    fn forecast_with_actuals(actual_start: Option<f64>, actual_end: Option<f64>) -> SweepForecast {
        SweepForecast {
            elev_number: 1,
            elev_angle: 0.5,
            waveform: "CS".to_string(),
            azimuth_rate_used: 20.0,
            rate_source: RateSource::VcpMessage,
            predicted_start: 0.0,
            predicted_duration: 0.0,
            predicted_chunks: None,
            actual_start,
            actual_end,
            actual_chunks: None,
            timing_source: None,
            status: SweepStatus::Future,
        }
    }

    // ---- SweepForecast::actual_duration ----

    #[wasm_bindgen_test]
    fn actual_duration_some_when_end_after_start() {
        let f = forecast_with_actuals(Some(100.0), Some(115.5));
        let d = f.actual_duration().expect("duration");
        assert!((d - 15.5).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn actual_duration_none_when_end_equals_start() {
        // Guard is strict `e > s`, so equal endpoints yield None.
        let f = forecast_with_actuals(Some(100.0), Some(100.0));
        assert_eq!(f.actual_duration(), None);
    }

    #[wasm_bindgen_test]
    fn actual_duration_none_when_end_before_start() {
        let f = forecast_with_actuals(Some(100.0), Some(90.0));
        assert_eq!(f.actual_duration(), None);
    }

    #[wasm_bindgen_test]
    fn actual_duration_none_when_either_endpoint_missing() {
        assert_eq!(
            forecast_with_actuals(Some(100.0), None).actual_duration(),
            None
        );
        assert_eq!(
            forecast_with_actuals(None, Some(100.0)).actual_duration(),
            None
        );
        assert_eq!(forecast_with_actuals(None, None).actual_duration(), None);
    }

    // ---- RateSource::short ----

    #[wasm_bindgen_test]
    fn rate_source_short_labels() {
        assert_eq!(RateSource::VcpMessage.short(), "VCP");
        assert_eq!(RateSource::MethodBFallback.short(), "FB");
        assert_eq!(RateSource::ProjectionLibrary.short(), "LIB");
    }

    // ---- WaitResolution ----

    #[wasm_bindgen_test]
    fn wait_resolution_short_labels() {
        assert_eq!(WaitResolution::SleptToPrediction.short(), "sleep");
        assert_eq!(WaitResolution::EarlyFired.short(), "early");
        assert_eq!(WaitResolution::ReAnchored.short(), "reanchor");
    }

    #[wasm_bindgen_test]
    fn wait_resolution_default_is_slept_to_prediction() {
        assert_eq!(WaitResolution::default(), WaitResolution::SleptToPrediction);
    }

    // ---- BucketKey::short ----

    #[wasm_bindgen_test]
    fn bucket_key_short_first_in_sweep_true() {
        let k = BucketKey {
            chunk_type: "I",
            waveform: "CS",
            channel: "RP",
            first_in_sweep: true,
        };
        assert_eq!(k.short(), "I|CS|RP|T");
    }

    #[wasm_bindgen_test]
    fn bucket_key_short_first_in_sweep_false() {
        let k = BucketKey {
            chunk_type: "S",
            waveform: "CDWO",
            channel: "SZ2",
            first_in_sweep: false,
        };
        assert_eq!(k.short(), "S|CDWO|SZ2|F");
    }

    // ---- BucketKey::from_characteristics (covers chunk_type_str/waveform_str/channel_str) ----

    #[wasm_bindgen_test]
    fn bucket_key_from_characteristics_intermediate_cs_constant() {
        let c = ChunkCharacteristics {
            chunk_type: ChunkType::Intermediate,
            waveform_type: WaveformType::CS,
            channel_configuration: ChannelConfiguration::ConstantPhase,
            is_first_in_sweep: false,
        };
        let k = BucketKey::from_characteristics(&c);
        assert_eq!(k.chunk_type, "I");
        assert_eq!(k.waveform, "CS");
        assert_eq!(k.channel, "CP");
        assert!(!k.first_in_sweep);
        assert_eq!(k.short(), "I|CS|CP|F");
    }

    #[wasm_bindgen_test]
    fn bucket_key_from_characteristics_start_b_random_first() {
        let c = ChunkCharacteristics {
            chunk_type: ChunkType::Start,
            waveform_type: WaveformType::B,
            channel_configuration: ChannelConfiguration::RandomPhase,
            is_first_in_sweep: true,
        };
        let k = BucketKey::from_characteristics(&c);
        assert_eq!(k.chunk_type, "S");
        assert_eq!(k.waveform, "B");
        assert_eq!(k.channel, "RP");
        assert!(k.first_in_sweep);
    }

    #[wasm_bindgen_test]
    fn bucket_key_from_characteristics_end_spp_sz2() {
        let c = ChunkCharacteristics {
            chunk_type: ChunkType::End,
            waveform_type: WaveformType::SPP,
            channel_configuration: ChannelConfiguration::SZ2Phase,
            is_first_in_sweep: false,
        };
        let k = BucketKey::from_characteristics(&c);
        assert_eq!(k.chunk_type, "E");
        assert_eq!(k.waveform, "SPP");
        assert_eq!(k.channel, "SZ2");
    }

    #[wasm_bindgen_test]
    fn bucket_key_from_characteristics_cdw_unknown_channel() {
        let c = ChunkCharacteristics {
            chunk_type: ChunkType::Intermediate,
            waveform_type: WaveformType::CDW,
            channel_configuration: ChannelConfiguration::UnknownPhase,
            is_first_in_sweep: false,
        };
        let k = BucketKey::from_characteristics(&c);
        assert_eq!(k.waveform, "CDW");
        assert_eq!(k.channel, "?");
    }

    #[wasm_bindgen_test]
    fn bucket_key_from_characteristics_unknown_waveform() {
        let c = ChunkCharacteristics {
            chunk_type: ChunkType::Intermediate,
            waveform_type: WaveformType::Unknown,
            channel_configuration: ChannelConfiguration::ConstantPhase,
            is_first_in_sweep: false,
        };
        let k = BucketKey::from_characteristics(&c);
        assert_eq!(k.waveform, "?");
    }

    // ---- ChunkArrivalStat diagnostic methods ----

    #[wasm_bindgen_test]
    fn prediction_error_secs_positive_and_none() {
        // success_at(1010) − predicted(1002) = 8 (too optimistic).
        let mut a = ChunkArrivalStat::minimal_for_test(1, 1010.0);
        a.predicted_available_at = Some(1002.0);
        let err = a.prediction_error_secs().expect("err");
        assert!((err - 8.0).abs() < 1e-9);

        // Negative: waited longer than predicted.
        let mut b = ChunkArrivalStat::minimal_for_test(2, 1000.0);
        b.predicted_available_at = Some(1005.0);
        assert!((b.prediction_error_secs().unwrap() - (-5.0)).abs() < 1e-9);

        // No prediction → None.
        let c = ChunkArrivalStat::minimal_for_test(3, 1000.0);
        assert_eq!(c.prediction_error_secs(), None);
    }

    #[wasm_bindgen_test]
    fn wait_after_last_empty_ms_scales_to_ms_and_none() {
        let mut a = ChunkArrivalStat::minimal_for_test(1, 1002.5);
        a.last_empty_poll_at = Some(1000.0);
        // (1002.5 − 1000.0) * 1000 = 2500 ms.
        let ms = a.wait_after_last_empty_ms().expect("ms");
        assert!((ms - 2500.0).abs() < 1e-6);

        // No last-empty-poll timestamp → None.
        let b = ChunkArrivalStat::minimal_for_test(2, 1002.5);
        assert_eq!(b.wait_after_last_empty_ms(), None);
    }

    #[wasm_bindgen_test]
    fn actual_interval_secs_requires_both_and_increasing() {
        let mut cur = ChunkArrivalStat::minimal_for_test(2, 0.0);
        cur.collection_time_secs = Some(110.0);
        let mut prev = ChunkArrivalStat::minimal_for_test(1, 0.0);
        prev.collection_time_secs = Some(100.0);
        assert!((cur.actual_interval_secs(&prev).unwrap() - 10.0).abs() < 1e-9);

        // Non-increasing (cur == prev) → None (guard is strict `c > p`).
        let mut eq = ChunkArrivalStat::minimal_for_test(3, 0.0);
        eq.collection_time_secs = Some(100.0);
        assert_eq!(eq.actual_interval_secs(&prev), None);

        // Missing prev collection time → None.
        let missing = ChunkArrivalStat::minimal_for_test(4, 0.0);
        assert_eq!(cur.actual_interval_secs(&missing), None);
    }

    #[wasm_bindgen_test]
    fn interval_error_ms_combines_actual_and_predicted() {
        let mut cur = ChunkArrivalStat::minimal_for_test(2, 0.0);
        cur.collection_time_secs = Some(112.0);
        cur.predicted_wait_secs = Some(9.0);
        let mut prev = ChunkArrivalStat::minimal_for_test(1, 0.0);
        prev.collection_time_secs = Some(100.0);
        // actual interval = 12; (12 − 9) * 1000 = 3000 ms (underestimated gap).
        let err = cur.interval_error_ms(&prev).expect("err");
        assert!((err - 3000.0).abs() < 1e-6);

        // No predicted_wait_secs → None even with a valid interval.
        let mut cur2 = ChunkArrivalStat::minimal_for_test(3, 0.0);
        cur2.collection_time_secs = Some(112.0);
        assert_eq!(cur2.interval_error_ms(&prev), None);
    }
}
