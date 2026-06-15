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
    TimingTuning,
};
use nexrad_data::aws::realtime::ChunkType;
use nexrad_decode::messages::volume_coverage_pattern;

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
    /// Project the three time axes for the next chunk given the previous
    /// chunk's collection time and the empirical availability lag.
    /// `tuning.poll_bias_secs` shifts the poll axis so the first attempt
    /// lands slightly after the expected upload rather than racing it.
    pub fn project_times(
        &self,
        anchor_collection_secs: f64,
        availability_lag_secs: f64,
        tuning: &TimingTuning,
    ) -> ProjectedTimes {
        let collection_at_secs = anchor_collection_secs + self.seconds;
        let available_at_secs = collection_at_secs + availability_lag_secs;
        let poll_at_secs = available_at_secs + self.retry_budget_secs + tuning.poll_bias_secs;
        ProjectedTimes {
            collection_at_secs,
            available_at_secs,
            poll_at_secs,
        }
    }
}

/// Single-hop interval estimate. When stats hold a COLLECTION-domain
/// historical average for `next_bucket` (deltas of parsed radial
/// collection-end times), blends physics and historical per
/// `tuning.hist_weight` (default 70/30); otherwise returns pure physics.
/// AVAILABILITY-domain (S3 upload→upload) samples are never used as the
/// historical term — upload jitter would bias the collection axis.
/// Retry budget is read from the same bucket's attempts average.
pub fn estimate_interval(
    prev: &ChunkMetadata,
    next: &ChunkMetadata,
    next_bucket: Option<&ChunkCharacteristics>,
    stats: Option<&ChunkTimingStats>,
    tuning: &TimingTuning,
) -> IntervalEstimate {
    let physics = ChunkTimingModel::estimate_chunk_interval_breakdown(prev, next);
    let stats_n = match (next_bucket, stats) {
        (Some(b), Some(s)) => s.sample_count(b),
        _ => 0,
    };
    let historical_secs = next_bucket
        .zip(stats)
        .and_then(|(b, s)| s.average_collection_interval(b))
        .map(|d| d.num_milliseconds() as f64 / 1000.0);

    let (seconds, used_historical) = match historical_secs {
        Some(hist) => (
            physics.total_secs * (1.0 - tuning.hist_weight) + hist * tuning.hist_weight,
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
        let t = est.project_times(1000.0, 0.5, &TimingTuning::DEFAULT);
        assert!((t.collection_at_secs - 1003.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn project_times_available_includes_lag() {
        let est = estimate(3.0, 0.0);
        let t = est.project_times(1000.0, 0.5, &TimingTuning::DEFAULT);
        assert!((t.available_at_secs - 1003.5).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn project_times_poll_includes_retry_budget_and_bias() {
        let est = estimate(3.0, 1.25);
        let t = est.project_times(1000.0, 0.5, &TimingTuning::DEFAULT);
        let expected = 1003.5 + 1.25 + TimingTuning::DEFAULT.poll_bias_secs;
        assert!((t.poll_at_secs - expected).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn project_times_zero_retry_budget_only_adds_bias() {
        let est = estimate(2.0, 0.0);
        let t = est.project_times(0.0, 0.0, &TimingTuning::DEFAULT);
        assert!((t.poll_at_secs - (2.0 + TimingTuning::DEFAULT.poll_bias_secs)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn project_times_axes_are_monotonic() {
        let est = estimate(5.0, 0.75);
        let t = est.project_times(2000.0, 0.4, &TimingTuning::DEFAULT);
        assert!(t.collection_at_secs <= t.available_at_secs);
        assert!(t.available_at_secs <= t.poll_at_secs);
    }

    #[wasm_bindgen_test]
    fn estimate_interval_blend_follows_hist_weight() {
        use chrono::Duration;
        use nexrad_decode::messages::volume_coverage_pattern::{
            ChannelConfiguration, WaveformType,
        };

        // Two intra-sweep chunks at 18°/s.
        let prev = ChunkMetadata::for_test(5, Some(1), 1, 6, false, 18.0);
        let next = ChunkMetadata::for_test(6, Some(1), 2, 6, false, 18.0);
        let bucket = ChunkCharacteristics {
            chunk_type: nexrad_data::aws::realtime::ChunkType::Intermediate,
            waveform_type: WaveformType::CS,
            channel_configuration: ChannelConfiguration::ConstantPhase,
            is_first_in_sweep: false,
        };
        // Historical samples deliberately far from physics so the blend
        // weight is observable.
        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, Duration::seconds(99), None, 1);

        let physics_only =
            estimate_interval(&prev, &next, Some(&bucket), None, &TimingTuning::DEFAULT);
        assert!(!physics_only.used_historical);

        // An AVAILABILITY-only sample (no collection observation attached)
        // must NOT count as history — that would re-conflate the domains.
        let availability_only = estimate_interval(
            &prev,
            &next,
            Some(&bucket),
            Some(&stats),
            &TimingTuning::DEFAULT,
        );
        assert!(!availability_only.used_historical);
        assert!((availability_only.seconds - physics_only.seconds).abs() < 1e-9);

        // Once the worker reports collection ends, the COLLECTION-domain
        // interval becomes the historical term.
        stats.attach_collection_interval(&bucket, Duration::seconds(30));
        let default_blend = estimate_interval(
            &prev,
            &next,
            Some(&bucket),
            Some(&stats),
            &TimingTuning::DEFAULT,
        );
        assert!(default_blend.used_historical);
        let expected_default = physics_only.seconds * 0.7 + 30.0 * 0.3;
        assert!((default_blend.seconds - expected_default).abs() < 1e-9);

        // A heavier historical weight pulls the estimate toward the samples.
        let heavy = TimingTuning {
            hist_weight: 0.9,
            ..TimingTuning::DEFAULT
        };
        let heavy_blend = estimate_interval(&prev, &next, Some(&bucket), Some(&stats), &heavy);
        let expected_heavy = physics_only.seconds * 0.1 + 30.0 * 0.9;
        assert!((heavy_blend.seconds - expected_heavy).abs() < 1e-9);
        assert!(heavy_blend.seconds > default_blend.seconds);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::nexrad::timing::test_vcp::{build_vcp, TestElevation};
    use chrono::Duration;
    use nexrad_decode::messages::volume_coverage_pattern::{ChannelConfiguration, WaveformType};
    use wasm_bindgen_test::wasm_bindgen_test;

    // Two intra-sweep chunks at a fixed azimuth rate; convenient prev/next pair.
    fn intra_pair() -> (ChunkMetadata, ChunkMetadata) {
        let prev = ChunkMetadata::for_test(5, Some(1), 1, 6, false, 18.0);
        let next = ChunkMetadata::for_test(6, Some(1), 2, 6, false, 18.0);
        (prev, next)
    }

    fn cs_bucket() -> ChunkCharacteristics {
        ChunkCharacteristics {
            chunk_type: nexrad_data::aws::realtime::ChunkType::Intermediate,
            waveform_type: WaveformType::CS,
            channel_configuration: ChannelConfiguration::ConstantPhase,
            is_first_in_sweep: false,
        }
    }

    // ── estimate_interval: no-history / no-bucket branches ────────────────

    #[wasm_bindgen_test]
    fn estimate_interval_no_bucket_is_pure_physics() {
        let (prev, next) = intra_pair();
        let physics = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);

        // next_bucket = None short-circuits both the stats_n and retry lookups.
        let est = estimate_interval(&prev, &next, None, None, &TimingTuning::DEFAULT);
        assert!(!est.used_historical);
        assert_eq!(est.stats_n, 0);
        assert!((est.retry_budget_secs - 0.0).abs() < 1e-12);
        // seconds is a verbatim copy of physics.total_secs when no history.
        assert!((est.seconds - physics.total_secs).abs() < 1e-12);
        // The physics decomposition is always carried through verbatim.
        assert!((est.physics.total_secs - physics.total_secs).abs() < 1e-12);
        assert!(est.physics.case == physics.case);
    }

    #[wasm_bindgen_test]
    fn estimate_interval_bucket_but_no_stats_is_pure_physics() {
        let (prev, next) = intra_pair();
        let bucket = cs_bucket();
        let physics = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);

        // Some(bucket) but stats = None: the (Some, Some) arm for stats_n is
        // not taken, and the zip in the historical/retry lookups is None.
        let est = estimate_interval(&prev, &next, Some(&bucket), None, &TimingTuning::DEFAULT);
        assert!(!est.used_historical);
        assert_eq!(est.stats_n, 0);
        assert!((est.retry_budget_secs - 0.0).abs() < 1e-12);
        assert!((est.seconds - physics.total_secs).abs() < 1e-12);
    }

    #[wasm_bindgen_test]
    fn estimate_interval_stats_present_but_empty_bucket_is_pure_physics() {
        let (prev, next) = intra_pair();
        let bucket = cs_bucket();
        let physics = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);

        // Stats object exists but the probed bucket was never populated.
        let stats = ChunkTimingStats::new();
        let est = estimate_interval(
            &prev,
            &next,
            Some(&bucket),
            Some(&stats),
            &TimingTuning::DEFAULT,
        );
        assert!(!est.used_historical);
        assert_eq!(est.stats_n, 0);
        assert!((est.retry_budget_secs - 0.0).abs() < 1e-12);
        assert!((est.seconds - physics.total_secs).abs() < 1e-12);
    }

    // ── estimate_interval: stats_n + retry_budget plumbing ───────────────

    #[wasm_bindgen_test]
    fn estimate_interval_reports_sample_count_and_retry_budget() {
        let (prev, next) = intra_pair();
        let bucket = cs_bucket();

        // Two availability-only samples (no collection observation) — they
        // populate the bucket (stats_n) and carry attempts, but never become
        // the historical term.
        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, Duration::seconds(10), None, 3);
        stats.add_timing(bucket, Duration::seconds(10), None, 5);

        let est = estimate_interval(
            &prev,
            &next,
            Some(&bucket),
            Some(&stats),
            &TimingTuning::DEFAULT,
        );
        // Two samples in the bucket.
        assert_eq!(est.stats_n, 2);
        // Pure availability samples don't count as history.
        assert!(!est.used_historical);
        // avg_attempts = (3+5)/2 = 4.0 → retry = (4-1).max(0) = 3.0.
        assert!((est.retry_budget_secs - 3.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn estimate_interval_retry_budget_clamps_at_zero_for_single_attempt() {
        let (prev, next) = intra_pair();
        let bucket = cs_bucket();

        // A single-attempt sample: (1 - 1).max(0) = 0.0, no negative budget.
        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, Duration::seconds(10), None, 1);

        let est = estimate_interval(
            &prev,
            &next,
            Some(&bucket),
            Some(&stats),
            &TimingTuning::DEFAULT,
        );
        assert_eq!(est.stats_n, 1);
        assert!((est.retry_budget_secs - 0.0).abs() < 1e-12);
    }

    #[wasm_bindgen_test]
    fn estimate_interval_retry_budget_clamps_at_zero_for_zero_attempts() {
        let (prev, next) = intra_pair();
        let bucket = cs_bucket();

        // attempts = 0 would give (0 - 1) = -1; .max(0.0) must clamp it.
        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, Duration::seconds(10), None, 0);

        let est = estimate_interval(
            &prev,
            &next,
            Some(&bucket),
            Some(&stats),
            &TimingTuning::DEFAULT,
        );
        assert!(est.retry_budget_secs >= 0.0);
        assert!((est.retry_budget_secs - 0.0).abs() < 1e-12);
    }

    #[wasm_bindgen_test]
    fn estimate_interval_averages_multiple_collection_intervals() {
        let (prev, next) = intra_pair();
        let bucket = cs_bucket();
        let physics = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);

        // Two collection observations: 20s and 40s → integer-mean 30s.
        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, Duration::seconds(10), None, 1);
        stats.attach_collection_interval(&bucket, Duration::seconds(20));
        stats.add_timing(bucket, Duration::seconds(10), None, 1);
        stats.attach_collection_interval(&bucket, Duration::seconds(40));

        let est = estimate_interval(
            &prev,
            &next,
            Some(&bucket),
            Some(&stats),
            &TimingTuning::DEFAULT,
        );
        assert!(est.used_historical);
        assert_eq!(est.stats_n, 2);
        // DEFAULT.hist_weight = 0.3 → 0.7*physics + 0.3*30.
        let expected = physics.total_secs * 0.7 + 30.0 * 0.3;
        assert!((est.seconds - expected).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn estimate_interval_hist_weight_one_ignores_physics() {
        let (prev, next) = intra_pair();
        let bucket = cs_bucket();

        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, Duration::seconds(10), None, 1);
        stats.attach_collection_interval(&bucket, Duration::seconds(42));

        // hist_weight = 1.0 collapses the blend to the pure historical term.
        let tuning = TimingTuning {
            hist_weight: 1.0,
            ..TimingTuning::DEFAULT
        };
        let est = estimate_interval(&prev, &next, Some(&bucket), Some(&stats), &tuning);
        assert!(est.used_historical);
        assert!((est.seconds - 42.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn estimate_interval_hist_weight_zero_ignores_history_value() {
        let (prev, next) = intra_pair();
        let bucket = cs_bucket();
        let physics = ChunkTimingModel::estimate_chunk_interval_breakdown(&prev, &next);

        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, Duration::seconds(10), None, 1);
        stats.attach_collection_interval(&bucket, Duration::seconds(999));

        // hist_weight = 0.0 still flags used_historical (history was present)
        // but the blended value equals pure physics.
        let tuning = TimingTuning {
            hist_weight: 0.0,
            ..TimingTuning::DEFAULT
        };
        let est = estimate_interval(&prev, &next, Some(&bucket), Some(&stats), &tuning);
        assert!(est.used_historical);
        assert!((est.seconds - physics.total_secs).abs() < 1e-9);
    }

    // ── chunk_characteristics ────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn chunk_characteristics_none_for_start_chunk() {
        // elevation_number = None (the Start chunk) → early return None.
        let vcp = build_vcp(&[TestElevation::standard_cs(0, 1 << 14)]);
        let meta = ChunkMetadata::for_test(1, None, 0, 1, false, 0.0);
        let result = chunk_characteristics(&meta, &vcp);
        assert!(result.is_none());
    }

    #[wasm_bindgen_test]
    fn chunk_characteristics_none_when_elevation_out_of_range() {
        // VCP has a single cut (index 0). elevation_number = 2 → get(1) → None.
        let vcp = build_vcp(&[TestElevation::standard_cs(0, 1 << 14)]);
        let meta = ChunkMetadata::for_test(7, Some(2), 0, 3, true, 18.0);
        let result = chunk_characteristics(&meta, &vcp);
        assert!(result.is_none());
    }

    #[wasm_bindgen_test]
    fn chunk_characteristics_resolves_waveform_channel_and_first_in_sweep() {
        // Cut 0: CS / ConstantPhase. Cut 1: CDWO / RandomPhase. elevation_number
        // is 1-based, so Some(2) resolves to index 1 (the second cut).
        let cut0 = TestElevation::standard_cs(0, 1 << 14);
        let cut1 = TestElevation {
            super_res: false,
            elevation_angle_raw: 0,
            azimuth_rate_raw: 1 << 14,
            waveform_raw: 3, // CDWO
            channel_raw: 1,  // RandomPhase
        };
        let vcp = build_vcp(&[cut0, cut1]);

        // is_first_in_sweep = true is forwarded onto the bucket key.
        let meta = ChunkMetadata::for_test(8, Some(2), 0, 3, true, 18.0);
        let resolved = chunk_characteristics(&meta, &vcp).expect("cut index 1 resolves");
        assert!(resolved.chunk_type == nexrad_data::aws::realtime::ChunkType::Intermediate);
        assert!(resolved.waveform_type == WaveformType::CDWO);
        assert!(resolved.channel_configuration == ChannelConfiguration::RandomPhase);
        assert!(resolved.is_first_in_sweep);

        // The first cut (Some(1) → index 0) carries the CS / ConstantPhase key
        // and an intra-sweep (is_first_in_sweep = false) flag.
        let meta0 = ChunkMetadata::for_test(5, Some(1), 1, 3, false, 18.0);
        let resolved0 = chunk_characteristics(&meta0, &vcp).expect("cut index 0 resolves");
        assert!(resolved0.waveform_type == WaveformType::CS);
        assert!(resolved0.channel_configuration == ChannelConfiguration::ConstantPhase);
        assert!(!resolved0.is_first_in_sweep);
    }
}
