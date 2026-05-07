//! Single-hop interval estimate shared by the scheduler and the projector.
//!
//! Both the scheduler ([`super::estimate_chunk_processing_diagnostics`]) and
//! the projector ([`super::project_scan_timing`]) need to ask the same
//! question: "given the previous chunk and the next chunk, how many seconds
//! apart will they be?" Before this module the two paths answered that
//! question with subtly different formulas — the scheduler used pure
//! historical when stats were available, while the projector blended 70%
//! physics + 30% historical. The blend is the better answer (it hedges
//! against systematic physics bias without over-fitting the ≤10-sample
//! window), so this module unifies on it.
//!
//! Per-call-site additions (e.g. the scheduler's `(avg_attempts − 1)` retry
//! budget) layer on top of the shared estimate at the call site, not here.

use super::{
    ChunkCharacteristics, ChunkMetadata, ChunkTimingModel, ChunkTimingStats, PhysicsBreakdown,
};
use nexrad_data::aws::realtime::ChunkType;
use nexrad_decode::messages::volume_coverage_pattern;

/// Weight applied to historical samples in the physics/historical blend.
/// `seconds = (1 − HIST_WEIGHT) * physics + HIST_WEIGHT * historical`.
const HIST_WEIGHT: f64 = 0.3;

/// Result of [`estimate_interval`]: a blended interval prediction with the
/// physics decomposition, the sample count we drew from, and whether
/// historical data contributed.
#[derive(Debug, Clone, Copy)]
pub struct IntervalEstimate {
    /// Predicted seconds between the two chunks.
    pub seconds: f64,
    /// Pure-physics decomposition that fed the blend (or stands alone if no
    /// historical samples were available).
    pub physics: PhysicsBreakdown,
    /// Sample count in the bucket the lookup probed. `0` when no bucket /
    /// no stats.
    pub stats_n: usize,
    /// `true` when historical samples were available and contributed to
    /// `seconds`. `false` for pure physics.
    pub used_historical: bool,
}

/// Single-hop interval estimate. When stats hold a historical average for
/// `next_bucket`, blends `0.7 * physics + 0.3 * historical`; otherwise
/// returns pure physics.
pub fn estimate_interval(
    prev: &ChunkMetadata,
    next: &ChunkMetadata,
    next_bucket: Option<&ChunkCharacteristics>,
    stats: Option<&ChunkTimingStats>,
) -> IntervalEstimate {
    let physics = ChunkTimingModel::estimate_chunk_interval_breakdown(prev, next);
    let stats_n = match (next_bucket, stats) {
        (Some(b), Some(s)) => s.sample_count(b),
        _ => 0,
    };
    let historical_secs = next_bucket
        .zip(stats)
        .and_then(|(b, s)| s.get_average_timing(b))
        .map(|d| d.num_milliseconds() as f64 / 1000.0);

    let (seconds, used_historical) = match historical_secs {
        Some(hist) => (
            physics.total_secs * (1.0 - HIST_WEIGHT) + hist * HIST_WEIGHT,
            true,
        ),
        None => (physics.total_secs, false),
    };

    IntervalEstimate {
        seconds,
        physics,
        stats_n,
        used_historical,
    }
}

/// Resolve a chunk's `ChunkCharacteristics` against the VCP for stats
/// lookup. Returns `None` when the chunk has no elevation (Start chunk) or
/// when the elevation index doesn't resolve in the VCP.
pub fn chunk_characteristics(
    meta: &ChunkMetadata,
    vcp: &volume_coverage_pattern::Message,
) -> Option<ChunkCharacteristics> {
    let elev_num = meta.elevation_number()?;
    let elevation = vcp.elevations().get(elev_num - 1)?;
    Some(ChunkCharacteristics {
        chunk_type: ChunkType::Intermediate,
        waveform_type: elevation.waveform_type(),
        channel_configuration: elevation.channel_configuration(),
        is_first_in_sweep: meta.is_first_in_sweep(),
    })
}
