//! Connected-component labeling on a polar (azimuth × gate) grid.
//!
//! 8-neighborhood, with the azimuth axis treated as circular so that a cell
//! straddling the 0°/360° seam is still a single component. Wrap-around
//! between adjacent sorted azimuth indices only applies when those two
//! radials are actually close in angle — live / partial sweeps can have
//! large gaps where the "first" and "last" indices are nowhere near each
//! other, and falsely connecting them would glue unrelated cells together.
//!
//! Iterative flood-fill (explicit `Vec` stack) — no recursion. One
//! [`Labeler`] serves every level of the threshold ladder.

/// One pixel belonging to a component: (azimuth index, gate index).
pub(super) type Pixel = (u16, u16);

/// Label connected components over a grid of above-threshold physical
/// values. Gates with `NaN` are considered background.
#[cfg(test)]
pub(super) fn label(
    grid: &[f32],
    azimuths: &[f32],
    azimuth_count: usize,
    gate_count: usize,
) -> Vec<Vec<Pixel>> {
    if azimuth_count == 0 || gate_count == 0 {
        return Vec::new();
    }
    debug_assert_eq!(grid.len(), azimuth_count * gate_count);
    let az_adjacent = precompute_az_adjacency(azimuths, azimuth_count);
    let mut labeler = Labeler::new(grid, &az_adjacent, gate_count);
    let mut components = Vec::new();
    for seed in 0..grid.len() {
        if let Some(c) = labeler.fill(seed, f32::NEG_INFINITY, 1) {
            components.push(c);
        }
    }
    components
}

/// Flood-fill labeler shared by every threshold level.
///
/// Levels are nested (a gate at or above level k+1 is also at or above level
/// k), so one byte per gate is enough to remember visits across all levels:
/// `stamp[i]` is the highest level (1-based) whose fill has visited gate
/// `i`. A gate is open at level `k` while `stamp[i] < k`. This keeps the
/// whole ladder at one visited allocation instead of one per level.
pub(super) struct Labeler<'a> {
    grid: &'a [f32],
    az_adjacent: &'a [bool],
    gate_count: usize,
    azimuth_count: usize,
    stamp: Vec<u8>,
    stack: Vec<usize>,
}

impl<'a> Labeler<'a> {
    pub(super) fn new(grid: &'a [f32], az_adjacent: &'a [bool], gate_count: usize) -> Self {
        let azimuth_count = az_adjacent.len();
        debug_assert_eq!(grid.len(), azimuth_count * gate_count);
        Self {
            grid,
            az_adjacent,
            gate_count,
            azimuth_count,
            stamp: vec![0; grid.len()],
            stack: Vec::new(),
        }
    }

    /// Flood-fill the component containing `seed` over gates `>= level`
    /// (8-neighbourhood, azimuth wrap where radials are adjacent), marking
    /// them with `stamp_val`. Returns `None` if `seed` is below `level` or
    /// was already filled at this stamp.
    pub(super) fn fill(&mut self, seed: usize, level: f32, stamp_val: u8) -> Option<Vec<Pixel>> {
        if !self.open(seed, level, stamp_val) {
            return None;
        }
        let gate_count = self.gate_count;
        let mut pixels: Vec<Pixel> = Vec::new();
        self.stack.clear();
        self.stack.push(seed);
        self.stamp[seed] = stamp_val;

        while let Some(idx) = self.stack.pop() {
            let az = idx / gate_count;
            let g = idx % gate_count;
            pixels.push((az as u16, g as u16));

            for daz in [-1i32, 0, 1] {
                let naz = wrap_az(az, daz, self.azimuth_count);
                // `az_adjacent[i]` says whether index `i` is spatially
                // adjacent to `i + 1 (mod)`. Going forward checks the
                // edge at `az`; going backward checks the edge at
                // `naz` (which is `az - 1 mod`).
                if daz > 0 && !self.az_adjacent[az] {
                    continue;
                }
                if daz < 0 && !self.az_adjacent[naz] {
                    continue;
                }
                for dg in [-1i32, 0, 1] {
                    if daz == 0 && dg == 0 {
                        continue;
                    }
                    let ng_signed = g as i32 + dg;
                    if ng_signed < 0 || ng_signed >= gate_count as i32 {
                        continue;
                    }
                    let nidx = naz * gate_count + ng_signed as usize;
                    if !self.open(nidx, level, stamp_val) {
                        continue;
                    }
                    self.stamp[nidx] = stamp_val;
                    self.stack.push(nidx);
                }
            }
        }
        Some(pixels)
    }

    fn open(&self, idx: usize, level: f32, stamp_val: u8) -> bool {
        // NaN compares false, so background never opens.
        self.stamp[idx] < stamp_val && self.grid[idx] >= level
    }
}

/// `result[i] == true` iff azimuth index `i` is spatially adjacent to
/// index `(i + 1) % az_count` — i.e. the angular gap between those two
/// sorted radials is within `MAX_GAP_FACTOR × median_spacing`.
pub(super) fn precompute_az_adjacency(azimuths: &[f32], azimuth_count: usize) -> Vec<bool> {
    const MAX_GAP_FACTOR: f32 = 2.0;

    if azimuth_count <= 1 {
        return vec![false; azimuth_count];
    }

    let mut gaps: Vec<f32> = Vec::with_capacity(azimuth_count);
    for i in 0..azimuth_count {
        let a = azimuths.get(i).copied().unwrap_or(-1.0);
        let b = azimuths
            .get((i + 1) % azimuth_count)
            .copied()
            .unwrap_or(-1.0);
        if a < 0.0 || b < 0.0 {
            gaps.push(f32::NAN);
            continue;
        }
        let diff = (b - a).rem_euclid(360.0);
        gaps.push(diff);
    }

    // Median of valid gaps, used as the reference spacing.
    let mut valid: Vec<f32> = gaps.iter().copied().filter(|v| !v.is_nan()).collect();
    if valid.is_empty() {
        return vec![false; azimuth_count];
    }
    valid.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = valid[valid.len() / 2].max(0.01);
    let max_gap = median * MAX_GAP_FACTOR;

    gaps.into_iter()
        .map(|g| !g.is_nan() && g <= max_gap)
        .collect()
}

/// Median angular spacing between consecutive sorted radials, in degrees
/// (1.0 when it cannot be determined).
pub(super) fn median_spacing_deg(azimuths: &[f32], azimuth_count: usize) -> f64 {
    let mut gaps: Vec<f32> = (0..azimuth_count)
        .filter_map(|i| {
            let a = *azimuths.get(i)?;
            let b = *azimuths.get((i + 1) % azimuth_count)?;
            (a >= 0.0 && b >= 0.0 && azimuth_count > 1).then(|| (b - a).rem_euclid(360.0))
        })
        .filter(|g| *g > 0.0)
        .collect();
    if gaps.is_empty() {
        return 1.0;
    }
    gaps.sort_by(f32::total_cmp);
    gaps[gaps.len() / 2] as f64
}

pub(super) fn wrap_az(az: usize, delta: i32, az_count: usize) -> usize {
    let n = az_count as i32;
    let v = az as i32 + delta;
    (((v % n) + n) % n) as usize
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const FG: f32 = 50.0;
    const BG: f32 = f32::NAN;

    fn even_az(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32 * (360.0 / n as f32)).collect()
    }

    // -- wrap_az --------------------------------------------------------

    #[wasm_bindgen_test]
    fn wrap_az_wraps_both_directions() {
        assert_eq!(wrap_az(0, -1, 8), 7);
        assert_eq!(wrap_az(7, 1, 8), 0);
        assert_eq!(wrap_az(3, 0, 8), 3);
        assert_eq!(wrap_az(3, 1, 8), 4);
        assert_eq!(wrap_az(0, -2, 8), 6);
    }

    // -- precompute_az_adjacency ---------------------------------------

    #[wasm_bindgen_test]
    fn adjacency_single_radial_is_never_adjacent() {
        assert_eq!(precompute_az_adjacency(&[5.0], 1), vec![false]);
        assert_eq!(precompute_az_adjacency(&[], 0), Vec::<bool>::new());
    }

    #[wasm_bindgen_test]
    fn adjacency_even_spacing_is_all_adjacent_including_seam() {
        let az = even_az(8);
        let adj = precompute_az_adjacency(&az, 8);
        assert_eq!(adj, vec![true; 8], "evenly-spaced radials all connect");
    }

    #[wasm_bindgen_test]
    fn adjacency_partial_sweep_breaks_the_large_seam_gap() {
        // 0,20,40,60: three 20° gaps, then a 300° wrap gap. Median spacing 20,
        // max allowed 40 → only the wrap edge (index 3) is non-adjacent.
        let az = vec![0.0_f32, 20.0, 40.0, 60.0];
        let adj = precompute_az_adjacency(&az, 4);
        assert_eq!(adj, vec![true, true, true, false]);
    }

    #[wasm_bindgen_test]
    fn adjacency_negative_padded_radials_are_not_adjacent() {
        // Padded (negative) azimuths produce NaN gaps → never adjacent.
        let az = vec![0.0_f32, -1.0, 90.0];
        let adj = precompute_az_adjacency(&az, 3);
        assert!(!adj[0]);
        assert!(!adj[1]);
    }

    #[wasm_bindgen_test]
    fn adjacency_all_padded_is_all_false() {
        let az = vec![-1.0_f32, -1.0, -1.0];
        assert_eq!(precompute_az_adjacency(&az, 3), vec![false; 3]);
    }

    // -- label ----------------------------------------------------------

    #[wasm_bindgen_test]
    fn label_empty_grid_dims_yields_nothing() {
        assert!(label(&[], &[], 0, 0).is_empty());
    }

    #[wasm_bindgen_test]
    fn label_all_background_yields_no_components() {
        let grid = vec![BG; 6];
        let az = even_az(2);
        assert!(label(&grid, &az, 2, 3).is_empty());
    }

    #[wasm_bindgen_test]
    fn label_single_radial_splits_on_gate_gaps() {
        // One radial (no azimuth connectivity), foreground at gate 0 and at
        // gates 3-4 with a gap between → two components.
        let grid = vec![FG, BG, BG, FG, FG];
        let az = vec![0.0_f32];
        let mut comps = label(&grid, &az, 1, 5);
        comps.sort_by_key(|c| c.len());
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].len(), 1); // the isolated gate
        assert_eq!(comps[1].len(), 2); // the adjacent pair
    }

    #[wasm_bindgen_test]
    fn label_connects_eight_neighborhood_into_one() {
        // (az0,g0),(az0,g1),(az1,g0) form one diagonal-connected blob.
        // grid[az*gate_count + g], 2 az × 2 gate.
        let grid = vec![FG, FG, FG, BG];
        let az = even_az(2);
        let comps = label(&grid, &az, 2, 2);
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].len(), 3);
    }

    #[wasm_bindgen_test]
    fn label_merges_across_azimuth_seam() {
        // Foreground on the first and last evenly-spaced radials at the same
        // gate → one component across the 0/360 seam.
        let az = even_az(4);
        // 4 az × 2 gate; mark (az0,g0) and (az3,g0).
        let mut grid = vec![BG; 8];
        grid[0] = FG; // az0,g0
        grid[3 * 2] = FG; // az3,g0
        let comps = label(&grid, &az, 4, 2);
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].len(), 2);
    }
}
