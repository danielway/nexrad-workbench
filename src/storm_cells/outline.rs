//! Footprint outlines: trace a component's outer boundary in polar index
//! space, place it on the ground, and simplify it.
//!
//! The boundary is traced along gate edges (a "crack-following" walk over
//! the corners of the azimuth × gate grid), keeping the component on the
//! left and joining pixels that touch only at a corner, to match the
//! 8-connected labeling. Holes are ignored: the outline is the outer ring.
//!
//! Before tracing, the component's radials are unrolled so the azimuth axis
//! is contiguous: the walk starts after the widest run of radials the
//! component does not touch, so a cell straddling 0°/360° is one ring. A
//! component that touches every radial (a rain shield around the radar) is
//! cut at radial 0, which gives a ring with a zero-width slit along that
//! radial; drawn filled or stroked it still covers the right area.

use super::components::Pixel;
use super::model::MAX_OUTLINE_VERTICES;
use super::{DetectionInput, KM_PER_DEG};

/// Simplification tolerance in km.
pub(super) const TOLERANCE_KM: f64 = 1.0;

/// Outline of `pixels` as a closed (lat, lon) ring: first point repeated at
/// the end, at most `MAX_OUTLINE_VERTICES` distinct vertices.
pub(super) fn outline(
    pixels: &[Pixel],
    input: &DetectionInput,
    half_spacing_deg: f64,
) -> Vec<(f64, f64)> {
    if pixels.is_empty() {
        return Vec::new();
    }
    let n = input.azimuth_count;

    // Unroll the azimuth axis.
    let mut occupied = vec![false; n];
    let (mut gmin, mut gmax) = (u16::MAX, 0u16);
    for &(a, g) in pixels {
        occupied[a as usize] = true;
        gmin = gmin.min(g);
        gmax = gmax.max(g);
    }
    let start = unroll_start(&occupied);
    let width = occupied.iter().filter(|o| **o).count();
    let height = (gmax - gmin) as usize + 1;
    let mut mask = vec![false; width * height];
    for &(a, g) in pixels {
        let x = (a as usize + n - start) % n;
        mask[x * height + (g - gmin) as usize] = true;
    }
    let fg = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && (x as usize) < width
            && (y as usize) < height
            && mask[x as usize * height + y as usize]
    };

    // Start at the lowest gate of the first unrolled radial.
    let y0 = (0..height as i32).find(|&y| fg(0, y)).unwrap_or(0);
    let ring = trace(&fg, (0, y0), width, height);

    // Corners → radar-local km (x = east, y = north).
    let az_edge = |x: i32| -> f64 {
        let x = x as usize;
        if x < width {
            input.azimuths[(start + x) % n] as f64 - half_spacing_deg
        } else {
            input.azimuths[(start + width - 1) % n] as f64 + half_spacing_deg
        }
    };
    let km: Vec<(f64, f64)> = ring
        .iter()
        .map(|&(x, y)| {
            let az = az_edge(x).to_radians();
            let r = input.first_gate_km + (gmin as f64 + y as f64) * input.gate_interval_km;
            (r * az.sin(), r * az.cos())
        })
        .collect();

    let simplified = simplify_ring(&km, TOLERANCE_KM, MAX_OUTLINE_VERTICES);
    let cos_lat = input.radar_lat.to_radians().cos().max(1e-6);
    let mut out: Vec<(f64, f64)> = simplified
        .iter()
        .map(|&(x, y)| {
            (
                input.radar_lat + y / KM_PER_DEG,
                input.radar_lon + x / (KM_PER_DEG * cos_lat),
            )
        })
        .collect();
    if let Some(&first) = out.first() {
        out.push(first);
    }
    out
}

/// First radial of the unrolled axis: the one after the longest circular
/// run of unoccupied radials (0 when every radial is occupied).
fn unroll_start(occupied: &[bool]) -> usize {
    let n = occupied.len();
    if occupied.iter().all(|o| *o) {
        return 0;
    }
    let Some(first_occupied) = occupied.iter().position(|o| *o) else {
        return 0;
    };
    // Walk one full turn starting just after an occupied radial, so no run
    // of empty radials is split by the starting point.
    let (mut best_len, mut best_end) = (0, first_occupied);
    let mut run = 0;
    for k in 1..=n {
        let i = (first_occupied + k) % n;
        if occupied[i] {
            run = 0;
        } else {
            run += 1;
            if run > best_len {
                best_len = run;
                best_end = i;
            }
        }
    }
    (best_end + 1) % n
}

/// Walk the outer boundary of the 8-connected pixel set containing pixel
/// `start` (which must be its lowest-y pixel in the lowest-x column, so its
/// left and bottom edges are boundary). Returns every grid corner the walk
/// visits, in order, so arcs keep their curvature once placed on the
/// ground (simplification removes the excess). Pixel `(x, y)` covers
/// `[x, x+1] × [y, y+1]`.
pub(super) fn trace(
    fg: &dyn Fn(i32, i32) -> bool,
    start: (i32, i32),
    width: usize,
    height: usize,
) -> Vec<(i32, i32)> {
    let left = |(dx, dy): (i32, i32)| (-dy, dx);
    let right = |(dx, dy): (i32, i32)| (dy, -dx);
    // Pixel ahead of vertex `v` on side `side` (left or right) of `d`.
    let ahead = |v: (i32, i32), d: (i32, i32), side: (i32, i32)| {
        let o = (d.0 + side.0, d.1 + side.1);
        (v.0 + (o.0 - 1).div_euclid(2), v.1 + (o.1 - 1).div_euclid(2))
    };

    let origin = start;
    let mut verts = vec![origin];
    let mut v = origin;
    let mut d = (1, 0); // along the bottom edge, component on the left
    let limit = 4 * (width + 2) * (height + 2);
    for _ in 0..limit {
        v = (v.0 + d.0, v.1 + d.1);
        if v == origin {
            break;
        }
        let ra = ahead(v, d, right(d));
        let la = ahead(v, d, left(d));
        let nd = if fg(ra.0, ra.1) {
            right(d) // 8-connectivity: follow the diagonal neighbour
        } else if fg(la.0, la.1) {
            d
        } else {
            left(d)
        };
        verts.push(v);
        d = nd;
    }
    verts
}

/// Douglas-Peucker on a closed ring (no repeated closing point), loosening
/// the tolerance until at most `max_vertices` remain and tightening it
/// until at least four do.
pub(super) fn simplify_ring(
    ring: &[(f64, f64)],
    tolerance: f64,
    max_vertices: usize,
) -> Vec<(f64, f64)> {
    if ring.len() <= 3 {
        return ring.to_vec();
    }
    // Split the ring at its first point and the point farthest from it.
    let far = (1..ring.len())
        .max_by(|&a, &b| dist2(ring[0], ring[a]).total_cmp(&dist2(ring[0], ring[b])))
        .unwrap_or(1);
    let run = |tol: f64| {
        let mut keep = vec![false; ring.len() + 1];
        keep[0] = true;
        keep[far] = true;
        keep[ring.len()] = true;
        // Second half wraps back to index 0, appended as index `len`.
        let mut closed: Vec<(f64, f64)> = ring.to_vec();
        closed.push(ring[0]);
        dp(&closed, 0, far, tol, &mut keep);
        dp(&closed, far, ring.len(), tol, &mut keep);
        ring.iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(p, _)| *p)
            .collect::<Vec<_>>()
    };
    // A footprint narrower than the tolerance would collapse to a line:
    // tighten until it keeps at least four vertices.
    let mut tol = tolerance.max(1e-9);
    let mut out = run(tol);
    while out.len() < 4 && tol > 1e-6 {
        tol *= 0.5;
        out = run(tol);
    }
    while out.len() > max_vertices.max(4) {
        tol *= 1.5;
        out = run(tol);
    }
    out
}

fn dp(points: &[(f64, f64)], first: usize, last: usize, tol: f64, keep: &mut [bool]) {
    if last <= first + 1 {
        return;
    }
    let (a, b) = (points[first], points[last]);
    let (mut worst, mut worst_d) = (first, -1.0);
    for (i, &p) in points.iter().enumerate().take(last).skip(first + 1) {
        let d = seg_dist(p, a, b);
        if d > worst_d {
            worst = i;
            worst_d = d;
        }
    }
    if worst_d > tol {
        keep[worst] = true;
        dp(points, first, worst, tol, keep);
        dp(points, worst, last, tol, keep);
    }
}

fn dist2(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

/// Distance from `p` to segment `ab`.
fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist2(p, (a.0 + t * dx, a.1 + t * dy)).sqrt()
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn trace_set(pixels: &[(i32, i32)]) -> Vec<(i32, i32)> {
        let w = pixels.iter().map(|p| p.0).max().unwrap() as usize + 1;
        let h = pixels.iter().map(|p| p.1).max().unwrap() as usize + 1;
        let fg = |x: i32, y: i32| pixels.contains(&(x, y));
        let x0 = pixels.iter().map(|p| p.0).min().unwrap();
        let y0 = pixels
            .iter()
            .filter(|p| p.0 == x0)
            .map(|p| p.1)
            .min()
            .unwrap();
        corners(&trace(&fg, (x0, y0), w, h))
    }

    /// Keep only the vertices where the walk turns.
    fn corners(ring: &[(i32, i32)]) -> Vec<(i32, i32)> {
        let n = ring.len();
        (0..n)
            .filter(|&i| {
                let (p, c, q) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
                (c.0 - p.0, c.1 - p.1) != (q.0 - c.0, q.1 - c.1)
            })
            .map(|i| ring[i])
            .collect()
    }

    #[wasm_bindgen_test]
    fn single_pixel_traces_its_four_corners() {
        assert_eq!(trace_set(&[(0, 0)]), vec![(0, 0), (1, 0), (1, 1), (0, 1)]);
    }

    #[wasm_bindgen_test]
    fn l_shape_has_six_corners() {
        let ring = trace_set(&[(0, 0), (1, 0), (0, 1)]);
        assert_eq!(ring, vec![(0, 0), (2, 0), (2, 1), (1, 1), (1, 2), (0, 2)]);
    }

    #[wasm_bindgen_test]
    fn diagonal_neighbours_are_one_outline() {
        // (0,0) and (1,1) touch at a corner: 8-connected → one ring that
        // goes around both, passing the shared corner twice.
        let ring = trace_set(&[(0, 0), (1, 1)]);
        assert!(ring.contains(&(2, 2)), "{ring:?}");
        assert_eq!(ring.iter().filter(|v| **v == (1, 1)).count(), 2, "{ring:?}");
    }

    #[wasm_bindgen_test]
    fn holes_are_ignored() {
        // A 3×3 ring of pixels with the centre missing → the outer square.
        let mut px = Vec::new();
        for x in 0..3 {
            for y in 0..3 {
                if (x, y) != (1, 1) {
                    px.push((x, y));
                }
            }
        }
        assert_eq!(trace_set(&px), vec![(0, 0), (3, 0), (3, 3), (0, 3)]);
    }

    #[wasm_bindgen_test]
    fn unroll_starts_after_the_widest_gap() {
        // Occupied radials 6,7,0,1 of 8: the gap is 2..=5 → start at 6.
        let occ = [true, true, false, false, false, false, true, true];
        assert_eq!(unroll_start(&occ), 6);
        assert_eq!(unroll_start(&[true; 4]), 0);
        assert_eq!(unroll_start(&[false, true, true, false]), 1);
        // The widest gap wraps through index 0 (radials 2, 3, 0).
        assert_eq!(unroll_start(&[false, true, false, false]), 1);
    }

    #[wasm_bindgen_test]
    fn simplify_keeps_corners_and_drops_collinear_points() {
        let square = vec![
            (0.0, 0.0),
            (5.0, 0.0),
            (10.0, 0.0),
            (10.0, 5.0),
            (10.0, 10.0),
            (5.0, 10.0),
            (0.0, 10.0),
            (0.0, 5.0),
        ];
        let s = simplify_ring(&square, 0.5, 64);
        assert_eq!(s, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
    }

    #[wasm_bindgen_test]
    fn simplify_keeps_thin_footprints_as_polygons() {
        // 0.2 km wide, 3 km long: under a 1 km tolerance it would become a
        // line; the floor keeps a quadrilateral.
        let thin = vec![
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (3.0, 0.0),
            (3.0, 0.2),
            (0.0, 0.2),
        ];
        assert_eq!(simplify_ring(&thin, 1.0, 64).len(), 4);
    }

    #[wasm_bindgen_test]
    fn simplify_caps_the_vertex_count() {
        // A 400-point circle of radius 50 km.
        let circle: Vec<(f64, f64)> = (0..400)
            .map(|i| {
                let t = i as f64 / 400.0 * std::f64::consts::TAU;
                (50.0 * t.cos(), 50.0 * t.sin())
            })
            .collect();
        let s = simplify_ring(&circle, 0.01, 64);
        assert!(s.len() <= 64 && s.len() >= 16, "{}", s.len());
    }

    fn input<'a>(az: &'a [f32]) -> DetectionInput<'a> {
        DetectionInput {
            azimuths: az,
            gate_values: &[],
            azimuth_count: az.len(),
            gate_count: 100,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            data_scale: 0.0,
            data_offset: 0.0,
            radar_lat: 35.0,
            radar_lon: -97.0,
        }
    }

    #[wasm_bindgen_test]
    fn trace_visits_every_boundary_corner() {
        let fg = |x: i32, y: i32| (0..3).contains(&x) && y == 0;
        assert_eq!(trace(&fg, (0, 0), 3, 1).len(), 8); // perimeter of 3×1
    }

    #[wasm_bindgen_test]
    fn wide_arcs_keep_their_curvature() {
        // 90° of radials at 100 km: the outer edge is an arc, which must not
        // collapse to a chord (the chord would sit 29 km inside the arc).
        let az: Vec<f32> = (0..360).map(|i| i as f32).collect();
        let px: Vec<Pixel> = (0..90u16)
            .flat_map(|a| (95..100u16).map(move |g| (a, g)))
            .collect();
        let ring = outline(&px, &input(&az), 0.5);
        // Shoelace area in km² against the annular sector's 765.8 km²
        // (a chord in place of the 100 km arc would lose most of it).
        let cos_lat = 35f64.to_radians().cos();
        let km: Vec<(f64, f64)> = ring
            .iter()
            .map(|p| {
                (
                    (p.1 + 97.0) * KM_PER_DEG * cos_lat,
                    (p.0 - 35.0) * KM_PER_DEG,
                )
            })
            .collect();
        let area: f64 = km
            .windows(2)
            .map(|w| w[0].0 * w[1].1 - w[1].0 * w[0].1)
            .sum::<f64>()
            .abs()
            / 2.0;
        assert!((area - 765.8).abs() < 77.0, "area {area}");
    }

    #[wasm_bindgen_test]
    fn outline_is_a_closed_ring_around_the_gates() {
        // 360 one-degree radials; a block on radials 359, 0, 1 at 50-59 km.
        let az: Vec<f32> = (0..360).map(|i| i as f32).collect();
        let mut px = Vec::new();
        for a in [359u16, 0, 1] {
            for g in 50..60u16 {
                px.push((a, g));
            }
        }
        let ring = outline(&px, &input(&az), 0.5);
        assert_eq!(ring.first(), ring.last());
        // Four corners of the annular sector (straight in 1 km tolerance).
        assert_eq!(ring.len(), 5, "{ring:?}");
        // All north of the radar, straddling its longitude.
        assert!(ring.iter().all(|p| p.0 > 35.4 && p.0 < 35.6));
        assert!(ring.iter().any(|p| p.1 < -97.0) && ring.iter().any(|p| p.1 > -97.0));
    }

    #[wasm_bindgen_test]
    fn full_circle_component_is_traced_without_panicking() {
        let az: Vec<f32> = (0..36).map(|i| i as f32 * 10.0).collect();
        let px: Vec<Pixel> = (0..36u16)
            .flat_map(|a| (0..20u16).map(move |g| (a, g)))
            .collect();
        let ring = outline(&px, &input(&az), 5.0);
        assert!(ring.len() >= 4 && ring.len() <= MAX_OUTLINE_VERTICES + 1);
        // Reaches out to about 20 km in every direction.
        let max_lat = ring.iter().map(|p| p.0).fold(f64::MIN, f64::max);
        assert!((max_lat - (35.0 + 20.0 / KM_PER_DEG)).abs() < 0.05);
    }
}
