//! Per-cell feature extraction.
//!
//! Takes a component (a list of (row, column) pixels) and computes the
//! fields the UI/state layer needs: reflectivity-weighted centroid in
//! lat/lon, lat/lon bounding box, physical area in km², max/mean dBZ,
//! bearing and range from the local origin (the radar on a polar sweep),
//! and a 2×2 PCA for major-axis orientation + elongation. Positions and
//! areas come from the grid's geometry (`geometry::Frame`).

use super::components::Pixel;
use super::geometry::{Frame, Grid};

/// Summary of one component's gates.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Footprint {
    pub lat: f64,
    pub lon: f64,
    pub max_dbz: f32,
    pub mean_dbz: f32,
    pub area_km2: f32,
    pub bounds: (f64, f64, f64, f64),
    pub bearing_from_radar_deg: f32,
    pub range_from_radar_km: f32,
    pub orientation_deg: f32,
    pub elongation: f32,
    pub gate_count: u32,
}

pub(super) fn summarize<'a>(
    pixels: &[Pixel],
    grid: &[f32],
    input: impl Into<Grid<'a>>,
    edge_threshold_dbz: f32,
) -> Footprint {
    let geometry: Grid<'a> = input.into();
    let cols = geometry.shape().1;
    let frame = Frame::new(geometry);

    let mut max_dbz = f32::NEG_INFINITY;
    let mut sum_dbz = 0.0_f64;
    let mut area_km2 = 0.0_f64;

    // Weighted centroid and covariance accumulators in local Cartesian
    // (km): x = east, y = north.
    let mut sum_w = 0.0_f64;
    let mut sum_wx = 0.0_f64;
    let mut sum_wy = 0.0_f64;
    let mut sum_wxx = 0.0_f64;
    let mut sum_wyy = 0.0_f64;
    let mut sum_wxy = 0.0_f64;

    // Lat/lon bounds tracked from each pixel's own geographic position.
    let mut min_lat = f64::INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    let mut min_lon = f64::INFINITY;
    let mut max_lon = f64::NEG_INFINITY;

    for &(row, col) in pixels {
        let idx = row as usize * cols + col as usize;
        let dbz = grid[idx];
        if dbz.is_nan() {
            continue;
        }

        let (x_km, y_km, pixel_area_km2) = frame.pixel(row, col);
        area_km2 += pixel_area_km2;

        // Weight = reflectivity above the edge threshold. Linear-Z would be
        // more physical (10^(dbz/10)), but this dampens extreme cores that
        // would otherwise dominate the centroid.
        let w = (dbz - edge_threshold_dbz).max(0.0) as f64 + 0.001;
        sum_w += w;
        sum_wx += w * x_km;
        sum_wy += w * y_km;
        sum_wxx += w * x_km * x_km;
        sum_wyy += w * y_km * y_km;
        sum_wxy += w * x_km * y_km;

        if dbz > max_dbz {
            max_dbz = dbz;
        }
        sum_dbz += dbz as f64;

        // Per-pixel lat/lon for the bounding box.
        let (lat, lon) = frame.lat_lon(x_km, y_km);
        if lat < min_lat {
            min_lat = lat;
        }
        if lat > max_lat {
            max_lat = lat;
        }
        if lon < min_lon {
            min_lon = lon;
        }
        if lon > max_lon {
            max_lon = lon;
        }
    }

    let mean_dbz = if pixels.is_empty() {
        0.0
    } else {
        (sum_dbz / pixels.len() as f64) as f32
    };

    // Weighted centroid in local Cartesian, then to lat/lon.
    let inv_w = if sum_w > 0.0 { 1.0 / sum_w } else { 0.0 };
    let cx_km = sum_wx * inv_w;
    let cy_km = sum_wy * inv_w;
    let (centroid_lat, centroid_lon) = frame.lat_lon(cx_km, cy_km);

    let range_from_radar_km = (cx_km * cx_km + cy_km * cy_km).sqrt() as f32;
    let bearing_from_radar_deg = ((cx_km.atan2(cy_km).to_degrees() + 360.0) % 360.0) as f32;

    // Covariance around the centroid, derived from the "around origin"
    // accumulators: E[xx] - E[x]^2, etc.
    let (orientation_deg, elongation) = if sum_w > 0.0 {
        let sxx = sum_wxx * inv_w - cx_km * cx_km;
        let syy = sum_wyy * inv_w - cy_km * cy_km;
        let sxy = sum_wxy * inv_w - cx_km * cy_km;
        pca(sxx, syy, sxy)
    } else {
        (0.0, 1.0)
    };

    Footprint {
        lat: centroid_lat,
        lon: centroid_lon,
        max_dbz,
        mean_dbz,
        area_km2: area_km2 as f32,
        bounds: (min_lat, min_lon, max_lat, max_lon),
        bearing_from_radar_deg,
        range_from_radar_km,
        orientation_deg,
        elongation,
        gate_count: pixels.len() as u32,
    }
}

/// Principal-axis orientation (compass degrees in [0, 180)) and elongation
/// (√(λ_major / λ_minor)) from a 2×2 covariance matrix expressed in
/// radar-local Cartesian coords (x = east, y = north).
fn pca(sxx: f64, syy: f64, sxy: f64) -> (f32, f32) {
    let trace = sxx + syy;
    let det = sxx * syy - sxy * sxy;
    let disc = (trace * trace - 4.0 * det).max(0.0).sqrt();
    let lambda_major = (trace + disc) * 0.5;
    let lambda_minor = (trace - disc) * 0.5;

    // Eigenvector of the larger eigenvalue: angle from +x axis.
    let math_angle_rad = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    // Compass heading: 0° = north, clockwise. Orientation is axis-only,
    // so fold into [0, 180).
    let mut compass_deg = 90.0 - math_angle_rad.to_degrees();
    compass_deg = ((compass_deg % 180.0) + 180.0) % 180.0;

    let elongation = if lambda_minor > 1e-9 {
        (lambda_major / lambda_minor).sqrt() as f32
    } else {
        1.0
    };

    (compass_deg as f32, elongation)
}

#[cfg(test)]
mod coverage_tests {
    use super::super::DetectionInput;
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // -- pca ------------------------------------------------------------

    #[wasm_bindgen_test]
    fn pca_isotropic_is_unit_elongation() {
        // Equal variance, no covariance → circular blob → elongation 1.
        let (_orient, elong) = pca(1.0, 1.0, 0.0);
        assert!((elong - 1.0).abs() < 1e-5, "elong {elong}");
    }

    #[wasm_bindgen_test]
    fn pca_major_axis_east_west_reads_90_degrees() {
        // Variance concentrated along x (east) → major axis is E–W → compass 90.
        let (orient, elong) = pca(4.0, 1.0, 0.0);
        assert!((orient - 90.0).abs() < 1e-3, "orient {orient}");
        assert!((elong - 2.0).abs() < 1e-4, "elong {elong}"); // sqrt(4/1)
    }

    #[wasm_bindgen_test]
    fn pca_major_axis_north_south_reads_0_degrees() {
        // Variance along y (north) → major axis is N–S → compass 0.
        let (orient, elong) = pca(1.0, 4.0, 0.0);
        assert!(orient.abs() < 1e-3, "orient {orient}");
        assert!((elong - 2.0).abs() < 1e-4, "elong {elong}");
    }

    #[wasm_bindgen_test]
    fn pca_orientation_folds_into_zero_to_180() {
        for &(sxx, syy, sxy) in &[
            (5.0, 1.0, 2.0),
            (1.0, 5.0, -2.0),
            (3.0, 3.0, 1.5),
            (10.0, 0.5, -4.0),
        ] {
            let (orient, _e) = pca(sxx, syy, sxy);
            assert!(
                (0.0..180.0).contains(&orient),
                "orient {orient} out of range"
            );
        }
    }

    #[wasm_bindgen_test]
    fn pca_degenerate_minor_axis_falls_back_to_unit_elongation() {
        // Perfectly collinear (minor eigenvalue 0) → guard returns 1.0, not inf.
        let (_o, elong) = pca(4.0, 0.0, 0.0);
        assert_eq!(elong, 1.0);
    }

    // -- summarize ------------------------------------------------------

    #[wasm_bindgen_test]
    fn summarize_single_pixel_due_east_gives_bearing_90_and_range() {
        // A single foreground gate at azimuth 90° (east), 3 km out.
        let azimuths = [90.0_f32, 0.0, 0.0, 0.0];
        let input = DetectionInput {
            azimuths: &azimuths,
            gate_values: &[], // unused by summarize (reads the decoded grid)
            azimuth_count: 4,
            gate_count: 4,
            first_gate_km: 0.0,
            gate_interval_km: 2.0,
            data_scale: 0.0,
            data_offset: 0.0,
            radar_lat: 35.0,
            radar_lon: -97.0,
            azimuth_spacing_deg: None,
        };
        // grid index = az_idx * gate_count + g = 0*4 + 1 = 1; gate g=1 → range
        // = 0 + (1 + 0.5) * 2 = 3 km.
        let mut grid = vec![f32::NAN; 16];
        grid[1] = 50.0;
        let pixels = vec![(0u16, 1u16)];

        let cell = summarize(&pixels, &grid, &input, 30.0);
        assert_eq!(cell.gate_count, 1);
        assert_eq!(cell.max_dbz, 50.0);
        assert!((cell.mean_dbz - 50.0).abs() < 1e-3);
        assert!(
            (cell.range_from_radar_km - 3.0).abs() < 1e-3,
            "range {}",
            cell.range_from_radar_km
        );
        assert!(
            (cell.bearing_from_radar_deg - 90.0).abs() < 0.5,
            "bearing {}",
            cell.bearing_from_radar_deg
        );
        // Single pixel → degenerate covariance → unit elongation.
        assert_eq!(cell.elongation, 1.0);
    }
}
