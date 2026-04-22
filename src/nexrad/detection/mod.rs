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

use crate::state::StormCellInfo;

/// Borrowed view of the sweep data needed to run detection.
pub struct DetectionInput<'a> {
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
pub struct DetectionParams {
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
pub fn detect_cells(input: &DetectionInput, params: &DetectionParams) -> Vec<StormCellInfo> {
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
