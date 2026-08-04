//! Storm cell detection.
//!
//! Threshold + connected-component analysis on reflectivity gates in polar
//! space, followed by per-cell feature extraction (area, centroid, bounds,
//! bearing/range from radar, major-axis orientation, elongation).
//!
//! Operates directly on the CPU-side shadow of the rendered sweep, so no
//! decode / marshal work is needed. Keeping the algorithm in-tree lets us
//! iterate on heuristics (gate-area weighting, wrap-around handling,
//! velocity-based rotation, cross-scan tracking) independently from the
//! upstream `nexrad-process` crate.

mod components;
mod features;

use crate::core::StormCellInfo;

/// Borrowed view of the sweep data needed to run detection.
pub(crate) struct DetectionInput<'a> {
    /// Sorted azimuth angles (degrees, 0..360). Negative values mark padded
    /// slots from partial live sweeps and are skipped.
    pub azimuths: &'a [f32],
    /// Raw gate values, row-major as `az_idx * gate_count + gate_idx`.
    /// Sentinels: 0 = below threshold, 1 = range folded.
    pub gate_values: &'a [f32],
    pub azimuth_count: usize,
    pub gate_count: usize,
    pub first_gate_km: f64,
    pub gate_interval_km: f64,
    /// Physical conversion: `physical = (raw - offset) / scale`. If `scale`
    /// is zero the raw value is already physical.
    pub data_scale: f32,
    pub data_offset: f32,
    pub radar_lat: f64,
    pub radar_lon: f64,
}

/// Tuning knobs for the detector.
pub(crate) struct DetectionParams {
    /// Core (promotion) threshold in dBZ. A component must contain at
    /// least one gate this strong to survive.
    pub threshold_dbz: f32,
    /// How far below `threshold_dbz` the edge threshold sits. Gates between
    /// `threshold_dbz - edge_margin_dbz` and `threshold_dbz` are allowed
    /// to bridge two core regions, preventing a single storm from
    /// fragmenting into adjacent blobs when its reflectivity core has
    /// natural gaps.
    pub edge_margin_dbz: f32,
    /// Reject cells smaller than this. Guards against noise speckle.
    pub min_area_km2: f32,
    /// Reject cells with fewer than this many gates, regardless of area.
    pub min_gate_count: u32,
}

impl Default for DetectionParams {
    fn default() -> Self {
        Self {
            threshold_dbz: 35.0,
            edge_margin_dbz: 5.0,
            min_area_km2: 15.0,
            min_gate_count: 8,
        }
    }
}

/// Run detection over the provided sweep, returning one `StormCellInfo`
/// per surviving cell.
pub(crate) fn detect_cells(input: &DetectionInput, params: &DetectionParams) -> Vec<StormCellInfo> {
    if input.azimuth_count == 0 || input.gate_count == 0 || input.azimuths.is_empty() {
        return Vec::new();
    }
    if input.gate_values.len() < input.azimuth_count * input.gate_count {
        return Vec::new();
    }

    let core_threshold = params.threshold_dbz;
    let edge_threshold = params.threshold_dbz - params.edge_margin_dbz.max(0.0);

    // 1. Decode raw gate values into physical dBZ, masking anything below
    //    the edge threshold as NaN. Gates between edge and core thresholds
    //    participate in connectivity but must be promoted by an internal
    //    core gate to survive.
    let grid = build_physical_grid(input, edge_threshold);

    // 2. Label connected components with 8-neighborhood + azimuth wrap
    //    (wrap only when the angular gap between adjacent sorted azimuth
    //    indices is within the median spacing).
    let components =
        components::label(&grid, input.azimuths, input.azimuth_count, input.gate_count);

    // 3. Promote + summarize. Drop any component without a core-threshold
    //    gate, then drop any that fail the size filters.
    components
        .into_iter()
        .filter_map(|pixels| {
            if (pixels.len() as u32) < params.min_gate_count {
                return None;
            }
            let has_core = pixels.iter().any(|&(a, g)| {
                let idx = a as usize * input.gate_count + g as usize;
                grid[idx] >= core_threshold
            });
            if !has_core {
                return None;
            }
            let cell = features::summarize(&pixels, &grid, input, edge_threshold);
            if cell.area_km2 < params.min_area_km2 {
                None
            } else {
                Some(cell)
            }
        })
        .collect()
}

/// Decode raw gate values into physical dBZ, writing NaN for any gate that
/// shouldn't participate in detection (sentinel, padded azimuth row, below
/// edge threshold). Gates at or above `edge_threshold_dbz` keep their
/// physical value so `features::summarize` can read it back.
fn build_physical_grid(input: &DetectionInput, edge_threshold_dbz: f32) -> Vec<f32> {
    let n = input.azimuth_count * input.gate_count;
    let mut grid = vec![f32::NAN; n];

    let use_raw = input.data_scale == 0.0;

    for az_idx in 0..input.azimuth_count {
        let az_value = input.azimuths.get(az_idx).copied().unwrap_or(-1.0);
        if az_value < 0.0 {
            // Padded row from a partial sweep — leave as NaN.
            continue;
        }
        let row_start = az_idx * input.gate_count;
        for g in 0..input.gate_count {
            let raw = input.gate_values[row_start + g];
            if raw <= 1.0 {
                continue; // sentinel (no echo / range folded)
            }
            let physical = if use_raw {
                raw
            } else {
                (raw - input.data_offset) / input.data_scale
            };
            if physical >= edge_threshold_dbz {
                grid[row_start + g] = physical;
            }
        }
    }

    grid
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Build a `DetectionInput` over raw==physical gates (data_scale 0), with
    /// evenly-spaced azimuths unless `azimuths` is supplied.
    fn mk_input<'a>(
        azimuths: &'a [f32],
        gates: &'a [f32],
        gate_count: usize,
    ) -> DetectionInput<'a> {
        DetectionInput {
            azimuths,
            gate_values: gates,
            azimuth_count: azimuths.len(),
            gate_count,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            data_scale: 0.0, // raw value is already physical dBZ
            data_offset: 0.0,
            radar_lat: 35.0,
            radar_lon: -97.0,
        }
    }

    fn even_az(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32 * (360.0 / n as f32)).collect()
    }

    fn flatten(rows: Vec<Vec<f32>>) -> (Vec<f32>, usize) {
        let gate_count = rows[0].len();
        let flat = rows.into_iter().flatten().collect();
        (flat, gate_count)
    }

    /// Params that detect any connected core gate (size filters relaxed) so
    /// tests exercise the connectivity/promotion logic, not the geometry.
    fn permissive() -> DetectionParams {
        DetectionParams {
            threshold_dbz: 35.0,
            edge_margin_dbz: 5.0,
            min_area_km2: 0.0,
            min_gate_count: 1,
        }
    }

    #[wasm_bindgen_test]
    fn empty_input_yields_no_cells() {
        let input = mk_input(&[], &[], 0);
        assert!(detect_cells(&input, &permissive()).is_empty());
    }

    #[wasm_bindgen_test]
    fn truncated_gate_buffer_is_rejected() {
        // azimuth_count(2) * gate_count(4) = 8 expected, only 4 supplied.
        let az = even_az(2);
        let gates = vec![45.0; 4];
        let input = mk_input(&az, &gates, 4);
        assert!(detect_cells(&input, &permissive()).is_empty());
    }

    #[wasm_bindgen_test]
    fn single_block_is_one_cell_with_expected_summary() {
        // A 2×2 core of 45 dBZ in a 4×4 field of background.
        let (gates, gc) = flatten(vec![
            vec![45.0, 45.0, 0.0, 0.0],
            vec![45.0, 45.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0],
        ]);
        let az = even_az(4);
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive());
        assert_eq!(cells.len(), 1);
        let c = &cells[0];
        assert_eq!(c.gate_count, 4);
        assert_eq!(c.max_dbz, 45.0);
        assert!((c.mean_dbz - 45.0).abs() < 1e-3);
        // Geometry sanity.
        assert!(c.range_from_radar_km > 0.0);
        assert!((0.0..360.0).contains(&c.bearing_from_radar_deg));
        assert!((0.0..180.0).contains(&c.orientation_deg));
        assert!(c.elongation >= 1.0);
        let (min_lat, min_lon, max_lat, max_lon) = c.bounds;
        assert!(min_lat <= max_lat && min_lon <= max_lon);
    }

    #[wasm_bindgen_test]
    fn two_disjoint_blocks_are_two_cells() {
        // Separated in both axes so neither 8-neighborhood nor az-wrap links them.
        let (gates, gc) = flatten(vec![
            vec![45.0, 45.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![45.0, 45.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 45.0, 45.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 45.0, 45.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ]);
        let az = even_az(6);
        let input = mk_input(&az, &gates, gc);
        assert_eq!(detect_cells(&input, &permissive()).len(), 2);
    }

    #[wasm_bindgen_test]
    fn azimuth_seam_wraps_into_single_cell() {
        // Blobs on the first and last evenly-spaced radials are angularly
        // adjacent (gap == median spacing) → one component across the 0°/360° seam.
        let mut rows = vec![vec![0.0; 6]; 8];
        rows[0][2] = 45.0;
        rows[0][3] = 45.0;
        rows[7][2] = 45.0;
        rows[7][3] = 45.0;
        let (gates, gc) = flatten(rows);
        let az = even_az(8);
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive());
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].gate_count, 4);
    }

    #[wasm_bindgen_test]
    fn large_angular_gap_does_not_wrap() {
        // A partial sweep: first and last radials are nowhere near each other,
        // so the two same-gate blobs must stay separate components.
        let mut rows = vec![vec![0.0; 6]; 4];
        rows[0][2] = 45.0;
        rows[0][3] = 45.0;
        rows[3][2] = 45.0;
        rows[3][3] = 45.0;
        let (gates, gc) = flatten(rows);
        // azimuths: 0, 20, 40, 60 → gap 0→20→40→60 is 20 each, but the wrap
        // gap 60→0 is 300° ≫ 2× median(20) so the seam is NOT adjacent.
        let az = vec![0.0_f32, 20.0, 40.0, 60.0];
        let input = mk_input(&az, &gates, gc);
        assert_eq!(detect_cells(&input, &permissive()).len(), 2);
    }

    #[wasm_bindgen_test]
    fn edge_only_blob_without_core_is_dropped() {
        // All gates sit between the edge (30) and core (35) thresholds — they
        // form a component but none promotes it, so it is discarded.
        let (gates, gc) = flatten(vec![
            vec![32.0, 32.0, 0.0],
            vec![32.0, 32.0, 0.0],
            vec![0.0, 0.0, 0.0],
        ]);
        let az = even_az(3);
        let input = mk_input(&az, &gates, gc);
        assert!(detect_cells(&input, &permissive()).is_empty());
    }

    #[wasm_bindgen_test]
    fn min_gate_count_filter_rejects_small_components() {
        // A 2-gate core with min_gate_count 5 → rejected.
        let (gates, gc) = flatten(vec![vec![45.0, 45.0, 0.0], vec![0.0, 0.0, 0.0]]);
        let az = even_az(2);
        let input = mk_input(&az, &gates, gc);
        let params = DetectionParams {
            min_gate_count: 5,
            ..permissive()
        };
        assert!(detect_cells(&input, &params).is_empty());
    }

    #[wasm_bindgen_test]
    fn min_area_filter_rejects_below_area() {
        // Detected with permissive area, gone with an absurd area floor.
        let (gates, gc) = flatten(vec![vec![45.0, 45.0, 0.0], vec![45.0, 45.0, 0.0]]);
        let az = even_az(2);
        let input = mk_input(&az, &gates, gc);
        assert_eq!(detect_cells(&input, &permissive()).len(), 1);
        let params = DetectionParams {
            min_area_km2: 1.0e9,
            ..permissive()
        };
        assert!(detect_cells(&input, &params).is_empty());
    }

    #[wasm_bindgen_test]
    fn padded_azimuth_rows_are_ignored() {
        // The only strong gates live on a padded (negative-azimuth) row, which
        // build_physical_grid masks out → no cell.
        let (gates, gc) = flatten(vec![
            vec![0.0, 0.0, 0.0, 0.0],
            vec![45.0, 45.0, 45.0, 45.0],
            vec![0.0, 0.0, 0.0, 0.0],
        ]);
        let az = vec![0.0_f32, -1.0, 90.0];
        let input = mk_input(&az, &gates, gc);
        assert!(detect_cells(&input, &permissive()).is_empty());
    }

    #[wasm_bindgen_test]
    fn sentinel_values_are_background() {
        // raw 0 (below threshold) and 1 (range folded) never participate.
        let grid = build_physical_grid(&mk_input(&even_az(1), &[0.0, 1.0, 45.0, 0.9], 4), 30.0);
        assert!(grid[0].is_nan());
        assert!(grid[1].is_nan());
        assert_eq!(grid[2], 45.0);
        assert!(grid[3].is_nan());
    }

    #[wasm_bindgen_test]
    fn build_physical_grid_applies_scale_and_offset() {
        // physical = (raw - offset) / scale. raw 146, offset 66, scale 2 → 40 dBZ.
        let az = even_az(1);
        let gates = vec![146.0_f32, 100.0];
        let input = DetectionInput {
            azimuths: &az,
            gate_values: &gates,
            azimuth_count: 1,
            gate_count: 2,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            data_scale: 2.0,
            data_offset: 66.0,
            radar_lat: 35.0,
            radar_lon: -97.0,
        };
        let grid = build_physical_grid(&input, 30.0);
        assert!((grid[0] - 40.0).abs() < 1e-3);
        // (100 - 66) / 2 = 17 dBZ, below the 30 edge → masked.
        assert!(grid[1].is_nan());
    }

    #[wasm_bindgen_test]
    fn pca_produces_finite_elongation_and_bounded_orientation() {
        // A 2-row × 8-gate block is elongated along range but, unlike a single
        // radial, has non-degenerate width across azimuth — so the minor axis
        // is non-zero and elongation is a real (finite, ≥ 1) ratio.
        let az = even_az(4);
        let (gates, gc) = flatten(vec![
            vec![0.0; 8],
            vec![45.0; 8],
            vec![45.0; 8],
            vec![0.0; 8],
        ]);
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive());
        assert_eq!(cells.len(), 1);
        let c = &cells[0];
        assert!(c.elongation.is_finite());
        assert!(c.elongation >= 1.0);
        assert!((0.0..180.0).contains(&c.orientation_deg));
    }
}
