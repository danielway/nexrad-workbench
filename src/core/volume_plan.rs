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
