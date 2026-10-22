//! The grid detection runs on, and how an index pixel lands on the ground.
//!
//! The pipeline works in index space: rows by columns, a row adjacency that
//! says whether row `i` neighbours row `i + 1 (mod rows)`, and one ground
//! area per column. A radar's polar sweep ([`DetectionInput`]) fills that
//! in: rows are radials (azimuth wraps where neighbouring radials are
//! close), columns are gates, and a gate's area grows with range. Positions
//! are radar-local km, placed on the map with the flat projection around
//! the radar.
//!
//! [`Frame`] carries what one pass over pixels needs (the radial width, the
//! longitude scale), computed once per call.

use super::components;
use super::median::Decoder;
use super::{DetectionInput, KM_PER_DEG};

/// The geometry, as the pipeline sees it.
#[derive(Clone, Copy)]
pub(super) enum Grid<'a> {
    Polar(&'a DetectionInput<'a>),
}

impl<'a, 'b: 'a> From<&'a DetectionInput<'b>> for Grid<'a> {
    fn from(input: &'a DetectionInput<'b>) -> Self {
        Grid::Polar(input)
    }
}

impl<'a> Grid<'a> {
    /// Rows (radials) and columns (gates).
    pub(super) fn shape(&self) -> (usize, usize) {
        match self {
            Grid::Polar(p) => (p.azimuth_count, p.gate_count),
        }
    }

    /// The values, row-major, or `None` when the buffers do not hold the
    /// shape (or it is empty).
    pub(super) fn values(&self) -> Option<&'a [f32]> {
        match self {
            Grid::Polar(p) => {
                let ok = p.azimuth_count > 0
                    && p.gate_count > 0
                    && !p.azimuths.is_empty()
                    && p.gate_values.len() >= p.azimuth_count * p.gate_count
                    && p.azimuths.len() >= p.azimuth_count;
                ok.then_some(p.gate_values)
            }
        }
    }

    pub(super) fn decoder(&self) -> Decoder {
        match self {
            Grid::Polar(p) => Decoder {
                scale: p.data_scale,
                offset: p.data_offset,
            },
        }
    }

    /// Whether each row holds data. A polar row with a negative azimuth is
    /// padding from a partial sweep.
    pub(super) fn row_valid(&self) -> Vec<bool> {
        match self {
            Grid::Polar(p) => p.azimuths[..p.azimuth_count]
                .iter()
                .map(|a| *a >= 0.0)
                .collect(),
        }
    }

    /// `result[i]`: row `i` neighbours row `i + 1 (mod rows)`: azimuth wraps
    /// where the radials are close.
    pub(super) fn row_adjacency(&self) -> Vec<bool> {
        match self {
            Grid::Polar(p) => components::precompute_az_adjacency(p.azimuths, p.azimuth_count),
        }
    }

    /// Ground area of one pixel in each column, km².
    pub(super) fn column_area_km2(&self) -> Vec<f64> {
        match self {
            Grid::Polar(p) => {
                let az_spacing_rad = (radial_width_deg(p) as f64).to_radians();
                (0..p.gate_count)
                    .map(|g| {
                        let range_km = p.first_gate_km + (g as f64 + 0.5) * p.gate_interval_km;
                        range_km * az_spacing_rad * p.gate_interval_km
                    })
                    .collect()
            }
        }
    }

    /// Half the angular width the outline gives a radial.
    pub(super) fn half_spacing_deg(&self) -> f64 {
        match self {
            Grid::Polar(p) => components::median_spacing_deg(p.azimuths, p.azimuth_count) * 0.5,
        }
    }
}

/// What one pass over a grid's pixels needs, computed once.
#[derive(Clone, Copy)]
pub(super) enum Frame<'a> {
    Polar {
        input: &'a DetectionInput<'a>,
        az_spacing_rad: f64,
        cos_lat: f64,
    },
}

impl<'a> Frame<'a> {
    pub(super) fn new(grid: Grid<'a>) -> Self {
        match grid {
            Grid::Polar(input) => Frame::Polar {
                input,
                az_spacing_rad: (radial_width_deg(input) as f64).to_radians(),
                cos_lat: input.radar_lat.to_radians().cos().max(1e-6),
            },
        }
    }

    /// Pixel centre in local km (x east, y north, from the radar) and its
    /// ground area.
    #[inline]
    pub(super) fn pixel(self, row: u16, col: u16) -> (f64, f64, f64) {
        match self {
            Frame::Polar {
                input,
                az_spacing_rad,
                ..
            } => {
                let az_deg = input.azimuths[row as usize];
                let az_rad = (az_deg as f64).to_radians();
                let range_km = input.first_gate_km + (col as f64 + 0.5) * input.gate_interval_km;
                // Polar → radar-local Cartesian.
                let x_km = range_km * az_rad.sin();
                let y_km = range_km * az_rad.cos();
                // Gate footprint on the ground ≈ range · dθ · dr (annular sector).
                let area = range_km * az_spacing_rad * input.gate_interval_km;
                (x_km, y_km, area)
            }
        }
    }

    /// Local km to `(lat, lon)`.
    #[inline]
    pub(super) fn lat_lon(self, x_km: f64, y_km: f64) -> (f64, f64) {
        match self {
            Frame::Polar { input, cos_lat, .. } => (
                input.radar_lat + y_km / KM_PER_DEG,
                input.radar_lon + x_km / (KM_PER_DEG * cos_lat),
            ),
        }
    }
}

/// Angular width of one radial in degrees: the caller's spacing when given,
/// else the median gap between the supplied azimuths, else `360 / count`.
/// Using the real spacing keeps a partial sweep (only the radials received
/// so far) from counting each radial as wider than it is.
pub(super) fn radial_width_deg(input: &DetectionInput) -> f32 {
    if let Some(spacing) = input.azimuth_spacing_deg.filter(|s| *s > 0.0) {
        return spacing;
    }
    // Negative azimuths mark padded rows; they are not radials.
    let received: Vec<f32> = input
        .azimuths
        .iter()
        .copied()
        .filter(|a| *a >= 0.0)
        .collect();
    let median = median_gap_deg(&received);
    if median > 0.0 {
        median
    } else {
        360.0 / input.azimuth_count.max(1) as f32
    }
}

/// Median forward gap between consecutive azimuths, wrap-aware and
/// including the closing gap from the last back to the first, so a full
/// rotation reports its true spacing. 0 when there is nothing to measure.
fn median_gap_deg(azimuths: &[f32]) -> f32 {
    let n = azimuths.len();
    if n < 2 {
        return 0.0;
    }
    let mut gaps: Vec<f32> = (0..n)
        .map(|i| (azimuths[(i + 1) % n] - azimuths[i]).rem_euclid(360.0))
        .collect();
    gaps.sort_by(f32::total_cmp);
    gaps[n / 2]
}
