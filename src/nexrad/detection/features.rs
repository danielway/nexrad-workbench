//! Per-cell feature extraction.
//!
//! Takes a component (a list of (az_idx, gate_idx) pixels) and computes the
//! fields the UI/state layer needs: reflectivity-weighted centroid in
//! lat/lon, lat/lon bounding box, physical area in km², max/mean dBZ,
//! bearing and range from the radar, and a 2×2 PCA for major-axis
//! orientation + elongation.

use super::components::Pixel;
use super::DetectionInput;
use crate::state::StormCellInfo;

/// Approximate kilometers per degree of latitude. Matches the
/// equirectangular approximation used in `canvas_inspector.rs`.
const KM_PER_DEG: f64 = 111.0;

pub(super) fn summarize(
    pixels: &[Pixel],
    grid: &[f32],
    input: &DetectionInput,
    threshold_dbz: f32,
) -> StormCellInfo {
    let cos_lat = input.radar_lat.to_radians().cos().max(1e-6);
    let az_spacing_rad = (360.0_f64 / input.azimuth_count as f64).to_radians();

    let mut max_dbz = f32::NEG_INFINITY;
    let mut sum_dbz = 0.0_f64;
    let mut area_km2 = 0.0_f64;

    // Weighted centroid and covariance accumulators in radar-local
    // Cartesian (km): x = east, y = north.
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

    for &(az_idx, g) in pixels {
        let idx = az_idx as usize * input.gate_count + g as usize;
        let dbz = grid[idx];
        if dbz.is_nan() {
            continue;
        }

        let az_deg = input.azimuths[az_idx as usize];
        let az_rad = (az_deg as f64).to_radians();
        let range_km = input.first_gate_km + (g as f64 + 0.5) * input.gate_interval_km;

        // Polar → radar-local Cartesian.
        let x_km = range_km * az_rad.sin();
        let y_km = range_km * az_rad.cos();

        // Gate footprint on the ground ≈ range · dθ · dr (annular sector).
        let gate_area_km2 = range_km * az_spacing_rad * input.gate_interval_km;
        area_km2 += gate_area_km2;

        // Weight = reflectivity above threshold. Linear-Z would be more
        // physical (10^(dbz/10)), but this dampens extreme cores that would
        // otherwise dominate the centroid.
        let w = (dbz - threshold_dbz).max(0.0) as f64 + 0.001;
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
        let lat = input.radar_lat + y_km / KM_PER_DEG;
        let lon = input.radar_lon + x_km / (KM_PER_DEG * cos_lat);
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

    // Weighted centroid in radar-local Cartesian, then to lat/lon.
    let inv_w = if sum_w > 0.0 { 1.0 / sum_w } else { 0.0 };
    let cx_km = sum_wx * inv_w;
    let cy_km = sum_wy * inv_w;
    let centroid_lat = input.radar_lat + cy_km / KM_PER_DEG;
    let centroid_lon = input.radar_lon + cx_km / (KM_PER_DEG * cos_lat);

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

    StormCellInfo {
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
