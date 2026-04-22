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
