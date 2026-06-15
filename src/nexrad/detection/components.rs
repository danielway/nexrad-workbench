//! Connected-component labeling on a polar (azimuth × gate) grid.
//!
//! 8-neighborhood, with the azimuth axis treated as circular so that a cell
//! straddling the 0°/360° seam is still a single component. Wrap-around
//! between adjacent sorted azimuth indices only applies when those two
//! radials are actually close in angle — live / partial sweeps can have
//! large gaps where the "first" and "last" indices are nowhere near each
//! other, and falsely connecting them would glue unrelated cells together.
//!
//! Iterative flood-fill (explicit `Vec` stack) — no recursion, no
//! allocation per pixel after the initial capacity is reserved.

/// One pixel belonging to a component: (azimuth index, gate index).
pub(super) type Pixel = (u16, u16);

/// Label connected components over a grid of above-threshold physical
/// values. Gates with `NaN` are considered background.
pub(super) fn label(
    grid: &[f32],
    azimuths: &[f32],
    azimuth_count: usize,
    gate_count: usize,
) -> Vec<Vec<Pixel>> {
    if azimuth_count == 0 || gate_count == 0 {
        return Vec::new();
    }

    let n = azimuth_count * gate_count;
    debug_assert_eq!(grid.len(), n);

    // Pre-compute which azimuth-index pairs are spatially adjacent. Without
    // this, a partial sweep where `azimuths[0] = 5°` and
    // `azimuths[az_count - 1] = 50°` would still let the flood-fill jump
    // across the index wrap and glue unrelated cells together.
    let az_adjacent = precompute_az_adjacency(azimuths, azimuth_count);

    let mut visited = vec![false; n];
    let mut components: Vec<Vec<Pixel>> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    for start_az in 0..azimuth_count {
        for start_g in 0..gate_count {
            let start_idx = start_az * gate_count + start_g;
            if visited[start_idx] || grid[start_idx].is_nan() {
                continue;
            }

            let mut pixels: Vec<Pixel> = Vec::new();
            stack.clear();
            stack.push(start_idx);
            visited[start_idx] = true;

            while let Some(idx) = stack.pop() {
                let az = idx / gate_count;
                let g = idx % gate_count;
                pixels.push((az as u16, g as u16));

                for daz in [-1i32, 0, 1] {
                    let naz = wrap_az(az, daz, azimuth_count);
                    // `az_adjacent[i]` says whether index `i` is spatially
                    // adjacent to `i + 1 (mod)`. Going forward checks the
                    // edge at `az`; going backward checks the edge at
                    // `naz` (which is `az - 1 mod`).
                    if daz > 0 && !az_adjacent[az] {
                        continue;
                    }
                    if daz < 0 && !az_adjacent[naz] {
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
                        let ng = ng_signed as usize;
                        let nidx = naz * gate_count + ng;
                        if visited[nidx] || grid[nidx].is_nan() {
                            continue;
                        }
                        visited[nidx] = true;
                        stack.push(nidx);
                    }
                }
            }

            components.push(pixels);
        }
    }

    components
}

/// `result[i] == true` iff azimuth index `i` is spatially adjacent to
/// index `(i + 1) % az_count` — i.e. the angular gap between those two
/// sorted radials is within `MAX_GAP_FACTOR × median_spacing`.
fn precompute_az_adjacency(azimuths: &[f32], azimuth_count: usize) -> Vec<bool> {
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

fn wrap_az(az: usize, delta: i32, az_count: usize) -> usize {
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
        assert_eq!(adj[0], false);
        assert_eq!(adj[1], false);
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
