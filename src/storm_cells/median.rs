//! 3×3 median filter on the physical (dBZ) polar grid.
//!
//! Runs before segmentation so single-gate spikes cannot set a cell's peak
//! (the study's 73 dBZ label on Milton's rain shield came from one gate) and
//! one-gate slivers along data gaps close up. The window is 3 radials × 3
//! gates in index space, wrapping in azimuth only where the radials are
//! adjacent (the same rule the component labeling uses).
//!
//! Background (`NaN`) gates stay background and never contribute. A valid
//! gate is replaced by the median of the valid values in its window only when
//! at least [`MIN_VALID`] of the nine are valid (itself included); otherwise
//! it keeps its own value, so isolated echo at the edge of a data gap is not
//! dragged towards whatever few neighbours it has.

use super::components::wrap_az;

/// Valid gates (including the centre) a window needs before it is filtered.
pub(super) const MIN_VALID: usize = 5;

/// Median-filter `grid` (`azimuth_count × gate_count`, row-major by
/// azimuth). `az_adjacent[i]` says whether radial `i` neighbours radial
/// `i + 1 (mod n)`.
///
/// Only filtered values at or above `floor` are kept; everything below it is
/// written as `NaN` (the caller thresholds at `floor` anyway), which lets
/// most gates skip the sort.
pub(super) fn median3x3(
    grid: &[f32],
    az_adjacent: &[bool],
    gate_count: usize,
    floor: f32,
) -> Vec<f32> {
    let azimuth_count = az_adjacent.len();
    debug_assert_eq!(grid.len(), azimuth_count * gate_count);
    let mut out = vec![f32::NAN; grid.len()];
    if gate_count == 0 {
        return out;
    }

    // A gate can come out at or above `floor` only if its window holds at
    // least one gate at or above `floor`. So only the 3×3 neighbourhoods of
    // those "hot" gates are filtered; in clear air or light rain that skips
    // nearly the whole sweep.
    let neighbours = |az: usize| {
        let prev = (azimuth_count > 1 && az_adjacent[wrap_az(az, -1, azimuth_count)])
            .then(|| wrap_az(az, -1, azimuth_count));
        let next = (azimuth_count > 1 && az_adjacent[az]).then(|| wrap_az(az, 1, azimuth_count));
        [prev, Some(az), next]
    };
    // `near_hot[i]`: some gate in i's 3×3 window is hot. Built with a
    // gate-direction dilation per row, then an azimuth-direction one.
    let mut row_hot = vec![false; grid.len()];
    for row in 0..azimuth_count {
        let base = row * gate_count;
        for g in 0..gate_count {
            if grid[base + g] >= floor {
                row_hot[base + g.saturating_sub(1)..=base + (g + 1).min(gate_count - 1)].fill(true);
            }
        }
    }
    let mut near_hot = vec![false; grid.len()];
    for az in 0..azimuth_count {
        let out_row = &mut near_hot[az * gate_count..(az + 1) * gate_count];
        for row in neighbours(az).into_iter().flatten() {
            let src = &row_hot[row * gate_count..(row + 1) * gate_count];
            for (o, h) in out_row.iter_mut().zip(src) {
                *o |= *h;
            }
        }
    }
    drop(row_hot);

    let mut window = [0.0f32; 9];
    for (idx, &centre) in grid.iter().enumerate() {
        if !near_hot[idx] || centre.is_nan() {
            continue;
        }
        let (az, g) = (idx / gate_count, idx % gate_count);
        let lo = g.saturating_sub(1);
        let hi = (g + 1).min(gate_count - 1);
        let (mut n, mut hot) = (0usize, 0usize);
        for row in neighbours(az).into_iter().flatten() {
            for v in &grid[row * gate_count + lo..=row * gate_count + hi] {
                if !v.is_nan() {
                    window[n] = *v;
                    n += 1;
                    hot += usize::from(*v >= floor);
                }
            }
        }
        let value = if n < MIN_VALID {
            // Too sparse to filter: the gate keeps its own value.
            centre
        } else if 2 * hot < n {
            // Less than half the window reaches `floor`, so the median
            // cannot either.
            continue;
        } else {
            median(&mut window[..n])
        };
        if value >= floor {
            out[idx] = value;
        }
    }
    out
}

/// Median of a small slice (mean of the two middle values for even
/// length). Insertion sort: windows hold at most nine values, none NaN.
fn median(values: &mut [f32]) -> f32 {
    for i in 1..values.len() {
        let v = values[i];
        let mut j = i;
        while j > 0 && values[j - 1] > v {
            values[j] = values[j - 1];
            j -= 1;
        }
        values[j] = v;
    }
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const N: f32 = f32::NAN;

    fn all_adjacent(n: usize) -> Vec<bool> {
        vec![true; n]
    }

    #[wasm_bindgen_test]
    fn single_gate_spike_is_removed() {
        // 3×3 of 40 with a 73 spike in the middle → median 40.
        let mut grid = vec![40.0; 9];
        grid[4] = 73.0;
        let out = median3x3(&grid, &all_adjacent(3), 3, f32::NEG_INFINITY);
        assert_eq!(out[4], 40.0);
    }

    #[wasm_bindgen_test]
    fn background_stays_background() {
        let mut grid = vec![40.0; 9];
        grid[4] = N;
        let out = median3x3(&grid, &all_adjacent(3), 3, f32::NEG_INFINITY);
        assert!(out[4].is_nan());
    }

    #[wasm_bindgen_test]
    fn sparse_window_keeps_its_own_value() {
        // Centre plus three valid neighbours = 4 < MIN_VALID → unchanged.
        let grid = vec![
            10.0, N, N, //
            20.0, 60.0, N, //
            30.0, N, N,
        ];
        let out = median3x3(&grid, &[false, false, false], 3, f32::NEG_INFINITY);
        assert_eq!(out[4], 60.0);
    }

    #[wasm_bindgen_test]
    fn even_count_takes_mean_of_middle_pair() {
        // Six valid values in the centre's window: 10,20,30,40,50,60 → 35.
        let grid = vec![
            10.0, 20.0, N, //
            30.0, 40.0, N, //
            50.0, 60.0, N,
        ];
        // Only radial adjacency within the 3-row block (no seam wrap).
        let out = median3x3(&grid, &[true, true, false], 3, f32::NEG_INFINITY);
        assert_eq!(out[3], 35.0);
    }

    #[wasm_bindgen_test]
    fn window_wraps_across_the_seam_only_when_adjacent() {
        // Four radials × one gate. Radial 0 is a spike; its neighbours are
        // radial 3 (via the seam) and radial 1.
        let grid = vec![70.0, 30.0, 30.0, 30.0];
        // With 1 gate a window has at most 3 values (< MIN_VALID) → unchanged.
        let out = median3x3(&grid, &all_adjacent(4), 1, f32::NEG_INFINITY);
        assert_eq!(out[0], 70.0);
        // Three gates: the seam brings radial 3 into radial 0's window.
        let mut grid3 = vec![30.0; 12];
        grid3[1] = 70.0; // radial 0, gate 1
        let wrapped = median3x3(&grid3, &all_adjacent(4), 3, f32::NEG_INFINITY);
        assert_eq!(wrapped[1], 30.0);
        // No seam: radial 0 sees only itself and radial 1 (6 values, three of
        // them 30 and ... still the median of [30,30,30,30,30,70] = 30).
        let no_seam = median3x3(&grid3, &[true, true, true, false], 3, f32::NEG_INFINITY);
        assert_eq!(no_seam[1], 30.0);
    }

    #[wasm_bindgen_test]
    fn values_below_the_floor_are_dropped() {
        let grid = vec![20.0; 9];
        let out = median3x3(&grid, &all_adjacent(3), 3, 35.0);
        assert!(out.iter().all(|v| v.is_nan()));
        // A 40 dBZ spike in 20 dBZ echo filters to 20 → dropped too.
        let mut spike = vec![20.0; 9];
        spike[4] = 40.0;
        let out = median3x3(&spike, &all_adjacent(3), 3, 35.0);
        assert!(out[4].is_nan());
    }

    #[wasm_bindgen_test]
    fn floor_does_not_change_kept_values() {
        // Mixed window: the fast path must agree with the plain median.
        let grid = vec![
            30.0, 40.0, 50.0, //
            36.0, 38.0, 45.0, //
            20.0, 60.0, 35.0,
        ];
        let plain = median3x3(&grid, &all_adjacent(3), 3, f32::NEG_INFINITY);
        let floored = median3x3(&grid, &all_adjacent(3), 3, 35.0);
        for (p, f) in plain.iter().zip(&floored) {
            if *p >= 35.0 {
                assert_eq!(p, f);
            } else {
                assert!(f.is_nan());
            }
        }
    }
}
