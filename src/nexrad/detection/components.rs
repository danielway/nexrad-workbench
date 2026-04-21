//! Connected-component labeling on a polar (azimuth × gate) grid.
//!
//! 8-neighborhood, with the azimuth axis treated as circular so that a cell
//! straddling the 0°/360° seam is still a single component. Iterative
//! flood-fill (explicit `Vec` stack) — no recursion, no allocation per
//! pixel after the initial capacity is reserved.

/// One pixel belonging to a component: (azimuth index, gate index).
pub(super) type Pixel = (u16, u16);

/// Label connected components over a grid of above-threshold physical
/// values. Gates with `NaN` are considered background.
///
/// `threshold_dbz` is accepted for clarity but not used directly — the
/// caller is expected to have masked out below-threshold gates as NaN
/// already. It is retained in case future heuristics (e.g. hysteresis
/// thresholding) want to read it.
pub(super) fn label(
    grid: &[f32],
    azimuth_count: usize,
    gate_count: usize,
    _threshold_dbz: f32,
) -> Vec<Vec<Pixel>> {
    if azimuth_count == 0 || gate_count == 0 {
        return Vec::new();
    }

    let n = azimuth_count * gate_count;
    debug_assert_eq!(grid.len(), n);

    let mut visited = vec![false; n];
    let mut components: Vec<Vec<Pixel>> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    for start_az in 0..azimuth_count {
        for start_g in 0..gate_count {
            let start_idx = start_az * gate_count + start_g;
            if visited[start_idx] || grid[start_idx].is_nan() {
                continue;
            }

            // BFS/DFS over this component.
            let mut pixels: Vec<Pixel> = Vec::new();
            stack.clear();
            stack.push(start_idx);
            visited[start_idx] = true;

            while let Some(idx) = stack.pop() {
                let az = idx / gate_count;
                let g = idx % gate_count;
                pixels.push((az as u16, g as u16));

                // 8-neighborhood. Azimuth wraps; gate does not.
                for daz in [-1i32, 0, 1] {
                    let naz = wrap_az(az, daz, azimuth_count);
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

fn wrap_az(az: usize, delta: i32, az_count: usize) -> usize {
    let n = az_count as i32;
    let v = az as i32 + delta;
    (((v % n) + n) % n) as usize
}
