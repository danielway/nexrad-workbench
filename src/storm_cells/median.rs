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
    raw: &[f32],
    decoder: &Decoder,
    row_valid: &[bool],
    az_adjacent: &[bool],
    gate_count: usize,
    floor: f32,
) -> Vec<f32> {
    let azimuth_count = az_adjacent.len();
    debug_assert_eq!(raw.len(), azimuth_count * gate_count);
    debug_assert_eq!(row_valid.len(), azimuth_count);
    let mut out = vec![f32::NAN; raw.len()];
    if gate_count == 0 {
        return out;
    }
    // Physical value of a gate, NaN for no echo. Decoded on the fly rather
    // than into a second full-size grid: most gates are never looked at.
    let value = |r: usize, g: usize| -> f32 {
        if row_valid[r] {
            decoder.decode(raw[r * gate_count + g])
        } else {
            f32::NAN
        }
    };

    // A gate can come out at or above `floor` only if its window holds at
    // least one gate at or above `floor`, so only gates next to such "hot"
    // gates are filtered; in clear air or light rain that skips nearly the
    // whole sweep. `row_hot[i]`: gate i or a gate beside it on its radial is
    // hot; OR-ing three radials gives the 3×3 test. `span[r]` bounds the
    // flagged gates of radial r, so the main loop only visits the stretch
    // of each radial that can produce output.
    let mut row_hot = vec![false; raw.len()];
    let mut span: Vec<Option<(usize, usize)>> = vec![None; azimuth_count];
    for r in 0..azimuth_count {
        if !row_valid[r] {
            continue;
        }
        let row_out = &mut row_hot[r * gate_count..(r + 1) * gate_count];
        for g in 0..gate_count {
            if value(r, g) >= floor {
                let lo = g.saturating_sub(1);
                let hi = (g + 1).min(gate_count - 1);
                row_out[lo..=hi].fill(true);
                span[r] = Some(span[r].map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
            }
        }
    }

    let hot = |r: usize| &row_hot[r * gate_count..(r + 1) * gate_count];
    let mut window = [0.0f32; 9];
    for az in 0..azimuth_count {
        if !row_valid[az] {
            continue;
        }
        let prev = wrap_az(az, -1, azimuth_count);
        let next = wrap_az(az, 1, azimuth_count);
        let has_prev = azimuth_count > 1 && az_adjacent[prev];
        let has_next = azimuth_count > 1 && az_adjacent[az];
        let rows = [has_prev.then_some(prev), Some(az), has_next.then_some(next)];
        let Some((g_lo, g_hi)) = rows
            .into_iter()
            .flatten()
            .filter_map(|r| span[r])
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
        else {
            continue;
        };
        // Neighbouring radials' flags, or this radial's again when there is
        // no neighbour (already counted, so harmless).
        let (hp, hc, hn) = (
            hot(if has_prev { prev } else { az }),
            hot(az),
            hot(if has_next { next } else { az }),
        );
        let out_row = &mut out[az * gate_count..(az + 1) * gate_count];

        for g in g_lo..=g_hi {
            if !(hp[g] | hc[g] | hn[g]) {
                continue;
            }
            let centre = value(az, g);
            if centre.is_nan() {
                continue;
            }
            let lo = g.saturating_sub(1);
            let hi = (g + 1).min(gate_count - 1);
            let (mut n, mut n_hot) = (0usize, 0usize);
            for r in rows.into_iter().flatten() {
                for ng in lo..=hi {
                    let v = value(r, ng);
                    if !v.is_nan() {
                        window[n] = v;
                        n += 1;
                        n_hot += usize::from(v >= floor);
                    }
                }
            }
            let filtered = if n < MIN_VALID {
                // Too sparse to filter: the gate keeps its own value.
                centre
            } else if 2 * n_hot < n {
                // Less than half the window reaches `floor`, so the median
                // cannot either.
                continue;
            } else {
                median(&mut window[..n])
            };
            if filtered >= floor {
                out_row[g] = filtered;
            }
        }
    }
    out
}

/// Raw gate code → physical value.
#[derive(Clone, Copy, Debug)]
pub(super) struct Decoder {
    /// `physical = (raw - offset) / scale`; `scale == 0` means the raw value
    /// is already physical.
    pub scale: f32,
    pub offset: f32,
}

impl Decoder {
    /// Physical value, or NaN for the sentinels (raw 0 = below threshold,
    /// 1 = range folded).
    #[inline]
    pub(super) fn decode(&self, raw: f32) -> f32 {
        if raw <= 1.0 {
            f32::NAN
        } else if self.scale == 0.0 {
            raw
        } else {
            (raw - self.offset) / self.scale
        }
    }
}

/// Median of a small slice (mean of the two middle values for even
/// length). Windows hold at most nine values, none NaN.
fn median(values: &mut [f32]) -> f32 {
    if let Ok(nine) = <&mut [f32; 9]>::try_from(&mut *values) {
        return median9(nine);
    }
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

/// Median of nine with the classic 19-exchange network (full windows are by
/// far the common case inside echo).
fn median9(p: &mut [f32; 9]) -> f32 {
    let mut s = |a: usize, b: usize| {
        if p[a] > p[b] {
            p.swap(a, b);
        }
    };
    s(1, 2);
    s(4, 5);
    s(7, 8);
    s(0, 1);
    s(3, 4);
    s(6, 7);
    s(1, 2);
    s(4, 5);
    s(7, 8);
    s(0, 3);
    s(5, 8);
    s(4, 7);
    s(3, 6);
    s(1, 4);
    s(2, 5);
    s(4, 7);
    s(4, 2);
    s(6, 4);
    s(4, 2);
    p[4]
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const N: f32 = f32::NAN;

    fn all_adjacent(n: usize) -> Vec<bool> {
        vec![true; n]
    }

    /// Median over an already-physical grid (NaN = no echo), all rows valid.
    fn m(grid: &[f32], adj: &[bool], gate_count: usize, floor: f32) -> Vec<f32> {
        let identity = Decoder {
            scale: 0.0,
            offset: 0.0,
        };
        median3x3(
            grid,
            &identity,
            &vec![true; adj.len()],
            adj,
            gate_count,
            floor,
        )
    }

    #[wasm_bindgen_test]
    fn decoder_maps_sentinels_to_nan_and_applies_scale() {
        let d = Decoder {
            scale: 2.0,
            offset: 66.0,
        };
        assert!(d.decode(0.0).is_nan());
        assert!(d.decode(1.0).is_nan());
        assert!((d.decode(146.0) - 40.0).abs() < 1e-6);
        let raw = Decoder {
            scale: 0.0,
            offset: 0.0,
        };
        assert_eq!(raw.decode(45.0), 45.0);
        assert!(raw.decode(f32::NAN).is_nan());
    }

    #[wasm_bindgen_test]
    fn invalid_rows_are_background() {
        let grid = vec![50.0; 9];
        let identity = Decoder {
            scale: 0.0,
            offset: 0.0,
        };
        let out = median3x3(
            &grid,
            &identity,
            &[true, false, true],
            &all_adjacent(3),
            3,
            0.0,
        );
        assert!(out[3..6].iter().all(|v| v.is_nan()));
        assert_eq!(out[0], 50.0);
    }

    #[wasm_bindgen_test]
    fn median9_matches_a_sort_for_every_rotation() {
        let base = [7.0, 3.0, 9.0, 1.0, 5.0, 8.0, 2.0, 6.0, 4.0];
        for k in 0..9 {
            let mut v = base;
            v.rotate_left(k);
            assert_eq!(median9(&mut v), 5.0);
        }
        let mut dup = [40.0, 40.0, 73.0, 40.0, 10.0, 40.0, 55.0, 40.0, 40.0];
        assert_eq!(median9(&mut dup), 40.0);
        // Pseudo-random windows against a full sort.
        let mut seed = 12345u32;
        for _ in 0..2000 {
            let mut v = [0.0f32; 9];
            for x in &mut v {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                *x = ((seed >> 16) % 40) as f32 * 0.5;
            }
            let mut sorted = v;
            sorted.sort_by(f32::total_cmp);
            assert_eq!(median9(&mut v), sorted[4]);
        }
    }

    #[wasm_bindgen_test]
    fn single_gate_spike_is_removed() {
        // 3×3 of 40 with a 73 spike in the middle → median 40.
        let mut grid = vec![40.0; 9];
        grid[4] = 73.0;
        let out = m(&grid, &all_adjacent(3), 3, f32::NEG_INFINITY);
        assert_eq!(out[4], 40.0);
    }

    #[wasm_bindgen_test]
    fn background_stays_background() {
        let mut grid = vec![40.0; 9];
        grid[4] = N;
        let out = m(&grid, &all_adjacent(3), 3, f32::NEG_INFINITY);
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
        let out = m(&grid, &[false, false, false], 3, f32::NEG_INFINITY);
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
        let out = m(&grid, &[true, true, false], 3, f32::NEG_INFINITY);
        assert_eq!(out[3], 35.0);
    }

    #[wasm_bindgen_test]
    fn window_wraps_across_the_seam_only_when_adjacent() {
        // Four radials × one gate. Radial 0 is a spike; its neighbours are
        // radial 3 (via the seam) and radial 1.
        let grid = vec![70.0, 30.0, 30.0, 30.0];
        // With 1 gate a window has at most 3 values (< MIN_VALID) → unchanged.
        let out = m(&grid, &all_adjacent(4), 1, f32::NEG_INFINITY);
        assert_eq!(out[0], 70.0);
        // Three gates: the seam brings radial 3 into radial 0's window.
        let mut grid3 = vec![30.0; 12];
        grid3[1] = 70.0; // radial 0, gate 1
        let wrapped = m(&grid3, &all_adjacent(4), 3, f32::NEG_INFINITY);
        assert_eq!(wrapped[1], 30.0);
        // No seam: radial 0 sees only itself and radial 1 (6 values, three of
        // them 30 and ... still the median of [30,30,30,30,30,70] = 30).
        let no_seam = m(&grid3, &[true, true, true, false], 3, f32::NEG_INFINITY);
        assert_eq!(no_seam[1], 30.0);
    }

    #[wasm_bindgen_test]
    fn values_below_the_floor_are_dropped() {
        let grid = vec![20.0; 9];
        let out = m(&grid, &all_adjacent(3), 3, 35.0);
        assert!(out.iter().all(|v| v.is_nan()));
        // A 40 dBZ spike in 20 dBZ echo filters to 20 → dropped too.
        let mut spike = vec![20.0; 9];
        spike[4] = 40.0;
        let out = m(&spike, &all_adjacent(3), 3, 35.0);
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
        let plain = m(&grid, &all_adjacent(3), 3, f32::NEG_INFINITY);
        let floored = m(&grid, &all_adjacent(3), 3, 35.0);
        for (p, f) in plain.iter().zip(&floored) {
            if *p >= 35.0 {
                assert_eq!(p, f);
            } else {
                assert!(f.is_nan());
            }
        }
    }
}
