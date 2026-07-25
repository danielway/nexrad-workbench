use super::{
    chunk_characteristics, estimate_interval, ChunkCharacteristics, ChunkTimingModel,
    ChunkTimingStats, ElevationChunkMapper, PhysicsBreakdown, TimingTuning,
};
use chrono::Duration as ChronoDuration;
use chrono::{DateTime, Utc};
use log::debug;
use nexrad_data::aws::realtime::{ChunkIdentifier, ChunkType};
use nexrad_decode::messages::volume_coverage_pattern;

/// Which branch of [`estimate_chunk_processing_diagnostics`] produced the
/// returned wait duration. Surfaced in the per-chunk diagnostic so we can see
/// which estimator drove each prediction without trawling the debug log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerPath {
    /// Constant Start→first-Intermediate gap (no prediction needed).
    StartConstant,
    /// `ChunkTimingStats` has samples for this bucket; the interval is a
    /// 70/30 physics/historical blend plus an `(avg_attempts − 1)` retry
    /// budget.
    Blended,
    /// Fell back to the physics model — no historical samples for this bucket
    /// (cold start, new VCP, etc.).
    Physics,
    /// Final fallback when neither metadata nor stats were available.
    Legacy,
}

impl SchedulerPath {
    pub(crate) fn short(&self) -> &'static str {
        match self {
            SchedulerPath::StartConstant => "start",
            SchedulerPath::Blended => "blend",
            SchedulerPath::Physics => "phys",
            SchedulerPath::Legacy => "legacy",
        }
    }
}

/// Rich diagnostic version of [`estimate_chunk_processing_time`]'s output.
#[derive(Debug, Clone)]
pub(crate) struct EstimatedChunkProcessing {
    pub duration: ChronoDuration,
    pub path: SchedulerPath,
    /// Number of samples in the next chunk's bucket at prediction time. 0
    /// when the lookup failed (e.g. legacy/start path or no metadata).
    pub stats_n_at_prediction: usize,
    /// Physics decomposition for the next-chunk transition. `Some` whenever
    /// metadata for both the current and next chunk was available — this is
    /// the case for `Blended` and `Physics` paths, never for `Legacy` or
    /// `StartConstant`.
    pub physics_breakdown: Option<PhysicsBreakdown>,
    /// `ChunkCharacteristics` bucket key used by the estimator's lookup —
    /// `Some` for `Blended` / `Physics` paths, `None` for `Legacy` /
    /// `StartConstant`. Lets diagnostics display the same key the lookup
    /// hit (or missed) without re-deriving it from the chunk identifier.
    pub bucket: Option<ChunkCharacteristics>,
}

/// Attempts to estimate the time at which the next chunk will be available given the previous
/// chunk. Requires an [ElevationChunkMapper] to describe the relationship between chunk sequence
/// and VCP elevations. A None result indicates that the chunk is already available or that an
/// estimate cannot be made.
///
/// The estimate is anchored to the previous chunk's upload time rather than the current time,
/// so querying late will correctly yield a past time (indicating the chunk should already be
/// available) rather than pushing the estimate forward.
pub(crate) fn estimate_chunk_availability_time(
    chunk: &ChunkIdentifier,
    vcp: &volume_coverage_pattern::Message,
    elevation_chunk_mapper: &ElevationChunkMapper,
    timing_stats: Option<&ChunkTimingStats>,
) -> Option<DateTime<Utc>> {
    let processing_time =
        estimate_chunk_processing_time(chunk, vcp, elevation_chunk_mapper, timing_stats)?;

    let anchor = chunk.upload_date_time().unwrap_or_else(Utc::now);
    let availability_time = anchor + processing_time;

    Some(availability_time)
}

/// Attempts to estimate the time the given chunk will take to become available in the real-time S3
/// bucket following the previous chunk. Requires an [ElevationChunkMapper] to describe the
/// relationship between chunk sequence and VCP elevations. A None result indicates that an estimate
/// cannot be made.
pub(crate) fn estimate_chunk_processing_time(
    chunk: &ChunkIdentifier,
    vcp: &volume_coverage_pattern::Message,
    elevation_chunk_mapper: &ElevationChunkMapper,
    timing_stats: Option<&ChunkTimingStats>,
) -> Option<ChronoDuration> {
    estimate_chunk_processing_diagnostics(chunk, vcp, elevation_chunk_mapper, timing_stats)
        .map(|e| e.duration)
}

/// Like [`estimate_chunk_processing_time`] but exposes which estimator path
/// was taken, the bucket sample count at prediction time, and (when applicable)
/// the physics-term decomposition. Used by the diagnostics modal to attribute
/// prediction errors to a specific component.
pub(crate) fn estimate_chunk_processing_diagnostics(
    chunk: &ChunkIdentifier,
    vcp: &volume_coverage_pattern::Message,
    elevation_chunk_mapper: &ElevationChunkMapper,
    timing_stats: Option<&ChunkTimingStats>,
) -> Option<EstimatedChunkProcessing> {
    // Start chunks: the next chunk is the first intermediate within the same volume,
    // which follows the Start chunk by only a few seconds (not the full inter-volume gap).
    if chunk.chunk_type() == ChunkType::Start {
        let gap_ms = (ChunkTimingModel::start_to_first_intermediate_gap_secs() * 1000.0) as i64;
        return Some(EstimatedChunkProcessing {
            duration: ChronoDuration::milliseconds(gap_ms),
            path: SchedulerPath::StartConstant,
            stats_n_at_prediction: 0,
            physics_breakdown: None,
            bucket: None,
        });
    }

    // Physics + optional historical blend via the shared primitive. The
    // primitive carries the retry budget (`(avg_attempts − 1).max(0)` for
    // the target bucket) so we don't redo the lookup here.
    //
    // Bucket lookup keys on the *arriving* chunk's characteristics rather
    // than the anchor's. Writes in `StreamingState::update_timing_stats`
    // record each chunk's arrival under the arriving chunk's elevation,
    // so reading under the anchor's elevation would silently miss.
    if let (Some(current_metadata), Some(next_metadata)) = (
        elevation_chunk_mapper.get_chunk_metadata(chunk.sequence()),
        elevation_chunk_mapper.get_chunk_metadata(chunk.sequence() + 1),
    ) {
        let bucket = chunk_characteristics(next_metadata, vcp);
        let estimate = estimate_interval(
            current_metadata,
            next_metadata,
            bucket.as_ref(),
            timing_stats,
            &TimingTuning::DEFAULT,
        );

        // Wait until the chunk is expected to be available *and* a typical
        // retry budget has elapsed. Poll bias is applied separately by the
        // streaming loop via `TimingTuning::poll_bias_secs`.
        let wait_secs = estimate.seconds + estimate.retry_budget_secs;

        let path = if estimate.used_historical {
            SchedulerPath::Blended
        } else {
            SchedulerPath::Physics
        };

        debug!(
            "Scheduler {}: interval={:.3}s (physics={:.3}s, used_hist={}, attempts_pad={:.3}s)",
            path.short(),
            estimate.seconds,
            estimate.physics.total_secs,
            estimate.used_historical,
            estimate.retry_budget_secs,
        );

        return Some(EstimatedChunkProcessing {
            duration: ChronoDuration::milliseconds((wait_secs * 1000.0) as i64),
            path,
            stats_n_at_prediction: estimate.stats_n,
            physics_breakdown: Some(estimate.physics),
            bucket,
        });
    }

    // Final fallback: use old static estimation for edge cases where metadata is unavailable
    if let Some(elevation) = elevation_chunk_mapper
        .get_sequence_elevation_number(chunk.sequence())
        .and_then(|elevation_number| vcp.elevations().get(elevation_number - 1))
    {
        let wait_time = get_legacy_default_wait_time(
            elevation.waveform_type(),
            elevation.channel_configuration(),
        );

        debug!(
            "No metadata available, using legacy static estimation of {}ms",
            wait_time.num_milliseconds()
        );

        return Some(EstimatedChunkProcessing {
            duration: wait_time,
            path: SchedulerPath::Legacy,
            stats_n_at_prediction: 0,
            physics_breakdown: None,
            bucket: None,
        });
    }

    None
}

/// Multi-hop variant of [`estimate_chunk_processing_diagnostics`] used by the
/// filter-aware streaming path.
///
/// Sums the per-hop blended interval for every step from the current chunk's
/// sequence up to (and including) the hop into `target_sequence`, so a
/// streaming loop that's about to skip several chunks can sleep through the
/// entire run in one shot. Returns `None` when sequence metadata is missing.
///
/// Each hop uses the same shared [`estimate_interval`] primitive as the
/// single-hop scheduler and the projector, so a filter-disabled run remains
/// in lock-step with the no-filter path.
pub(crate) fn estimate_chunk_processing_time_to_target(
    current: &ChunkIdentifier,
    target_sequence: usize,
    vcp: &volume_coverage_pattern::Message,
    elevation_chunk_mapper: &ElevationChunkMapper,
    timing_stats: Option<&ChunkTimingStats>,
) -> Option<EstimatedChunkProcessing> {
    let current_sequence = current.sequence();
    if target_sequence <= current_sequence {
        return None;
    }

    // Single-hop case: defer to the existing diagnostic so the no-filter
    // path's predictions are identical to today's behavior.
    if target_sequence == current_sequence + 1 {
        return estimate_chunk_processing_diagnostics(
            current,
            vcp,
            elevation_chunk_mapper,
            timing_stats,
        );
    }

    let mut total_secs: f64 = 0.0;
    let mut last_breakdown: Option<PhysicsBreakdown> = None;
    let mut any_hop_used_historical = false;

    // First hop: handle the Start-chunk special case (1.5s constant) before
    // any interval math so the constant matches the single-hop StartConstant
    // path.
    let first_hop_start = if current.chunk_type() == ChunkType::Start {
        total_secs += ChunkTimingModel::start_to_first_intermediate_gap_secs();
        current_sequence + 1
    } else {
        current_sequence
    };

    // Sum every blended hop from `first_hop_start` to `target_sequence`.
    for seq in first_hop_start..target_sequence {
        let prev_meta = elevation_chunk_mapper.get_chunk_metadata(seq)?;
        let next_meta = elevation_chunk_mapper.get_chunk_metadata(seq + 1)?;
        let bucket = chunk_characteristics(next_meta, vcp);
        let estimate = estimate_interval(
            prev_meta,
            next_meta,
            bucket.as_ref(),
            timing_stats,
            &TimingTuning::DEFAULT,
        );
        total_secs += estimate.seconds;
        last_breakdown = Some(estimate.physics);
        if estimate.used_historical {
            any_hop_used_historical = true;
        }
    }

    // Retry budget for the target chunk: re-derive a single `IntervalEstimate`
    // ending at `target_sequence` so the budget comes from the same primitive
    // the per-hop interval math used. The hop value matches one already summed
    // above, so this lookup is just for `retry_budget_secs` / `stats_n`.
    let target_meta = elevation_chunk_mapper.get_chunk_metadata(target_sequence)?;
    let target_bucket = chunk_characteristics(target_meta, vcp);
    let target_prev_meta = elevation_chunk_mapper.get_chunk_metadata(target_sequence - 1)?;
    let target_estimate = estimate_interval(
        target_prev_meta,
        target_meta,
        target_bucket.as_ref(),
        timing_stats,
        &TimingTuning::DEFAULT,
    );
    let stats_n_at_prediction = target_estimate.stats_n;
    total_secs += target_estimate.retry_budget_secs;

    let path = if any_hop_used_historical {
        SchedulerPath::Blended
    } else {
        SchedulerPath::Physics
    };

    Some(EstimatedChunkProcessing {
        duration: ChronoDuration::milliseconds((total_secs * 1000.0) as i64),
        path,
        stats_n_at_prediction,
        physics_breakdown: last_breakdown,
        bucket: target_bucket,
    })
}

/// Legacy default wait time based on waveform type and channel configuration.
///
/// Only used as a last resort when chunk metadata is unavailable (should be rare).
fn get_legacy_default_wait_time(
    waveform_type: nexrad_decode::messages::volume_coverage_pattern::WaveformType,
    channel_config: nexrad_decode::messages::volume_coverage_pattern::ChannelConfiguration,
) -> ChronoDuration {
    use nexrad_decode::messages::volume_coverage_pattern::{ChannelConfiguration, WaveformType};

    if waveform_type == WaveformType::CS {
        ChronoDuration::seconds(11)
    } else if channel_config == ChannelConfiguration::ConstantPhase {
        ChronoDuration::seconds(7)
    } else {
        ChronoDuration::seconds(4)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_vcp::{build_vcp, TestElevation};
    use super::super::ChunkCharacteristics;
    use super::*;
    use chrono::{DateTime, Utc};
    use nexrad_data::aws::realtime::VolumeIndex;
    use nexrad_decode::messages::volume_coverage_pattern::{
        self, ChannelConfiguration, WaveformType,
    };
    use wasm_bindgen_test::wasm_bindgen_test;

    fn when() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn chunk(sequence: usize, chunk_type: ChunkType) -> ChunkIdentifier {
        ChunkIdentifier::new(
            "KDMX".to_string(),
            VolumeIndex::new(1),
            when().naive_utc(),
            sequence,
            chunk_type,
            Some(when()),
        )
    }

    /// One super-res CS elevation at 22.5 dps → 6 chunks (seqs 2..=7), final 7.
    /// sweep = 360/22.5 - 0.67 = 15.33; chunk_dur = 15.33/6.
    fn vcp_1elev() -> volume_coverage_pattern::Message<'static> {
        build_vcp(&[TestElevation {
            super_res: true,
            elevation_angle_raw: 0,
            azimuth_rate_raw: 1 << 14, // 22.5 dps
            waveform_raw: 1,           // CS
            channel_raw: 0,            // ConstantPhase
        }])
    }

    const CHUNK_DUR: f64 = (360.0 / 22.5 - 0.67) / 6.0;

    #[wasm_bindgen_test]
    fn start_chunk_uses_constant_gap() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let e =
            estimate_chunk_processing_diagnostics(&chunk(1, ChunkType::Start), &vcp, &mapper, None)
                .unwrap();
        assert_eq!(e.path, SchedulerPath::StartConstant);
        assert_eq!(e.duration, ChronoDuration::milliseconds(1500));
        assert!(e.bucket.is_none());
        assert!(e.physics_breakdown.is_none());
    }

    #[wasm_bindgen_test]
    fn physics_path_when_no_stats() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        // Intra-sweep hop seq 3 -> 4 (both elevation 1, not first-in-sweep).
        let e = estimate_chunk_processing_diagnostics(
            &chunk(3, ChunkType::Intermediate),
            &vcp,
            &mapper,
            None,
        )
        .unwrap();
        assert_eq!(e.path, SchedulerPath::Physics);
        // No stats → wait == pure physics chunk duration (retry budget 0).
        let wait = e.duration.num_milliseconds() as f64 / 1000.0;
        assert!((wait - CHUNK_DUR).abs() < 1e-3, "wait={wait}");
    }

    #[wasm_bindgen_test]
    fn blended_path_when_collection_stats_present() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        // The bucket the read path builds for the ARRIVING chunk (seq 4).
        let next_meta = mapper.get_chunk_metadata(4).unwrap();
        let bucket = chunk_characteristics(next_meta, &vcp).unwrap();
        // Seed a COLLECTION-domain historical interval far from physics so the
        // 70/30 blend is observable, plus an attempts average for retry budget.
        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, ChronoDuration::seconds(30), None, 3);
        stats.attach_collection_interval(&bucket, ChronoDuration::seconds(30));

        let e = estimate_chunk_processing_diagnostics(
            &chunk(3, ChunkType::Intermediate),
            &vcp,
            &mapper,
            Some(&stats),
        )
        .unwrap();
        assert_eq!(e.path, SchedulerPath::Blended);
        assert_eq!(e.bucket, Some(bucket));
        // wait = blended interval + retry budget. Blend = 0.7*physics + 0.3*30;
        // retry budget = avg_attempts - 1 = 2.
        let blend = 0.7 * CHUNK_DUR + 0.3 * 30.0;
        let expected = blend + 2.0;
        let wait = e.duration.num_milliseconds() as f64 / 1000.0;
        assert!(
            (wait - expected).abs() < 1e-3,
            "wait={wait} expected={expected}"
        );
    }

    #[wasm_bindgen_test]
    fn legacy_path_when_metadata_absent_but_elevation_resolves() {
        // At the final sequence (7), get_chunk_metadata(8) is None so the
        // metadata-pair guard fails, but get_sequence_elevation_number(7)
        // resolves → legacy path. The elevation is CS → 11s.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let e =
            estimate_chunk_processing_diagnostics(&chunk(7, ChunkType::End), &vcp, &mapper, None)
                .unwrap();
        assert_eq!(e.path, SchedulerPath::Legacy);
        assert_eq!(e.duration, ChronoDuration::seconds(11)); // CS
        assert!(e.bucket.is_none());
    }

    #[wasm_bindgen_test]
    fn legacy_default_wait_time_branches() {
        // CS → 11s regardless of channel config.
        assert_eq!(
            get_legacy_default_wait_time(WaveformType::CS, ChannelConfiguration::RandomPhase),
            ChronoDuration::seconds(11)
        );
        // Non-CS + ConstantPhase → 7s.
        assert_eq!(
            get_legacy_default_wait_time(WaveformType::CDW, ChannelConfiguration::ConstantPhase),
            ChronoDuration::seconds(7)
        );
        // Non-CS + non-ConstantPhase → 4s.
        assert_eq!(
            get_legacy_default_wait_time(WaveformType::CDW, ChannelConfiguration::RandomPhase),
            ChronoDuration::seconds(4)
        );
    }

    #[wasm_bindgen_test]
    fn to_target_rejects_non_advancing_targets() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let cur = chunk(3, ChunkType::Intermediate);
        assert!(estimate_chunk_processing_time_to_target(&cur, 3, &vcp, &mapper, None).is_none());
        assert!(estimate_chunk_processing_time_to_target(&cur, 2, &vcp, &mapper, None).is_none());
    }

    #[wasm_bindgen_test]
    fn to_target_single_hop_equals_single_hop_diagnostic() {
        // Stated invariant: target == current+1 must EXACTLY equal the
        // single-hop diagnostic.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let cur = chunk(3, ChunkType::Intermediate);
        let single = estimate_chunk_processing_diagnostics(&cur, &vcp, &mapper, None).unwrap();
        let multi = estimate_chunk_processing_time_to_target(&cur, 4, &vcp, &mapper, None).unwrap();
        assert_eq!(single.duration, multi.duration);
    }

    #[wasm_bindgen_test]
    fn to_target_multi_hop_sums_per_hop_plus_target_retry_budget() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        // Give the target bucket (seq 6) an attempts average so the target
        // retry budget is non-zero and we can verify it's added exactly once.
        let target_meta = mapper.get_chunk_metadata(6).unwrap();
        let target_bucket = chunk_characteristics(target_meta, &vcp).unwrap();
        let mut stats = ChunkTimingStats::new();
        stats.add_timing(target_bucket, ChronoDuration::seconds(5), None, 4); // attempts=4

        let cur = chunk(3, ChunkType::Intermediate);
        let multi =
            estimate_chunk_processing_time_to_target(&cur, 6, &vcp, &mapper, Some(&stats)).unwrap();

        // Expected = sum of per-hop estimate_interval seconds for hops 3->4,
        // 4->5, 5->6, plus the target (seq 6) retry budget (avg_attempts-1=3).
        let mut sum = 0.0;
        for seq in 3..6 {
            let prev = mapper.get_chunk_metadata(seq).unwrap();
            let next = mapper.get_chunk_metadata(seq + 1).unwrap();
            let bucket = chunk_characteristics(next, &vcp);
            let est = estimate_interval(
                prev,
                next,
                bucket.as_ref(),
                Some(&stats),
                &TimingTuning::DEFAULT,
            );
            sum += est.seconds;
        }
        let target_prev = mapper.get_chunk_metadata(5).unwrap();
        let target_est = estimate_interval(
            target_prev,
            target_meta,
            Some(&target_bucket),
            Some(&stats),
            &TimingTuning::DEFAULT,
        );
        sum += target_est.retry_budget_secs;
        assert!((target_est.retry_budget_secs - 3.0).abs() < 1e-9);

        let got = multi.duration.num_milliseconds() as f64 / 1000.0;
        assert!((got - sum).abs() < 1e-3, "got={got} sum={sum}");
        assert_eq!(multi.stats_n_at_prediction, target_est.stats_n);
    }

    #[wasm_bindgen_test]
    fn to_target_start_first_hop_adds_constant_once() {
        // From the Start chunk, the first hop is the 1.5s constant, then the
        // loop sums intervals from current+1. Verify the constant is added
        // exactly once (not double-counted) by comparing against a hand sum.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let start = chunk(1, ChunkType::Start);
        // Target seq 4: hops are start->2 (constant 1.5), 2->3, 3->4.
        let multi =
            estimate_chunk_processing_time_to_target(&start, 4, &vcp, &mapper, None).unwrap();

        let mut sum = ChunkTimingModel::start_to_first_intermediate_gap_secs();
        for seq in 2..4 {
            let prev = mapper.get_chunk_metadata(seq).unwrap();
            let next = mapper.get_chunk_metadata(seq + 1).unwrap();
            let bucket = chunk_characteristics(next, &vcp);
            let est = estimate_interval(prev, next, bucket.as_ref(), None, &TimingTuning::DEFAULT);
            sum += est.seconds;
        }
        // target (seq 4) retry budget is 0 (no stats).
        let got = multi.duration.num_milliseconds() as f64 / 1000.0;
        assert!((got - sum).abs() < 1e-3, "got={got} sum={sum}");
    }

    #[wasm_bindgen_test]
    fn unbucketable_bucket_is_none_for_start_metadata() {
        // chunk_characteristics for the Start chunk (no elevation) is None;
        // this pins that the read-path bucket builder returns None rather than
        // a bogus key.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let start_meta = mapper.get_chunk_metadata(1).unwrap();
        assert!(chunk_characteristics(start_meta, &vcp).is_none());
        // And a data chunk DOES bucket, with the normalized Intermediate type.
        let data_meta = mapper.get_chunk_metadata(2).unwrap();
        let b: ChunkCharacteristics = chunk_characteristics(data_meta, &vcp).unwrap();
        assert_eq!(b.chunk_type, ChunkType::Intermediate);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::super::test_vcp::{build_vcp, TestElevation};
    use super::*;
    use chrono::{DateTime, Utc};
    use nexrad_data::aws::realtime::VolumeIndex;
    use nexrad_decode::messages::volume_coverage_pattern;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn when() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn chunk(sequence: usize, chunk_type: ChunkType) -> ChunkIdentifier {
        ChunkIdentifier::new(
            "KDMX".to_string(),
            VolumeIndex::new(1),
            when().naive_utc(),
            sequence,
            chunk_type,
            Some(when()),
        )
    }

    /// One super-res CS elevation at 22.5 dps → 6 chunks (seqs 2..=7), final 7.
    fn vcp_1elev() -> volume_coverage_pattern::Message<'static> {
        build_vcp(&[TestElevation {
            super_res: true,
            elevation_angle_raw: 0,
            azimuth_rate_raw: 1 << 14, // 22.5 dps
            waveform_raw: 1,           // CS
            channel_raw: 0,            // ConstantPhase
        }])
    }

    /// One STANDARD-res (3-chunk: seqs 2..=4, final 4) non-CS elevation.
    /// waveform=CDW (raw 2), channel selectable so the legacy branch resolves
    /// to either 7s (ConstantPhase) or 4s (RandomPhase).
    fn vcp_1elev_non_cs(channel_raw: u8) -> volume_coverage_pattern::Message<'static> {
        build_vcp(&[TestElevation {
            super_res: false,
            elevation_angle_raw: 0,
            azimuth_rate_raw: 1 << 14, // 22.5 dps
            waveform_raw: 2,           // CDW (non-CS)
            channel_raw,
        }])
    }

    const CHUNK_DUR: f64 = (360.0 / 22.5 - 0.67) / 6.0;

    // ── SchedulerPath::short() — every variant (untested by mod tests) ──────

    #[wasm_bindgen_test]
    fn scheduler_path_short_covers_all_variants() {
        assert_eq!(SchedulerPath::StartConstant.short(), "start");
        assert_eq!(SchedulerPath::Blended.short(), "blend");
        assert_eq!(SchedulerPath::Physics.short(), "phys");
        assert_eq!(SchedulerPath::Legacy.short(), "legacy");
    }

    // ── estimate_chunk_availability_time — anchors to upload time ───────────

    #[wasm_bindgen_test]
    fn availability_time_for_start_chunk_is_anchor_plus_constant_gap() {
        // Start chunk → 1500ms processing; anchor is the chunk's upload time
        // (which our builder sets to `when()`), so availability is exactly
        // when() + 1.5s.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let avail =
            estimate_chunk_availability_time(&chunk(1, ChunkType::Start), &vcp, &mapper, None)
                .unwrap();
        let expected = when() + ChronoDuration::milliseconds(1500);
        assert_eq!(avail, expected);
    }

    #[wasm_bindgen_test]
    fn availability_time_for_physics_chunk_is_anchor_plus_processing() {
        // Non-Start, no stats → physics chunk duration; availability is anchor
        // (when()) + that processing time. Verify by differencing against the
        // anchor rather than recomputing the millisecond truncation.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let cur = chunk(3, ChunkType::Intermediate);
        let proc = estimate_chunk_processing_time(&cur, &vcp, &mapper, None).unwrap();
        let avail = estimate_chunk_availability_time(&cur, &vcp, &mapper, None).unwrap();
        assert_eq!(avail, when() + proc);
        // And the processing component is the physics chunk duration.
        let secs = proc.num_milliseconds() as f64 / 1000.0;
        assert!((secs - CHUNK_DUR).abs() < 1e-3, "secs={secs}");
    }

    // ── estimate_chunk_processing_time — duration-only wrapper ──────────────

    #[wasm_bindgen_test]
    fn processing_time_equals_diagnostics_duration() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let cur = chunk(3, ChunkType::Intermediate);
        let diag = estimate_chunk_processing_diagnostics(&cur, &vcp, &mapper, None).unwrap();
        let dur = estimate_chunk_processing_time(&cur, &vcp, &mapper, None).unwrap();
        assert_eq!(dur, diag.duration);
    }

    #[wasm_bindgen_test]
    fn processing_time_is_none_when_diagnostics_is_none() {
        // Sequence past the end of the volume: neither the metadata pair nor a
        // legacy elevation resolves, so both diagnostics and the duration-only
        // wrapper yield None.
        let vcp = vcp_1elev(); // final sequence 7
        let mapper = ElevationChunkMapper::new(&vcp);
        let oob = chunk(8, ChunkType::Intermediate);
        assert!(estimate_chunk_processing_diagnostics(&oob, &vcp, &mapper, None).is_none());
        assert!(estimate_chunk_processing_time(&oob, &vcp, &mapper, None).is_none());
        assert!(estimate_chunk_availability_time(&oob, &vcp, &mapper, None).is_none());
    }

    // ── Legacy path: non-CS waveform branches via the PUBLIC function ───────

    #[wasm_bindgen_test]
    fn legacy_path_non_cs_constant_phase_yields_7s() {
        // Standard-res single non-CS / ConstantPhase elevation: seqs 2..=4,
        // final 4. Querying seq 4 fails the metadata pair (no seq 5) but the
        // elevation resolves → legacy. Non-CS + ConstantPhase → 7s.
        let vcp = vcp_1elev_non_cs(0); // ConstantPhase
        let mapper = ElevationChunkMapper::new(&vcp);
        let e = estimate_chunk_processing_diagnostics(
            &chunk(4, ChunkType::Intermediate),
            &vcp,
            &mapper,
            None,
        )
        .unwrap();
        assert_eq!(e.path, SchedulerPath::Legacy);
        assert_eq!(e.duration, ChronoDuration::seconds(7));
        assert!(e.bucket.is_none());
        assert!(e.physics_breakdown.is_none());
        assert_eq!(e.stats_n_at_prediction, 0);
    }

    #[wasm_bindgen_test]
    fn legacy_path_non_cs_random_phase_yields_4s() {
        // Same shape, RandomPhase channel → non-CS + non-ConstantPhase → 4s.
        let vcp = vcp_1elev_non_cs(1); // RandomPhase
        let mapper = ElevationChunkMapper::new(&vcp);
        let e = estimate_chunk_processing_diagnostics(
            &chunk(4, ChunkType::Intermediate),
            &vcp,
            &mapper,
            None,
        )
        .unwrap();
        assert_eq!(e.path, SchedulerPath::Legacy);
        assert_eq!(e.duration, ChronoDuration::seconds(4));
    }

    // ── estimate_chunk_processing_time_to_target — None / boundary paths ────

    #[wasm_bindgen_test]
    fn to_target_none_when_target_metadata_out_of_range() {
        // Target beyond the final sequence: a hop reaches a sequence with no
        // metadata, so the `?` short-circuits to None.
        let vcp = vcp_1elev(); // final 7, total 7
        let mapper = ElevationChunkMapper::new(&vcp);
        let cur = chunk(3, ChunkType::Intermediate);
        assert!(estimate_chunk_processing_time_to_target(&cur, 20, &vcp, &mapper, None).is_none());
    }

    #[wasm_bindgen_test]
    fn to_target_equal_sequence_is_none() {
        // target == current is non-advancing → None (distinct from the
        // existing test's target<current and target=2 cases for seq 3).
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let cur = chunk(5, ChunkType::Intermediate);
        assert!(estimate_chunk_processing_time_to_target(&cur, 5, &vcp, &mapper, None).is_none());
    }

    #[wasm_bindgen_test]
    fn to_target_single_hop_from_start_equals_start_constant() {
        // current+1 single-hop short-circuit must equal the single-hop
        // diagnostic even for the Start chunk (StartConstant path, 1.5s),
        // exercising the delegation branch with a Start input.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let start = chunk(1, ChunkType::Start);
        let single = estimate_chunk_processing_diagnostics(&start, &vcp, &mapper, None).unwrap();
        let multi =
            estimate_chunk_processing_time_to_target(&start, 2, &vcp, &mapper, None).unwrap();
        assert_eq!(multi.path, SchedulerPath::StartConstant);
        assert_eq!(multi.duration, single.duration);
        assert_eq!(multi.duration, ChronoDuration::milliseconds(1500));
    }
}
