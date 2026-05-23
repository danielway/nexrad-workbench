//! Single-hop interval estimate shared by the scheduler and the projector.
//!
//! Both the scheduler ([`super::estimate_chunk_processing_diagnostics`]) and
//! the projector ([`super::project_scan_timing`]) need to ask the same
//! question: "given the previous chunk and the next chunk, when will the
//! next chunk be collected, available on S3, and worth polling?" Before this
//! module the two paths answered with subtly different formulas — the
//! scheduler used pure historical when stats were available; the projector
//! blended 70% physics + 30% historical. The blend is the better answer (it
//! hedges against systematic physics bias without over-fitting the ≤10-sample
//! window), so this module unifies on it.
//!
//! Beyond the interval itself, the scheduler also needs a *retry budget*
//! (the typical `(avg_attempts − 1)`s of polling overhead before a chunk
//! actually appears) and a *poll bias* (a small fixed delay applied so the
//! first poll lands slightly after expected availability rather than racing
//! the upload). Both are folded into [`IntervalEstimate`] so every consumer
//! reads a single source of truth instead of redoing the math: scheduler,
//! projector, and any UI surface that needs to display "next chunk at" all
//! call [`IntervalEstimate::project_times`] with the same anchor and lag.

use super::{
    ChunkCharacteristics, ChunkMetadata, ChunkTimingModel, ChunkTimingStats, PhysicsBreakdown,
};
use nexrad_data::aws::realtime::ChunkType;
use nexrad_decode::messages::volume_coverage_pattern;

/// Weight applied to historical samples in the physics/historical blend.
/// `seconds = (1 − HIST_WEIGHT) * physics + HIST_WEIGHT * historical`.
const HIST_WEIGHT: f64 = 0.3;

/// Result of [`estimate_interval`]: a blended interval prediction with the
/// physics decomposition, the sample count we drew from, whether historical
/// data contributed, and the retry budget for the *target* chunk's bucket.
#[derive(Debug, Clone, Copy)]
pub struct IntervalEstimate {
    /// Predicted seconds between the two chunks. Pure interval — does not
    /// include retry budget or poll bias.
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
    /// Typical extra polling overhead — `(avg_attempts − 1).max(0)` for the
    /// *target* chunk's bucket, in seconds. `0.0` when the bucket has no
    /// attempts samples. Applied by `project_times` to the `poll_at` axis;
    /// kept off `seconds` so the projector's collection/availability axes
    /// stay unbiased.
    pub retry_budget_secs: f64,
}

/// Three time axes derived from one [`IntervalEstimate`] anchored on the
/// previous chunk's collection time.
///
/// Every consumer picks the axis it needs, and they stay in lock-step
/// because they share the same calculation:
/// - [`Self::collection_at_secs`] — when the radar physically finishes
///   the next chunk. Drives timeline placement of future-chunk markers.
/// - [`Self::available_at_secs`] — when the chunk is expected to appear
///   in S3 (`collection_at + lag`). Drives "next in Xs" countdown labels.
/// - [`Self::poll_at_secs`] — when the scheduler should fire its first
///   download poll (`available_at + retry_budget + POLL_BIAS`). Drives the
///   sleep before each download attempt.
#[derive(Debug, Clone, Copy)]
pub struct ProjectedTimes {
    pub collection_at_secs: f64,
    pub available_at_secs: f64,
    pub poll_at_secs: f64,
}

impl IntervalEstimate {
    /// Bias applied between expected availability and the scheduler's first
    /// poll, so the first attempt lands slightly after the upload rather
    /// than racing it. Was `POLL_DELAY_AFTER_PREDICTED_MS` at the streaming
    /// loop's call site; consolidated here so projector, scheduler, and any
    /// UI surface that wants to display the poll axis use the same value.
    pub const POLL_BIAS_SECS: f64 = 0.750;

    /// Project the three time axes for the next chunk given the previous
    /// chunk's collection time and the empirical availability lag.
    pub fn project_times(
        &self,
        anchor_collection_secs: f64,
        availability_lag_secs: f64,
    ) -> ProjectedTimes {
        let collection_at_secs = anchor_collection_secs + self.seconds;
        let available_at_secs = collection_at_secs + availability_lag_secs;
        let poll_at_secs = available_at_secs + self.retry_budget_secs + Self::POLL_BIAS_SECS;
        ProjectedTimes {
            collection_at_secs,
            available_at_secs,
            poll_at_secs,
        }
    }
}

/// Single-hop interval estimate. When stats hold a historical average for
/// `next_bucket`, blends `0.7 * physics + 0.3 * historical`; otherwise
/// returns pure physics. Retry budget is read from the same bucket's
/// attempts average.
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

    let retry_budget_secs = next_bucket
        .zip(stats)
        .and_then(|(b, s)| s.get_average_attempts(b))
        .map(|att| (att - 1.0).max(0.0))
        .unwrap_or(0.0);

    IntervalEstimate {
        seconds,
        physics,
        stats_n,
        used_historical,
        retry_budget_secs,
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn estimate(seconds: f64, retry_budget_secs: f64) -> IntervalEstimate {
        IntervalEstimate {
            seconds,
            physics: PhysicsBreakdown {
                case: super::super::chunk_timing_model::IntervalCase::IntraSweep,
                total_secs: seconds,
                chunk_duration_secs: None,
                inter_sweep_gap_secs: None,
                waveform_penalty_secs: None,
            },
            stats_n: 0,
            used_historical: false,
            retry_budget_secs,
        }
    }

    #[wasm_bindgen_test]
    fn project_times_collection_is_anchor_plus_interval() {
        let est = estimate(3.0, 0.0);
        let t = est.project_times(1000.0, 0.5);
        assert!((t.collection_at_secs - 1003.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn project_times_available_includes_lag() {
        let est = estimate(3.0, 0.0);
        let t = est.project_times(1000.0, 0.5);
        assert!((t.available_at_secs - 1003.5).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn project_times_poll_includes_retry_budget_and_bias() {
        let est = estimate(3.0, 1.25);
        let t = est.project_times(1000.0, 0.5);
        let expected = 1003.5 + 1.25 + IntervalEstimate::POLL_BIAS_SECS;
        assert!((t.poll_at_secs - expected).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn project_times_zero_retry_budget_only_adds_bias() {
        let est = estimate(2.0, 0.0);
        let t = est.project_times(0.0, 0.0);
        assert!((t.poll_at_secs - (2.0 + IntervalEstimate::POLL_BIAS_SECS)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn project_times_axes_are_monotonic() {
        let est = estimate(5.0, 0.75);
        let t = est.project_times(2000.0, 0.4);
        assert!(t.collection_at_secs <= t.available_at_secs);
        assert!(t.available_at_secs <= t.poll_at_secs);
    }
}
