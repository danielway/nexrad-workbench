//! The two grids detection runs on, and how an index pixel lands on the
//! ground.
//!
//! The pipeline works in index space: rows by columns, a row adjacency that
//! says whether row `i` neighbours row `i + 1 (mod rows)`, and one ground
//! area per column. Two geometries fill that in:
//!
//! - **Polar** ([`DetectionInput`]): one radar's sweep. Rows are radials
//!   (azimuth wraps where neighbouring radials are close), columns are
//!   gates, and a gate's area grows with range. Positions are radar-local
//!   km, placed on the map with the flat projection around the radar.
//! - **Cartesian** ([`CartesianInput`]): the composite grid of several
//!   radars (`composite`). Rows run south to north, columns west to east,
//!   nothing wraps, and every cell has the same area. Positions are km from
//!   the grid's south-west corner, placed on the map with the flat
//!   projection about that corner (`cos(lat)` taken at the grid's centre),
//!   so a cell centre maps to lat/lon linearly.
//!
//! [`Frame`] carries what one pass over pixels needs (the radial width, the
//! longitude scale), computed once per call so the polar arithmetic is the
//! same as it was before the Cartesian grid existed.

use super::components::{self, Pixel};
use super::median::Decoder;
use super::{DetectionInput, KM_PER_DEG};

/// A radar that contributes to a Cartesian grid.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GridSite {
    /// The radar's id, as the cells name it.
    pub id: String,
    pub lat: f64,
    pub lon: f64,
}

/// Borrowed view of a Cartesian grid (see the module docs).
pub(crate) struct CartesianInput<'a> {
    /// dBZ per cell, row-major as `row * cols + col`, row 0 southmost. A
    /// value at or below 1 is background, as the sweep sentinels are.
    pub values: &'a [f32],
    /// Per cell, bit `i` set when `sites[i]` had a valid gate there.
    pub mask: &'a [u8],
    pub rows: usize,
    pub cols: usize,
    /// Side of one square cell, km.
    pub cell_km: f64,
    /// South-west corner of cell (0, 0).
    pub origin_lat: f64,
    pub origin_lon: f64,
    /// `cos` of the grid centre's latitude: km per degree of longitude is
    /// `KM_PER_DEG * cos_lat` everywhere on the grid.
    pub cos_lat: f64,
    /// The radars behind `mask`, at most eight.
    pub sites: &'a [GridSite],
}

/// Either geometry, as the pipeline sees it.
#[derive(Clone, Copy)]
pub(super) enum Grid<'a> {
    Polar(&'a DetectionInput<'a>),
    Cartesian(&'a CartesianInput<'a>),
}

impl<'a, 'b: 'a> From<&'a DetectionInput<'b>> for Grid<'a> {
    fn from(input: &'a DetectionInput<'b>) -> Self {
        Grid::Polar(input)
    }
}

impl<'a, 'b: 'a> From<&'a CartesianInput<'b>> for Grid<'a> {
    fn from(input: &'a CartesianInput<'b>) -> Self {
        Grid::Cartesian(input)
    }
}

/// A radar that sees a cell, with the cell's bearing (degrees) and range
/// (km) from it.
pub(super) struct Sighting {
    pub site: String,
    pub bearing_deg: f32,
    pub range_km: f32,
}

impl<'a> Grid<'a> {
    /// Rows (radials, or south to north) and columns (gates, or west to
    /// east).
    pub(super) fn shape(&self) -> (usize, usize) {
        match self {
            Grid::Polar(p) => (p.azimuth_count, p.gate_count),
            Grid::Cartesian(c) => (c.rows, c.cols),
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
            Grid::Cartesian(c) => {
                let ok = c.rows > 0
                    && c.cols > 0
                    && c.values.len() >= c.rows * c.cols
                    && c.mask.len() >= c.rows * c.cols;
                ok.then(|| &c.values[..c.rows * c.cols])
            }
        }
    }

    pub(super) fn decoder(&self) -> Decoder {
        match self {
            Grid::Polar(p) => Decoder {
                scale: p.data_scale,
                offset: p.data_offset,
            },
            Grid::Cartesian(_) => Decoder {
                scale: 0.0,
                offset: 0.0,
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
            Grid::Cartesian(c) => vec![true; c.rows],
        }
    }

    /// `result[i]`: row `i` neighbours row `i + 1 (mod rows)`. Polar wraps
    /// where the radials are close; Cartesian never wraps.
    pub(super) fn row_adjacency(&self) -> Vec<bool> {
        match self {
            Grid::Polar(p) => components::precompute_az_adjacency(p.azimuths, p.azimuth_count),
            Grid::Cartesian(c) => {
                let mut adj = vec![true; c.rows];
                if let Some(last) = adj.last_mut() {
                    *last = false;
                }
                adj
            }
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
            Grid::Cartesian(c) => vec![c.cell_km * c.cell_km; c.cols],
        }
    }

    /// Half the angular width the outline gives a radial (polar only).
    pub(super) fn half_spacing_deg(&self) -> f64 {
        match self {
            Grid::Polar(p) => components::median_spacing_deg(p.azimuths, p.azimuth_count) * 0.5,
            Grid::Cartesian(_) => 0.0,
        }
    }

    /// The radars that see a footprint, nearest to its centroid first, each
    /// with the centroid's bearing and range from it. Empty on a polar grid,
    /// where the one radar is the input's.
    pub(super) fn sightings(&self, pixels: &[Pixel], centroid: (f64, f64)) -> Vec<Sighting> {
        let Grid::Cartesian(c) = self else {
            return Vec::new();
        };
        let bits = pixels.iter().fold(0u8, |acc, &(r, col)| {
            acc | c.mask[r as usize * c.cols + col as usize]
        });
        let mut seen: Vec<Sighting> = c
            .sites
            .iter()
            .enumerate()
            .filter(|(i, _)| *i < 8 && bits & (1 << i) != 0)
            .map(|(_, s)| {
                let (bearing_deg, range_km) = bearing_range_from((s.lat, s.lon), centroid);
                Sighting {
                    site: s.id.clone(),
                    bearing_deg,
                    range_km,
                }
            })
            .collect();
        seen.sort_by(|a, b| a.range_km.total_cmp(&b.range_km).then(a.site.cmp(&b.site)));
        seen
    }
}

/// Compass bearing (degrees) and range (km) of `to` from `from`, in the flat
/// projection around `from` that the 2D canvas and the polar detector use.
pub(super) fn bearing_range_from(from: (f64, f64), to: (f64, f64)) -> (f32, f32) {
    let cos_lat = from.0.to_radians().cos().max(1e-6);
    let x = (to.1 - from.1) * KM_PER_DEG * cos_lat;
    let y = (to.0 - from.0) * KM_PER_DEG;
    let bearing = ((x.atan2(y).to_degrees() + 360.0) % 360.0) as f32;
    (bearing, x.hypot(y) as f32)
}

/// What one pass over a grid's pixels needs, computed once.
#[derive(Clone, Copy)]
pub(super) enum Frame<'a> {
    Polar {
        input: &'a DetectionInput<'a>,
        az_spacing_rad: f64,
        cos_lat: f64,
    },
    Cartesian {
        input: &'a CartesianInput<'a>,
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
            Grid::Cartesian(input) => Frame::Cartesian { input },
        }
    }

    /// Pixel centre in local km (x east, y north) and its ground area. The
    /// local origin is the radar (polar) or the grid's south-west corner.
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
            Frame::Cartesian { input } => (
                (col as f64 + 0.5) * input.cell_km,
                (row as f64 + 0.5) * input.cell_km,
                input.cell_km * input.cell_km,
            ),
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
            Frame::Cartesian { input } => (
                input.origin_lat + y_km / KM_PER_DEG,
                input.origin_lon + x_km / (KM_PER_DEG * input.cos_lat),
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn grid<'a>(values: &'a [f32], mask: &'a [u8], sites: &'a [GridSite]) -> CartesianInput<'a> {
        CartesianInput {
            values,
            mask,
            rows: 4,
            cols: 3,
            cell_km: 0.5,
            origin_lat: 28.0,
            origin_lon: -82.0,
            cos_lat: 28.0f64.to_radians().cos(),
            sites,
        }
    }

    #[wasm_bindgen_test]
    fn cartesian_rows_never_wrap() {
        let (v, m) = (vec![0.0; 12], vec![0u8; 12]);
        let g = grid(&v, &m, &[]);
        assert_eq!(
            Grid::from(&g).row_adjacency(),
            vec![true, true, true, false]
        );
        assert_eq!(Grid::from(&g).column_area_km2(), vec![0.25; 3]);
        assert_eq!(Grid::from(&g).row_valid(), vec![true; 4]);
    }

    #[wasm_bindgen_test]
    fn cartesian_buffers_must_hold_the_shape() {
        let (v, m) = (vec![0.0; 11], vec![0u8; 12]);
        assert!(Grid::from(&grid(&v, &m, &[])).values().is_none());
        let (v, m) = (vec![0.0; 12], vec![0u8; 11]);
        assert!(Grid::from(&grid(&v, &m, &[])).values().is_none());
    }

    #[wasm_bindgen_test]
    fn cartesian_cell_centres_map_linearly_to_lat_lon() {
        let (v, m) = (vec![0.0; 12], vec![0u8; 12]);
        let g = grid(&v, &m, &[]);
        let f = Frame::new(Grid::from(&g));
        let (x0, y0, a) = f.pixel(0, 0);
        assert_eq!((x0, y0, a), (0.25, 0.25, 0.25));
        let (x1, y1, _) = f.pixel(2, 1);
        assert_eq!((x1, y1), (0.75, 1.25));
        let (lat0, lon0) = f.lat_lon(x0, y0);
        let (lat1, lon1) = f.lat_lon(x1, y1);
        // One cell north and one east is the same step everywhere.
        let (lat2, lon2) = f.lat_lon(x1 + 0.5, y1 + 0.5);
        assert!(((lat2 - lat1) - 0.5 / KM_PER_DEG).abs() < 1e-12);
        assert!(((lon2 - lon1) - (lon1 - lon0)).abs() < 1e-12);
        assert!((lat0 - (28.0 + 0.25 / KM_PER_DEG)).abs() < 1e-12);
    }

    #[wasm_bindgen_test]
    fn sightings_list_the_masked_sites_nearest_first() {
        let sites = vec![
            GridSite {
                id: "KJAX".into(),
                lat: 30.48,
                lon: -81.70,
            },
            GridSite {
                id: "KMLB".into(),
                lat: 28.11,
                lon: -80.65,
            },
            GridSite {
                id: "KTBW".into(),
                lat: 27.70,
                lon: -82.40,
            },
        ];
        let v = vec![0.0; 12];
        let mut m = vec![0u8; 12];
        m[0] = 0b001;
        m[4] = 0b100;
        let g = grid(&v, &m, &sites);
        let seen = Grid::from(&g).sightings(&[(0, 0), (1, 1)], (28.0, -82.0));
        let ids: Vec<&str> = seen.iter().map(|s| s.site.as_str()).collect();
        assert_eq!(ids, vec!["KTBW", "KJAX"]);
        // KTBW is south-west of (28, -82): bearing from it is north-east.
        assert!((0.0..90.0).contains(&seen[0].bearing_deg));
        assert!(seen[0].range_km < seen[1].range_km);
        // A footprint no site masks is seen by none.
        assert!(Grid::from(&g)
            .sightings(&[(3, 2)], (28.0, -82.0))
            .is_empty());
    }

    #[wasm_bindgen_test]
    fn bearing_range_matches_the_polar_convention() {
        // 10 km due east of a radar at 35 N.
        let cos = 35f64.to_radians().cos();
        let (b, r) = bearing_range_from((35.0, -97.0), (35.0, -97.0 + 10.0 / (KM_PER_DEG * cos)));
        assert!((b - 90.0).abs() < 1e-3, "{b}");
        assert!((r - 10.0).abs() < 1e-3, "{r}");
    }
}
