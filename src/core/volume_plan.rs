//! Pure volume-assembly decisions for the 3-D ray-march renderer.
//!
//! The GPU ray marcher ([`crate::nexrad::render::volume_ray_renderer`]) makes
//! two structural assumptions about the packed volume it receives:
//!
//! 1. Per-sweep metadata is ordered by **ascending elevation angle**, with no
//!    duplicate angles — its bracket search walks the array linearly and its
//!    top guard compares against the last entry.
//! 2. Each sweep's gate rows sit on a **uniform azimuth grid** starting at 0°,
//!    so a bin index can be computed with `round(az / Δ)` instead of a search.
//!
//! Neither assumption holds for raw NEXRAD data. Sweeps arrive ordered by
//! elevation *number* (SAILS/MRLE supplemental cuts carry high numbers but low
//! angles, and split cuts repeat an angle), and radial azimuths are irregular,
//! start at an arbitrary angle, and can leave gaps. This module contains the
//! pure decisions that make both assumptions true before the data reaches the
//! GPU; the worker packer ([`crate::nexrad::decode::worker_api`]) executes them.

/// Maximum sweeps the ray marcher can hold.
///
/// Must match the `[25]` uniform-array size baked into the volume fragment
/// shader's GLSL. After [`plan_volume_sweeps`] dedups split cuts and SAILS
/// rescans, no operational VCP exceeds ~16 unique angles, so this is a safety
/// valve rather than a routine limit.
pub(crate) const MAX_VOLUME_SWEEPS: usize = 25;

/// Angular tolerance for treating two cuts as the same elevation.
///
/// Real VCP cut separations are >= 0.4°; split-cut and SAILS repeats of the
/// same nominal angle differ only by antenna jitter (hundredths of a degree).
pub(crate) const ELEVATION_DEDUP_EPSILON_DEG: f32 = 0.15;

/// One candidate sweep for inclusion in a volume, read from a sweep-blob header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SweepCandidate {
    /// Mean elevation angle of the cut, in degrees.
    pub elevation_deg: f32,
    /// Farthest range the sweep actually carries data to, in km
    /// (`first_gate + gate_count * gate_interval`). Distinguishes the
    /// long-range surveillance half of a split cut from the Doppler half.
    pub coverage_km: f32,
    /// ACTUAL category: start-of-sweep collection time, Unix seconds.
    pub sweep_start_secs: f64,
}

/// Choose and order the sweeps that make up one volume.
///
/// Returns positions into `candidates`, in **ascending elevation-angle order**,
/// with at most one sweep per distinct angle (within
/// [`ELEVATION_DEDUP_EPSILON_DEG`]). Within a group of same-angle cuts the
/// winner is the one with the greatest range coverage — for a VCP 12/212 split
/// cut that is the surveillance half, whose reflectivity reaches 460 km rather
/// than the Doppler half's ~300 km. Ties break toward the most recent sweep,
/// which picks the freshest SAILS/MRLE revisit of the 0.5° cut.
///
/// The result is truncated to `max_sweeps`, keeping the lowest angles: low-level
/// structure is what the volume view is for, and the highest cuts contribute the
/// least occupied volume.
pub(crate) fn plan_volume_sweeps(candidates: &[SweepCandidate], max_sweeps: usize) -> Vec<usize> {
    if candidates.is_empty() || max_sweeps == 0 {
        return Vec::new();
    }

    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        candidates[a]
            .elevation_deg
            .total_cmp(&candidates[b].elevation_deg)
            .then(a.cmp(&b))
    });

    let mut kept: Vec<usize> = Vec::with_capacity(order.len());
    let mut i = 0;
    while i < order.len() {
        // Group against the *leader's* angle, not a running value, so a long
        // run of near-identical cuts can't chain-drift past the tolerance.
        let leader_deg = candidates[order[i]].elevation_deg;
        let mut j = i + 1;
        while j < order.len()
            && candidates[order[j]].elevation_deg - leader_deg <= ELEVATION_DEDUP_EPSILON_DEG
        {
            j += 1;
        }

        let best = order[i..j]
            .iter()
            .copied()
            .max_by(|&a, &b| {
                let (ca, cb) = (&candidates[a], &candidates[b]);
                ca.coverage_km
                    .total_cmp(&cb.coverage_km)
                    .then(ca.sweep_start_secs.total_cmp(&cb.sweep_start_secs))
                    // Full tie: prefer the lower source index. `max_by` keeps
                    // the last maximum, so invert the comparison.
                    .then(b.cmp(&a))
            })
            .expect("group is non-empty");
        kept.push(best);
        i = j;
    }

    kept.truncate(max_sweeps);
    kept
}

// ── Azimuth resampling ──────────────────────────────────────────────────────

/// Widest bin grid we will resample onto.
///
/// A full-rotation sweep lands at its own radial count (360 or 720), so this
/// only binds for a *partial* sweep, whose fine spacing over a short arc would
/// otherwise imply an absurd full-circle grid of mostly-empty rows.
pub(crate) const MAX_AZIMUTH_BINS: u32 = 1440;

/// Rejection threshold for a bin whose nearest radial is too far away,
/// as a multiple of the sweep's median radial spacing. Matches the gap rule
/// the 2-D path applies in `FIND_NEAREST_AZ_P`.
pub(crate) const AZIMUTH_GAP_FACTOR: f32 = 1.5;

/// Shortest angular distance between two bearings, in degrees.
fn circular_distance_deg(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// Median spacing between adjacent radials, in degrees.
///
/// Uses wrap-aware forward gaps including the closing gap from the last radial
/// back to the first, so a full rotation reports its true spacing rather than
/// being skewed by the seam. Returns 0 when there is nothing to measure.
pub(crate) fn median_azimuth_spacing_deg(azimuths: &[f32]) -> f32 {
    if azimuths.len() < 2 {
        return 0.0;
    }
    let n = azimuths.len();
    let mut gaps: Vec<f32> = (0..n)
        .map(|i| {
            let next = azimuths[(i + 1) % n];
            (next - azimuths[i]).rem_euclid(360.0)
        })
        .collect();
    gaps.sort_by(f32::total_cmp);
    gaps[n / 2]
}

/// Number of uniform bins to resample a sweep onto, given its median spacing.
pub(crate) fn choose_bin_count(median_spacing_deg: f32) -> u32 {
    // NaN and non-positive spacings both mean "no usable grid".
    if median_spacing_deg.is_nan() || median_spacing_deg <= 0.0 {
        return 0;
    }
    ((360.0 / median_spacing_deg).round() as u32).clamp(16, MAX_AZIMUTH_BINS)
}

/// Map each uniform azimuth bin to the radial that should fill it.
///
/// Bin `i` represents bearing `i * 360 / bin_count` degrees — its **center**,
/// which is what lets the shader index with `round(az / Δ)` and interpolate
/// between `floor` and `floor + 1`. Entries are `None` where the nearest radial
/// is farther than [`AZIMUTH_GAP_FACTOR`] times the median spacing, so dropped
/// radials and partial sweeps leave holes instead of smearing a distant
/// neighbour across the gap.
///
/// `azimuths` must be sorted ascending in `[0, 360)`, as sweep blobs store them.
pub(crate) fn plan_azimuth_bins(azimuths: &[f32], bin_count: u32) -> Vec<Option<u32>> {
    let n = azimuths.len();
    if n == 0 || bin_count == 0 {
        return vec![None; bin_count as usize];
    }

    let median = median_azimuth_spacing_deg(azimuths);
    // A single radial has no measurable spacing; fall back to the bin width so
    // it still fills the bin it sits in rather than none at all.
    let bin_width = 360.0 / bin_count as f32;
    let threshold = if median > 0.0 {
        median * AZIMUTH_GAP_FACTOR
    } else {
        bin_width * AZIMUTH_GAP_FACTOR
    };

    (0..bin_count)
        .map(|i| {
            let target = i as f32 * bin_width;
            // First radial at or past the target; both neighbours wrap.
            let pos = azimuths.partition_point(|&a| a < target);
            let hi = if pos >= n { 0 } else { pos };
            let lo = (hi + n - 1) % n;

            let d_lo = circular_distance_deg(target, azimuths[lo]);
            let d_hi = circular_distance_deg(target, azimuths[hi]);
            let (best, dist) = if d_lo <= d_hi { (lo, d_lo) } else { (hi, d_hi) };

            (dist <= threshold).then_some(best as u32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn cand(elevation_deg: f32, coverage_km: f32, sweep_start_secs: f64) -> SweepCandidate {
        SweepCandidate {
            elevation_deg,
            coverage_km,
            sweep_start_secs,
        }
    }

    fn angles(candidates: &[SweepCandidate], plan: &[usize]) -> Vec<f32> {
        plan.iter().map(|&i| candidates[i].elevation_deg).collect()
    }

    #[wasm_bindgen_test]
    fn already_ascending_order_is_preserved() {
        let c = vec![
            cand(0.5, 460.0, 100.0),
            cand(1.5, 460.0, 130.0),
            cand(2.4, 460.0, 160.0),
            cand(3.4, 300.0, 190.0),
        ];
        let plan = plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS);
        assert_eq!(plan, vec![0, 1, 2, 3]);
    }

    #[wasm_bindgen_test]
    fn sails_cut_at_tail_sorts_to_the_front() {
        // SAILS supplemental 0.5° scan carries a high elevation *number*, so it
        // arrives last. Before sorting, the shader's `el > u_elevation[last]`
        // guard rejected everything above 0.5°.
        let c = vec![
            cand(0.5, 460.0, 100.0),
            cand(1.5, 460.0, 130.0),
            cand(2.4, 460.0, 160.0),
            cand(0.5, 460.0, 220.0), // SAILS rescan, newest
        ];
        let plan = plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS);
        // One 0.5° entry survives, and it is the freshest rescan.
        assert_eq!(plan, vec![3, 1, 2]);
        assert_eq!(angles(&c, &plan), vec![0.5, 1.5, 2.4]);
    }

    #[wasm_bindgen_test]
    fn split_cut_keeps_the_long_range_surveillance_half() {
        // VCP 212 elevations 1 and 2 are both 0.5°: CS (surveillance, 460 km)
        // and CD (Doppler, range-limited reflectivity).
        let c = vec![
            cand(0.48, 460.0, 100.0), // surveillance
            cand(0.52, 300.0, 118.0), // Doppler — newer, but shorter range
            cand(1.5, 460.0, 140.0),
        ];
        let plan = plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS);
        assert_eq!(plan, vec![0, 2]);
    }

    #[wasm_bindgen_test]
    fn equal_coverage_breaks_toward_the_newest_sweep() {
        let c = vec![
            cand(0.5, 460.0, 100.0),
            cand(0.5, 460.0, 240.0), // freshest MRLE revisit
            cand(0.5, 460.0, 170.0),
        ];
        let plan = plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS);
        assert_eq!(plan, vec![1]);
    }

    #[wasm_bindgen_test]
    fn fully_tied_group_keeps_the_lowest_source_index() {
        let c = vec![cand(0.5, 460.0, 100.0), cand(0.5, 460.0, 100.0)];
        assert_eq!(plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS), vec![0]);
    }

    #[wasm_bindgen_test]
    fn distinct_cuts_just_past_the_epsilon_are_both_kept() {
        // 0.4° is the tightest real VCP separation and must survive dedup.
        let c = vec![cand(0.5, 460.0, 100.0), cand(0.9, 460.0, 130.0)];
        assert_eq!(plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS), vec![0, 1]);

        // Just inside the tolerance, the pair collapses to the newer sweep.
        let c = vec![cand(0.50, 460.0, 100.0), cand(0.64, 460.0, 130.0)];
        assert_eq!(plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS), vec![1]);
    }

    #[wasm_bindgen_test]
    fn grouping_does_not_chain_drift_past_the_tolerance() {
        // Each neighbour is within epsilon of the previous one, but the run
        // spans 0.6° overall. Grouping against the leader keeps 0.5 and 0.6
        // together while 0.7 (0.2 above the leader) starts a new group.
        let c = vec![
            cand(0.5, 460.0, 100.0),
            cand(0.6, 460.0, 110.0),
            cand(0.7, 460.0, 120.0),
            cand(0.8, 460.0, 130.0),
        ];
        let plan = plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS);
        assert_eq!(angles(&c, &plan), vec![0.6, 0.8]);
    }

    #[wasm_bindgen_test]
    fn truncation_keeps_the_lowest_angles() {
        let c: Vec<SweepCandidate> = (0..10)
            .map(|i| cand(0.5 + i as f32, 460.0, 100.0 + i as f64))
            .collect();
        let plan = plan_volume_sweeps(&c, 3);
        assert_eq!(angles(&c, &plan), vec![0.5, 1.5, 2.5]);
    }

    #[wasm_bindgen_test]
    fn truncation_applies_after_dedup_not_before() {
        // Five candidates, three unique angles: a cap of 3 keeps all three
        // angles rather than losing one to a duplicate.
        let c = vec![
            cand(0.5, 460.0, 100.0),
            cand(0.5, 460.0, 200.0),
            cand(1.5, 460.0, 130.0),
            cand(1.5, 460.0, 230.0),
            cand(2.4, 460.0, 160.0),
        ];
        let plan = plan_volume_sweeps(&c, 3);
        assert_eq!(angles(&c, &plan), vec![0.5, 1.5, 2.4]);
    }

    #[wasm_bindgen_test]
    fn degenerate_inputs_return_empty_or_single() {
        assert!(plan_volume_sweeps(&[], MAX_VOLUME_SWEEPS).is_empty());
        let c = vec![cand(0.5, 460.0, 100.0)];
        assert!(plan_volume_sweeps(&c, 0).is_empty());
        assert_eq!(plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS), vec![0]);
    }

    #[wasm_bindgen_test]
    fn output_is_strictly_ascending_for_a_realistic_vcp_212_with_sails() {
        // Elevation-number order as the worker receives it: the two 0.5° split
        // halves first, the mid-volume SAILS rescan last.
        let c = vec![
            cand(0.50, 460.0, 0.0),   // 1  CS
            cand(0.50, 300.0, 20.0),  // 2  CD
            cand(0.90, 300.0, 40.0),  // 3
            cand(1.30, 300.0, 60.0),  // 4
            cand(1.80, 300.0, 80.0),  // 5
            cand(2.40, 300.0, 100.0), // 6
            cand(3.10, 300.0, 120.0), // 7
            cand(4.00, 300.0, 140.0), // 8
            cand(5.10, 300.0, 160.0), // 9
            cand(6.40, 300.0, 180.0), // 10
            cand(0.50, 460.0, 200.0), // 11 SAILS
        ];
        let plan = plan_volume_sweeps(&c, MAX_VOLUME_SWEEPS);
        let out = angles(&c, &plan);
        assert_eq!(
            out,
            vec![0.50, 0.90, 1.30, 1.80, 2.40, 3.10, 4.00, 5.10, 6.40]
        );
        assert!(out.windows(2).all(|w| w[1] > w[0]));
        // The SAILS rescan (newest, full coverage) represents 0.5°.
        assert_eq!(plan[0], 10);
    }
}

#[cfg(test)]
mod azimuth_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A full rotation of `n` evenly spaced radials starting at `start` degrees,
    /// sorted ascending as sweep blobs store them.
    fn ring(n: usize, start: f32) -> Vec<f32> {
        let step = 360.0 / n as f32;
        let mut v: Vec<f32> = (0..n)
            .map(|i| (start + i as f32 * step).rem_euclid(360.0))
            .collect();
        v.sort_by(f32::total_cmp);
        v
    }

    #[wasm_bindgen_test]
    fn median_spacing_reads_the_true_step_of_a_full_rotation() {
        assert!((median_azimuth_spacing_deg(&ring(720, 0.0)) - 0.5).abs() < 1e-4);
        assert!((median_azimuth_spacing_deg(&ring(360, 0.0)) - 1.0).abs() < 1e-4);
        // The seam gap is measured wrap-aware, so an offset start is identical.
        assert!((median_azimuth_spacing_deg(&ring(720, 173.2)) - 0.5).abs() < 1e-3);
    }

    #[wasm_bindgen_test]
    fn median_spacing_of_degenerate_input_is_zero() {
        assert_eq!(median_azimuth_spacing_deg(&[]), 0.0);
        assert_eq!(median_azimuth_spacing_deg(&[42.0]), 0.0);
    }

    #[wasm_bindgen_test]
    fn bin_count_follows_the_radial_resolution() {
        assert_eq!(choose_bin_count(0.5), 720);
        assert_eq!(choose_bin_count(1.0), 360);
        assert_eq!(choose_bin_count(0.0), 0);
        // A pathologically fine partial sweep is capped rather than exploding.
        assert_eq!(choose_bin_count(0.01), MAX_AZIMUTH_BINS);
    }

    #[wasm_bindgen_test]
    fn aligned_full_ring_maps_to_the_identity() {
        let az = ring(720, 0.0);
        let bins = plan_azimuth_bins(&az, 720);
        assert_eq!(bins.len(), 720);
        for (i, b) in bins.iter().enumerate() {
            assert_eq!(*b, Some(i as u32), "bin {i}");
        }
    }

    #[wasm_bindgen_test]
    fn arbitrary_start_angle_maps_correctly_across_the_wrap() {
        // The seam is the artifact this exists to kill: radials start at 173.2°
        // and the sorted array wraps in the middle, so index order and bearing
        // order disagree everywhere.
        let az = ring(720, 173.2);
        let bins = plan_azimuth_bins(&az, 720);
        assert!(
            bins.iter().all(|b| b.is_some()),
            "a full ring leaves no gaps"
        );
        for (i, b) in bins.iter().enumerate() {
            let target = i as f32 * 0.5;
            let picked = az[b.unwrap() as usize];
            // Every bin resolves to a radial within half a bin width, including
            // the bins straddling 0°/360°.
            assert!(
                circular_distance_deg(target, picked) <= 0.2501,
                "bin {i} (target {target}) picked {picked}"
            );
        }
        // Every radial is used exactly once — no duplication, no dropout.
        let mut used: Vec<u32> = bins.iter().map(|b| b.unwrap()).collect();
        used.sort_unstable();
        used.dedup();
        assert_eq!(used.len(), 720);
    }

    #[wasm_bindgen_test]
    fn irregular_spacing_picks_the_true_nearest_radial() {
        // Deliberately uneven, sorted ascending.
        let az = vec![0.0, 10.0, 11.0, 100.0, 240.0, 350.0];
        let bins = plan_azimuth_bins(&az, 36); // 10° bins
        assert_eq!(bins[0], Some(0)); // 0° → 0.0
        assert_eq!(bins[1], Some(1)); // 10° → 10.0
        assert_eq!(bins[10], Some(3)); // 100° → 100.0
        assert_eq!(bins[24], Some(4)); // 240° → 240.0
        assert_eq!(bins[35], Some(5)); // 350° → 350.0
    }

    #[wasm_bindgen_test]
    fn a_sector_gap_leaves_empty_bins() {
        // A full 1° ring with a 30° wedge of radials dropped.
        let az: Vec<f32> = (0..360)
            .map(|i| i as f32)
            .filter(|a| !(100.0..130.0).contains(a))
            .collect();
        let bins = plan_azimuth_bins(&az, 360);
        // Deep inside the wedge there is nothing within 1.5°.
        assert_eq!(bins[115], None);
        assert_eq!(bins[110], None);
        // Just outside it, coverage resumes.
        assert_eq!(bins[99], Some(99));
        assert!(bins[131].is_some());
        // Only the wedge is lost.
        assert_eq!(bins.iter().filter(|b| b.is_none()).count(), 28);
    }

    #[wasm_bindgen_test]
    fn a_partial_sweep_rejects_bins_outside_its_arc() {
        // Radials covering only 0°–90° at 0.5°.
        let az: Vec<f32> = (0..=180).map(|i| i as f32 * 0.5).collect();
        let bins = plan_azimuth_bins(&az, 720);
        assert_eq!(bins[0], Some(0));
        assert_eq!(bins[180], Some(180)); // 90°
                                          // The uncovered three-quarters of the circle stays empty rather than
                                          // being smeared with the nearest edge radial.
        assert_eq!(bins[360], None); // 180°
        assert_eq!(bins[540], None); // 270°
                                     // 181 radials plus one feathered bin just past each end of the arc,
                                     // which sits within the 1.5x threshold of the edge radial.
        assert_eq!(bins.iter().filter(|b| b.is_some()).count(), 183);
        assert_eq!(bins[181], Some(180)); // 90.5° feathers off the last radial
        assert_eq!(bins[182], None); // 91° is beyond the threshold
        assert_eq!(bins[719], Some(0)); // 359.5° feathers off the first radial
    }

    #[wasm_bindgen_test]
    fn half_bin_offset_radials_still_fill_every_bin() {
        // Radials sitting exactly on bin boundaries rather than centers: each
        // is half a bin width from two centers, inside the 1.5x threshold.
        let az = ring(360, 0.5);
        let bins = plan_azimuth_bins(&az, 360);
        assert!(bins.iter().all(|b| b.is_some()));
    }

    #[wasm_bindgen_test]
    fn degenerate_inputs_are_handled() {
        assert!(plan_azimuth_bins(&[], 8).iter().all(|b| b.is_none()));
        assert_eq!(plan_azimuth_bins(&[10.0, 20.0], 0).len(), 0);
        // A lone radial fills only the bins it is close to.
        let bins = plan_azimuth_bins(&[0.0], 4);
        assert_eq!(bins[0], Some(0));
        assert_eq!(bins[2], None);
    }
}
