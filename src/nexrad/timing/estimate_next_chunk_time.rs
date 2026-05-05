use super::{
    ChunkCharacteristics, ChunkTimingModel, ChunkTimingStats, ElevationChunkMapper,
    PhysicsBreakdown,
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
    /// `ChunkTimingStats` has historical samples for this bucket; used the
    /// rolling average + retry budget.
    Historical,
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
            SchedulerPath::Historical => "hist",
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
    /// the case for `Historical` and `Physics` paths, never for `Legacy` or
    /// `StartConstant`.
    pub physics_breakdown: Option<PhysicsBreakdown>,
    /// `ChunkCharacteristics` bucket key used by the estimator's lookup —
    /// `Some` for `Historical` / `Physics` paths, `None` for `Legacy` /
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

    // Try to use the physics-based model via chunk metadata
    if let Some(next_metadata) = elevation_chunk_mapper.get_chunk_metadata(chunk.sequence() + 1) {
        let current_metadata = elevation_chunk_mapper.get_chunk_metadata(chunk.sequence());

        // Compute the physics breakdown up-front so we can attach it to either
        // the historical or physics path. The breakdown is purely descriptive;
        // the historical path's wait time is unchanged.
        let physics_breakdown = current_metadata
            .map(|c| ChunkTimingModel::estimate_chunk_interval_breakdown(c, next_metadata));

        // Check for historical timing data first.
        //
        // Key this lookup on the *arriving* chunk's (next_metadata) characteristics
        // rather than the anchor's. Writes in `StreamingState::update_timing_stats`
        // record each chunk's arrival duration under the arriving chunk's elevation,
        // so reading under the anchor's elevation looks up the wrong bucket and
        // silently falls back to pure physics. This cost us ~30% of the effective
        // shift from the physics penalties on sweep transitions.
        if let Some(elevation) = next_metadata
            .elevation_number()
            .and_then(|elevation_number| vcp.elevations().get(elevation_number - 1))
        {
            let characteristics = ChunkCharacteristics {
                chunk_type: ChunkType::Intermediate,
                waveform_type: elevation.waveform_type(),
                channel_configuration: elevation.channel_configuration(),
                is_first_in_sweep: next_metadata.is_first_in_sweep(),
            };

            let stats_n_at_prediction =
                timing_stats.map_or(0, |s| s.sample_count(&characteristics));
            let average_timing =
                timing_stats.and_then(|stats| stats.get_average_timing(&characteristics));
            let average_attempts =
                timing_stats.and_then(|stats| stats.get_average_attempts(&characteristics));

            if let (Some(avg_timing), Some(avg_attempts)) = (average_timing, average_attempts) {
                let mut wait_time = avg_timing;
                wait_time += chrono::Duration::seconds(avg_attempts as i64 - 1);

                debug!(
                    "Using historical average timing of {}ms and {} attempts for {}ms",
                    avg_timing.num_milliseconds(),
                    avg_attempts,
                    wait_time.num_milliseconds()
                );

                return Some(EstimatedChunkProcessing {
                    duration: wait_time,
                    path: SchedulerPath::Historical,
                    stats_n_at_prediction,
                    physics_breakdown,
                    bucket: Some(characteristics),
                });
            }
        }

        // Fall back to physics-based model. When we got here the bucket
        // lookup either failed (no elevation) or had no samples — record
        // whichever bucket we actually probed (if any) so diagnostics can
        // still show why the fallback fired.
        let probed_bucket = next_metadata
            .elevation_number()
            .and_then(|elevation_number| vcp.elevations().get(elevation_number - 1))
            .map(|elevation| ChunkCharacteristics {
                chunk_type: ChunkType::Intermediate,
                waveform_type: elevation.waveform_type(),
                channel_configuration: elevation.channel_configuration(),
                is_first_in_sweep: next_metadata.is_first_in_sweep(),
            });
        if let Some(breakdown) = physics_breakdown {
            let interval_ms = (breakdown.total_secs * 1000.0) as i64;

            debug!(
                "Using physics model: interval={}ms (az_rate={:.1} dps, first_in_sweep={})",
                interval_ms,
                next_metadata.azimuth_rate_dps(),
                next_metadata.is_first_in_sweep()
            );

            return Some(EstimatedChunkProcessing {
                duration: ChronoDuration::milliseconds(interval_ms),
                path: SchedulerPath::Physics,
                stats_n_at_prediction: 0,
                physics_breakdown: Some(breakdown),
                bucket: probed_bucket,
            });
        }
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
/// Sums the per-hop physics interval for every step from the current chunk's
/// sequence up to (and including) the hop into `target_sequence`, so a
/// streaming loop that's about to skip several chunks can sleep through the
/// entire run in one shot. Returns `None` when sequence metadata is missing.
///
/// The final hop substitutes the historical bucket average when one is
/// available — this matches the single-hop diagnostic's behavior so a
/// filter-disabled run is bit-identical to today.
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

    // First hop: handle the Start-chunk special case (1.5s constant) before
    // any physics math so the constant matches the single-hop StartConstant
    // path.
    if current.chunk_type() == ChunkType::Start {
        total_secs += ChunkTimingModel::start_to_first_intermediate_gap_secs();
    } else {
        let prev_meta = elevation_chunk_mapper.get_chunk_metadata(current_sequence)?;
        let next_meta = elevation_chunk_mapper.get_chunk_metadata(current_sequence + 1)?;
        let breakdown = ChunkTimingModel::estimate_chunk_interval_breakdown(prev_meta, next_meta);
        total_secs += breakdown.total_secs;
        last_breakdown = Some(breakdown);
    }

    // Intermediate hops: pure physics summing.
    for seq in (current_sequence + 1)..target_sequence {
        let prev_meta = elevation_chunk_mapper.get_chunk_metadata(seq)?;
        let next_meta = elevation_chunk_mapper.get_chunk_metadata(seq + 1)?;
        let breakdown = ChunkTimingModel::estimate_chunk_interval_breakdown(prev_meta, next_meta);
        total_secs += breakdown.total_secs;
        last_breakdown = Some(breakdown);
    }

    // Final hop: substitute historical bucket if available so the prediction
    // for the chunk we're actually waiting on lands as accurately as a
    // single-hop estimate would.
    let target_meta = elevation_chunk_mapper.get_chunk_metadata(target_sequence)?;
    let target_bucket = target_meta
        .elevation_number()
        .and_then(|n| vcp.elevations().get(n - 1))
        .map(|elevation| ChunkCharacteristics {
            chunk_type: ChunkType::Intermediate,
            waveform_type: elevation.waveform_type(),
            channel_configuration: elevation.channel_configuration(),
            is_first_in_sweep: target_meta.is_first_in_sweep(),
        });

    let mut path = SchedulerPath::Physics;
    let mut stats_n_at_prediction = 0usize;

    if let (Some(stats), Some(bucket)) = (timing_stats, target_bucket.as_ref()) {
        stats_n_at_prediction = stats.sample_count(bucket);
        if let (Some(avg_timing), Some(avg_attempts)) = (
            stats.get_average_timing(bucket),
            stats.get_average_attempts(bucket),
        ) {
            // Replace the last hop's physics with the historical average so
            // arrival-time prediction error is bounded by single-hop accuracy.
            let last_physics_secs = last_breakdown.as_ref().map(|b| b.total_secs).unwrap_or(0.0);
            total_secs = total_secs - last_physics_secs
                + avg_timing.num_milliseconds() as f64 / 1000.0
                + (avg_attempts - 1.0).max(0.0);
            path = SchedulerPath::Historical;
        }
    }

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
