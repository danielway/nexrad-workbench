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
pub enum SchedulerPath {
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
    pub fn short(&self) -> &'static str {
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
pub struct EstimatedChunkProcessing {
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
pub fn estimate_chunk_availability_time(
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
pub fn estimate_chunk_processing_time(
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
pub fn estimate_chunk_processing_diagnostics(
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
pub fn estimate_chunk_processing_time_to_target(
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
