//! The composite grid: several radars' lowest reflectivity cuts resampled
//! onto one Cartesian grid, so a storm two radars see is detected once.
//!
//! - **Cells.** Square, [`CELL_KM`] on a side, in the flat (equirectangular)
//!   projection about the grid's south-west corner, with `cos(lat)` taken
//!   at the radars' mean latitude (the `KM_PER_DEG` convention the detector
//!   uses everywhere).
//! - **Extent.** The union of every cut's echo bounding box (gates at or
//!   above the threshold), padded by [`PAD_KM`], so the grid covers the
//!   storms and not the whole of every disc; a cut with no echo adds
//!   nothing, and with no echo anywhere there is no grid. The box is then
//!   snapped outward onto a fixed lattice (whole cells from 0° latitude and
//!   longitude, for the same radars), so a cell's grid cells, and so its
//!   footprint, do not move when another radar's echo grows or shrinks the
//!   box.
//! - **Value.** Each cell takes, from every radar that covers its centre,
//!   the gate there: the nearest radial (none past 1.5 radial widths, a
//!   gap) and the gate whose span holds the range, the same lookup the 2D
//!   shader and the data probe use. Sentinels (raw 0 and 1) are skipped.
//!   The cell keeps the highest dBZ, and a mask bit for every radar that had
//!   a valid gate there.
//!
//! Positions on each radar follow the flat projection around that radar,
//! as its polar detection and its disc on the canvas do.

use super::geometry::{radial_width_deg, GridSite};
use super::median::Decoder;
use super::{
    detect_cells_cartesian, CartesianInput, DetectionInput, DetectionParams, StormCellsResult,
    KM_PER_DEG,
};

/// Side of one composite cell, km.
pub(crate) const CELL_KM: f64 = 0.5;

/// Margin around the union of the echo boxes, km: room for the median
/// filter's window and the outline, so no envelope touches the grid's edge.
pub(crate) const PAD_KM: f64 = 5.0;

/// Radars one grid can hold (one mask bit each).
pub(crate) const MAX_SITES: usize = 8;

/// One radar's cut and its id.
pub(crate) struct CompositeCut<'a> {
    pub site: &'a str,
    /// The cut's volume start, Unix ms: when this radar last saw what the
    /// cut shows.
    pub time_ms: i64,
    /// The cut, with the radar's position.
    pub sweep: DetectionInput<'a>,
}

/// A lat/lon box: (min_lat, min_lon, max_lat, max_lon).
pub(crate) type LatLonBox = (f64, f64, f64, f64);

/// Where the grid lies and how big it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Extent {
    /// South-west corner of cell (0, 0).
    pub origin_lat: f64,
    pub origin_lon: f64,
    /// `cos` of the latitude the projection is taken at: the box's centre
    /// from [`extent`], the radars' mean once on the lattice.
    pub cos_lat: f64,
    pub rows: usize,
    pub cols: usize,
}

/// The resampled grid, owned.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CompositeGrid {
    pub extent: Extent,
    /// dBZ per cell, row-major from the south-west; 0 where no radar had a
    /// valid gate.
    pub values: Vec<f32>,
    /// Per cell, bit `i` for `sites[i]` having a valid gate there.
    pub mask: Vec<u8>,
    /// Per cell, the index into `sites` of the radar whose gate gave the
    /// value (the first of equal maxima); `u8::MAX` where none did.
    pub winner: Vec<u8>,
    pub sites: Vec<GridSite>,
}

impl CompositeGrid {
    /// The detector's view of the grid.
    pub(crate) fn input(&self) -> CartesianInput<'_> {
        CartesianInput {
            values: &self.values,
            mask: &self.mask,
            winner: &self.winner,
            rows: self.extent.rows,
            cols: self.extent.cols,
            cell_km: CELL_KM,
            origin_lat: self.extent.origin_lat,
            origin_lon: self.extent.origin_lon,
            cos_lat: self.extent.cos_lat,
            sites: &self.sites,
        }
    }
}

fn decoder(sweep: &DetectionInput) -> Decoder {
    Decoder {
        scale: sweep.data_scale,
        offset: sweep.data_offset,
    }
}

fn radar_cos_lat(sweep: &DetectionInput) -> f64 {
    sweep.radar_lat.to_radians().cos().max(1e-6)
}

/// Whether the cut's buffers hold its shape.
fn well_formed(s: &DetectionInput) -> bool {
    s.azimuth_count > 0
        && s.gate_count > 0
        && s.azimuths.len() >= s.azimuth_count
        && s.gate_values.len() >= s.azimuth_count * s.gate_count
}

/// The lat/lon box around every gate of `sweep` at or above
/// `threshold_dbz` (each gate's whole range span), or `None` when there is
/// no such gate.
pub(crate) fn echo_box(sweep: &DetectionInput, threshold_dbz: f32) -> Option<LatLonBox> {
    if !well_formed(sweep) {
        return None;
    }
    let dec = decoder(sweep);
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for a in 0..sweep.azimuth_count {
        let az = sweep.azimuths[a];
        if az < 0.0 {
            continue;
        }
        let row = &sweep.gate_values[a * sweep.gate_count..(a + 1) * sweep.gate_count];
        let first = row.iter().position(|&v| dec.decode(v) >= threshold_dbz);
        let Some(first) = first else {
            continue;
        };
        let last = row
            .iter()
            .rposition(|&v| dec.decode(v) >= threshold_dbz)
            .unwrap_or(first);
        let (sin, cos) = (az as f64).to_radians().sin_cos();
        let near = sweep.first_gate_km + first as f64 * sweep.gate_interval_km;
        let far = sweep.first_gate_km + (last + 1) as f64 * sweep.gate_interval_km;
        for r in [near, far] {
            let (x, y) = (r * sin, r * cos);
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    if !min_x.is_finite() {
        return None;
    }
    // A radial's echo between its nearest and farthest gate can bulge
    // beyond the chord's ends by at most a radial width at that range;
    // padding (PAD_KM) covers it.
    let cos_lat = radar_cos_lat(sweep);
    Some((
        sweep.radar_lat + min_y / KM_PER_DEG,
        sweep.radar_lon + min_x / (KM_PER_DEG * cos_lat),
        sweep.radar_lat + max_y / KM_PER_DEG,
        sweep.radar_lon + max_x / (KM_PER_DEG * cos_lat),
    ))
}

/// The grid around the union of `boxes`, padded by `pad_km`, in cells of
/// `cell_km`. `None` with no box.
pub(crate) fn extent(boxes: &[LatLonBox], pad_km: f64, cell_km: f64) -> Option<Extent> {
    let (mut s, mut w, mut n, mut e) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for &(a, b, c, d) in boxes {
        s = s.min(a);
        w = w.min(b);
        n = n.max(c);
        e = e.max(d);
    }
    if !s.is_finite() {
        return None;
    }
    let cos_lat = ((s + n) / 2.0).to_radians().cos().max(1e-6);
    let lat_pad = pad_km / KM_PER_DEG;
    let lon_pad = pad_km / (KM_PER_DEG * cos_lat);
    let (origin_lat, origin_lon) = (s - lat_pad, w - lon_pad);
    let height_km = (n + lat_pad - origin_lat) * KM_PER_DEG;
    let width_km = (e + lon_pad - origin_lon) * KM_PER_DEG * cos_lat;
    let cells = |km: f64| ((km / cell_km).ceil() as usize).clamp(1, u16::MAX as usize);
    Some(Extent {
        origin_lat,
        origin_lon,
        cos_lat,
        rows: cells(height_km),
        cols: cells(width_km),
    })
}

/// `ext` moved onto the fixed lattice of [`CELL_KM`] cells at `cos_lat`:
/// the south-west corner snapped down to whole cells from 0° latitude and
/// longitude, and enough rows and columns to still cover `ext`'s north-east
/// corner.
fn on_lattice(ext: Extent, cos_lat: f64) -> Extent {
    let cos_lat = cos_lat.max(1e-6);
    let dlat = CELL_KM / KM_PER_DEG;
    let dlon = CELL_KM / (KM_PER_DEG * cos_lat);
    let north = ext.origin_lat + ext.rows as f64 * CELL_KM / KM_PER_DEG;
    let east = ext.origin_lon + ext.cols as f64 * CELL_KM / (KM_PER_DEG * ext.cos_lat);
    let origin_lat = (ext.origin_lat / dlat).floor() * dlat;
    let origin_lon = (ext.origin_lon / dlon).floor() * dlon;
    let cells = |span: f64, step: f64| ((span / step).ceil() as usize).clamp(1, u16::MAX as usize);
    Extent {
        origin_lat,
        origin_lon,
        cos_lat,
        rows: cells(north - origin_lat, dlat),
        cols: cells(east - origin_lon, dlon),
    }
}

/// Azimuth bins per degree in [`Radials::table`]: 0.01°, far finer than a
/// radial, so the bin's nearest radial is the point's nearest radial but
/// for points within 0.005° of halfway between two radials.
const BINS_PER_DEG: usize = 100;

/// A cut's radials sorted by azimuth, for the nearest-radial lookup.
struct Radials {
    /// (azimuth, row) of every valid radial, by azimuth.
    sorted: Vec<(f32, usize)>,
    /// Farther than this from every radial is a gap.
    max_gap_deg: f32,
}

impl Radials {
    /// The nearest radial's row for each azimuth bin (`u32::MAX` in a gap),
    /// so the resampling looks a radial up without a search.
    fn table(&self) -> Vec<u32> {
        (0..360 * BINS_PER_DEG)
            .map(|b| {
                let az = (b as f32 + 0.5) / BINS_PER_DEG as f32;
                self.nearest(az).map_or(u32::MAX, |r| r as u32)
            })
            .collect()
    }

    fn new(sweep: &DetectionInput) -> Self {
        let mut sorted: Vec<(f32, usize)> = sweep.azimuths[..sweep.azimuth_count]
            .iter()
            .enumerate()
            .filter(|(_, a)| **a >= 0.0)
            .map(|(i, a)| (*a, i))
            .collect();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self {
            sorted,
            max_gap_deg: radial_width_deg(sweep) * 1.5,
        }
    }

    /// Row of the radial nearest `az` (degrees, 0..360), wrap-aware, or
    /// `None` in a gap.
    fn nearest(&self, az: f32) -> Option<usize> {
        let n = self.sorted.len();
        if n == 0 {
            return None;
        }
        let p = self.sorted.partition_point(|r| r.0 < az);
        let dist = |i: usize| {
            let d = (az - self.sorted[i].0).abs();
            d.min(360.0 - d)
        };
        let (hi, lo) = (p % n, (p + n - 1) % n);
        let best = if dist(lo) <= dist(hi) { lo } else { hi };
        (dist(best) <= self.max_gap_deg).then_some(self.sorted[best].1)
    }
}

/// Resample `cuts` onto one grid around their echo at `threshold_dbz`.
/// `None` when no cut has echo at that level. Cuts past [`MAX_SITES`] are
/// left out.
pub(crate) fn resample(cuts: &[CompositeCut], threshold_dbz: f32) -> Option<CompositeGrid> {
    let cuts = &cuts[..cuts.len().min(MAX_SITES)];
    let boxes: Vec<LatLonBox> = cuts
        .iter()
        .filter_map(|c| echo_box(&c.sweep, threshold_dbz))
        .collect();
    let mean_lat = cuts.iter().map(|c| c.sweep.radar_lat).sum::<f64>() / cuts.len().max(1) as f64;
    let ext = on_lattice(
        extent(&boxes, PAD_KM, CELL_KM)?,
        mean_lat.to_radians().cos(),
    );
    let cells = ext.rows * ext.cols;
    let mut values = vec![0.0f32; cells];
    let mut mask = vec![0u8; cells];
    let mut winner = vec![u8::MAX; cells];
    let lon_km = KM_PER_DEG * ext.cos_lat;

    for (bit, cut) in cuts.iter().enumerate() {
        let s = &cut.sweep;
        if !well_formed(s) {
            continue;
        }
        let dec = decoder(s);
        let radials = Radials::new(s).table();
        let max_range = s.first_gate_km + s.gate_count as f64 * s.gate_interval_km;
        let radar_cos = radar_cos_lat(s);
        // Rows the disc can reach.
        let reach_lat = max_range / KM_PER_DEG;
        let row_of = |lat: f64| ((lat - ext.origin_lat) * KM_PER_DEG / CELL_KM).floor();
        let col_of = |lon: f64| ((lon - ext.origin_lon) * lon_km / CELL_KM).floor();
        let clamp = |v: f64, n: usize| v.clamp(0.0, n as f64 - 1.0) as usize;
        let (r0, r1) = (
            clamp(row_of(s.radar_lat - reach_lat), ext.rows),
            clamp(row_of(s.radar_lat + reach_lat), ext.rows),
        );
        let bins = 360 * BINS_PER_DEG;
        for r in r0..=r1 {
            let lat = ext.origin_lat + (r as f64 + 0.5) * CELL_KM / KM_PER_DEG;
            let y = (lat - s.radar_lat) * KM_PER_DEG;
            if y.abs() >= max_range {
                continue;
            }
            // The columns of this row inside the disc.
            let half = (max_range * max_range - y * y).sqrt() / (KM_PER_DEG * radar_cos);
            let (c0, c1) = (
                clamp(col_of(s.radar_lon - half), ext.cols),
                clamp(col_of(s.radar_lon + half), ext.cols),
            );
            for c in c0..=c1 {
                let lon = ext.origin_lon + (c as f64 + 0.5) * CELL_KM / lon_km;
                let x = (lon - s.radar_lon) * KM_PER_DEG * radar_cos;
                let range = (x * x + y * y).sqrt();
                if range < s.first_gate_km || range >= max_range {
                    continue;
                }
                let gate = ((range - s.first_gate_km) / s.gate_interval_km) as usize;
                if gate >= s.gate_count {
                    continue;
                }
                let az = x.atan2(y).to_degrees().rem_euclid(360.0);
                let row = radials[(az * BINS_PER_DEG as f64) as usize % bins];
                if row == u32::MAX {
                    continue;
                }
                let row = row as usize;
                let v = dec.decode(s.gate_values[row * s.gate_count + gate]);
                if v.is_nan() {
                    continue;
                }
                let i = r * ext.cols + c;
                mask[i] |= 1 << bit;
                // A first valid gate replaces the empty 0; later ones keep
                // the maximum.
                if mask[i] == 1 << bit || v > values[i] {
                    values[i] = v;
                    winner[i] = bit as u8;
                }
            }
        }
    }
    Some(CompositeGrid {
        extent: ext,
        values,
        mask,
        winner,
        sites: cuts
            .iter()
            .map(|c| GridSite {
                id: c.site.to_string(),
                lat: c.sweep.radar_lat,
                lon: c.sweep.radar_lon,
                time_ms: c.time_ms,
            })
            .collect(),
    })
}

/// Detect storm cells on the composite of `cuts`. Returns the result and
/// the grid's (rows, cols); `None` and an empty result when no cut has echo
/// at the threshold.
pub(crate) fn detect_composite(
    cuts: &[CompositeCut],
    params: &DetectionParams,
) -> (StormCellsResult, Option<(usize, usize)>) {
    match resample(cuts, params.threshold_dbz) {
        Some(grid) => (
            detect_cells_cartesian(&grid.input(), params),
            Some((grid.extent.rows, grid.extent.cols)),
        ),
        None => (
            StormCellsResult {
                source: super::DetectionSource {
                    params: params.clone(),
                    ..Default::default()
                },
                ..StormCellsResult::default()
            },
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::detect_cells;
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A synthetic cut: 360 one-degree radials of 1 km gates out to
    /// `gates` km, raw == physical dBZ, painted by `dbz(lat, lon)` at each
    /// gate centre (0 = no echo).
    struct Cut {
        lat: f64,
        lon: f64,
        azimuths: Vec<f32>,
        gates: Vec<f32>,
        gate_count: usize,
    }

    impl Cut {
        fn new(lat: f64, lon: f64, gate_count: usize, dbz: impl Fn(f64, f64) -> f32) -> Self {
            let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
            let cos = lat.to_radians().cos();
            let mut gates = vec![0.0; 360 * gate_count];
            for (a, &az) in azimuths.iter().enumerate() {
                let (sin, cosa) = (az as f64).to_radians().sin_cos();
                for g in 0..gate_count {
                    let r = g as f64 + 0.5;
                    let glat = lat + r * cosa / KM_PER_DEG;
                    let glon = lon + r * sin / (KM_PER_DEG * cos);
                    gates[a * gate_count + g] = dbz(glat, glon);
                }
            }
            Self {
                lat,
                lon,
                azimuths,
                gates,
                gate_count,
            }
        }

        fn sweep(&self) -> DetectionInput<'_> {
            DetectionInput {
                azimuths: &self.azimuths,
                gate_values: &self.gates,
                azimuth_count: self.azimuths.len(),
                gate_count: self.gate_count,
                first_gate_km: 0.0,
                gate_interval_km: 1.0,
                data_scale: 0.0,
                data_offset: 0.0,
                radar_lat: self.lat,
                radar_lon: self.lon,
                azimuth_spacing_deg: None,
            }
        }
    }

    /// km between two points, flat about the first.
    fn km(a: (f64, f64), b: (f64, f64)) -> f64 {
        let cos = a.0.to_radians().cos();
        ((b.0 - a.0) * KM_PER_DEG).hypot((b.1 - a.1) * KM_PER_DEG * cos)
    }

    /// A round storm of `peak` dBZ within `radius` km of `at`, 40 dBZ to
    /// twice the radius.
    fn storm(at: (f64, f64), radius: f64, peak: f32) -> impl Fn(f64, f64) -> f32 {
        move |lat, lon| {
            let d = km(at, (lat, lon));
            if d < radius {
                peak
            } else if d < 2.0 * radius {
                40.0
            } else {
                0.0
            }
        }
    }

    const STORM: (f64, f64) = (28.0, -81.5);

    #[wasm_bindgen_test]
    fn echo_box_covers_the_echo_and_nothing_without_it() {
        let cut = Cut::new(27.7, -82.4, 150, storm(STORM, 6.0, 55.0));
        let (s, w, n, e) = echo_box(&cut.sweep(), 35.0).expect("echo");
        assert!(s < STORM.0 && STORM.0 < n && w < STORM.1 && STORM.1 < e);
        // About 24 km across (40 dBZ to twice the 6 km radius), plus a gate.
        assert!(
            ((n - s) * KM_PER_DEG - 24.0).abs() < 3.0,
            "{}",
            (n - s) * 111.0
        );
        // Above every gate: no box.
        assert!(echo_box(&cut.sweep(), 60.0).is_none());
    }

    #[wasm_bindgen_test]
    fn extent_pads_the_union_and_counts_half_km_cells() {
        let a = (28.0, -82.0, 28.1, -81.9);
        let b = (28.2, -81.8, 28.3, -81.7);
        let ext = extent(&[a, b], PAD_KM, CELL_KM).unwrap();
        assert!(ext.origin_lat < 28.0 && ext.origin_lon < -82.0);
        // 0.3° of latitude is 33.3 km, plus 5 km each side, in 0.5 km rows.
        assert_eq!(ext.rows, ((33.3 + 10.0) / 0.5f64).ceil() as usize);
        let cos = 28.15f64.to_radians().cos();
        assert_eq!(
            ext.cols,
            ((0.3 * KM_PER_DEG * cos + 10.0) / 0.5).ceil() as usize
        );
        assert!(extent(&[], PAD_KM, CELL_KM).is_none());
    }

    #[wasm_bindgen_test]
    fn no_echo_anywhere_is_no_grid_and_no_cells() {
        let cut = Cut::new(27.7, -82.4, 100, |_, _| 20.0);
        let cuts = [CompositeCut {
            site: "KTBW",
            time_ms: 0,
            sweep: cut.sweep(),
        }];
        assert!(resample(&cuts, 35.0).is_none());
        let (r, size) = detect_composite(&cuts, &DetectionParams::default());
        assert!(r.is_empty() && size.is_none());
        assert_eq!(r.source.params, DetectionParams::default());
    }

    #[wasm_bindgen_test]
    fn one_radar_resamples_its_nearest_gate_and_skips_sentinels() {
        // 50 dBZ east of the radar, range-folded (raw 1) to the west.
        let (lat, lon) = (28.0, -82.0);
        let cut = Cut::new(
            lat,
            lon,
            60,
            move |_, glon| if glon > lon { 50.0 } else { 1.0 },
        );
        let cuts = [CompositeCut {
            site: "KTBW",
            time_ms: 0,
            sweep: cut.sweep(),
        }];
        let grid = resample(&cuts, 35.0).unwrap();
        let ext = grid.extent;
        let cell = |la: f64, lo: f64| {
            let r = ((la - ext.origin_lat) * KM_PER_DEG / CELL_KM) as usize;
            let c = ((lo - ext.origin_lon) * KM_PER_DEG * ext.cos_lat / CELL_KM) as usize;
            r * ext.cols + c
        };
        let east = cell(lat, lon + 20.0 / (KM_PER_DEG * ext.cos_lat));
        assert_eq!(grid.values[east], 50.0);
        assert_eq!(grid.mask[east], 1);
        // Range-folded gates are not valid: no value, no mask bit.
        let west = cell(lat, lon - 20.0 / (KM_PER_DEG * ext.cos_lat));
        assert_eq!((grid.values[west], grid.mask[west]), (0.0, 0));
        // Beyond the last gate (60 km) the radar sees nothing.
        let far = cell(lat + 0.1, lon + 59.9 / (KM_PER_DEG * ext.cos_lat));
        assert_eq!(grid.mask[far], 0);
        assert_eq!(grid.sites[0].id, "KTBW");
    }

    #[wasm_bindgen_test]
    fn overlapping_radars_keep_the_maximum_and_both_mask_bits() {
        let a = Cut::new(27.7, -82.0, 150, |_, _| 45.0);
        let b = Cut::new(28.3, -81.0, 150, |_, _| 52.0);
        let cuts = [
            CompositeCut {
                site: "KTBW",
                time_ms: 0,
                sweep: a.sweep(),
            },
            CompositeCut {
                site: "KMLB",
                time_ms: 0,
                sweep: b.sweep(),
            },
        ];
        let grid = resample(&cuts, 35.0).unwrap();
        let both: Vec<usize> = (0..grid.mask.len())
            .filter(|&i| grid.mask[i] == 0b11)
            .collect();
        assert!(!both.is_empty());
        assert!(both.iter().all(|&i| grid.values[i] == 52.0));
        // Where only the first radar reaches, its value.
        let only_a = (0..grid.mask.len())
            .find(|&i| grid.mask[i] == 0b01)
            .unwrap();
        assert_eq!(grid.values[only_a], 45.0);
    }

    #[wasm_bindgen_test]
    fn a_gap_in_the_radials_leaves_cells_empty() {
        let mut cut = Cut::new(28.0, -82.0, 50, |_, _| 50.0);
        // Drop radials 80..=100: east is a 20° gap, far over 1.5°.
        for a in 80..=100 {
            cut.azimuths[a] = -1.0;
        }
        let cuts = [CompositeCut {
            site: "KTBW",
            time_ms: 0,
            sweep: cut.sweep(),
        }];
        let grid = resample(&cuts, 35.0).unwrap();
        let ext = grid.extent;
        let r = ((28.0 - ext.origin_lat) * KM_PER_DEG / CELL_KM) as usize;
        let c = ((-82.0 + 30.0 / (KM_PER_DEG * ext.cos_lat) - ext.origin_lon)
            * KM_PER_DEG
            * ext.cos_lat
            / CELL_KM) as usize;
        assert_eq!(grid.mask[r * ext.cols + c], 0);
    }

    #[wasm_bindgen_test]
    fn a_storm_two_radars_see_is_one_cell_naming_both() {
        let a = Cut::new(27.7, -82.4, 200, storm(STORM, 6.0, 55.0));
        let b = Cut::new(28.1, -80.65, 200, storm(STORM, 6.0, 55.0));
        // Each radar alone finds the one storm.
        let params = DetectionParams::default();
        assert_eq!(detect_cells(&a.sweep(), &params).cells.len(), 1);
        assert_eq!(detect_cells(&b.sweep(), &params).cells.len(), 1);
        let cuts = [
            CompositeCut {
                site: "KTBW",
                time_ms: 0,
                sweep: a.sweep(),
            },
            CompositeCut {
                site: "KMLB",
                time_ms: 0,
                sweep: b.sweep(),
            },
        ];
        let (r, size) = detect_composite(&cuts, &params);
        let (rows, cols) = size.unwrap();
        assert!(rows < 80 && cols < 80, "{rows}x{cols}");
        assert_eq!(r.cells.len(), 1, "{:?}", r.cells);
        let cell = &r.cells[0];
        assert!(km(STORM, (cell.lat, cell.lon)) < 1.0);
        assert_eq!(cell.peak_dbz, 55.0);
        // The 6 km core reads about its true area.
        assert!(cell.core_outline.len() >= 4 && cell.outline.len() >= 4);
        assert!(
            (cell.area_km2 - std::f32::consts::PI * 144.0).abs() < 60.0,
            "{}",
            cell.area_km2
        );
        // KMLB is nearer (about 85 km against 95): it comes first and the
        // bearing and range are from it.
        assert_eq!(cell.sites, vec!["KMLB".to_string(), "KTBW".to_string()]);
        let (_, from_b) = super::super::geometry::bearing_range_from((28.1, -80.65), STORM);
        assert!((cell.range_from_radar_km - from_b).abs() < 1.0);
        assert!((240.0..300.0).contains(&cell.bearing_from_radar_deg));
    }

    #[wasm_bindgen_test]
    fn a_storm_one_radar_sees_names_that_radar_alone() {
        // KJAX's 100 km disc does not reach the storm.
        let a = Cut::new(27.7, -82.4, 200, storm(STORM, 6.0, 55.0));
        let j = Cut::new(30.48, -81.7, 100, |_, _| 0.0);
        let cuts = [
            CompositeCut {
                site: "KJAX",
                time_ms: 0,
                sweep: j.sweep(),
            },
            CompositeCut {
                site: "KTBW",
                time_ms: 0,
                sweep: a.sweep(),
            },
        ];
        let (r, _) = detect_composite(&cuts, &DetectionParams::default());
        assert_eq!(r.cells.len(), 1);
        assert_eq!(r.cells[0].sites, vec!["KTBW".to_string()]);
    }

    #[wasm_bindgen_test]
    fn each_cell_carries_its_cut_time() {
        // A storm both radars see: KTBW's cut has the stronger core, KMLB's
        // (listed first, newer) the stronger rim, which is most of the
        // footprint. A second storm only KTBW sees.
        const WEST: (f64, f64) = (27.7, -83.5);
        let rim = |at: (f64, f64), core: f32, rim: f32| {
            move |lat: f64, lon: f64| {
                let d = km(at, (lat, lon));
                if d < 6.0 {
                    core
                } else if d < 12.0 {
                    rim
                } else {
                    0.0
                }
            }
        };
        let both = rim(STORM, 55.0, 40.0);
        let west = rim(WEST, 55.0, 40.0);
        let tbw = Cut::new(27.7, -82.4, 200, move |la, lo| {
            both(la, lo).max(west(la, lo))
        });
        let mlb = Cut::new(28.1, -80.65, 200, rim(STORM, 50.0, 45.0));
        let cuts = [
            CompositeCut {
                site: "KMLB",
                time_ms: 2_000,
                sweep: mlb.sweep(),
            },
            CompositeCut {
                site: "KTBW",
                time_ms: 1_000,
                sweep: tbw.sweep(),
            },
        ];
        let grid = resample(&cuts, 35.0).unwrap();
        assert_eq!(grid.sites[0].time_ms, 2_000);
        assert_eq!(grid.sites[1].time_ms, 1_000);
        // Where only KTBW reaches, KTBW gave the value; where nobody has a
        // valid gate, nobody did.
        let only_tbw = (0..grid.mask.len())
            .find(|&i| grid.mask[i] == 0b10)
            .unwrap();
        assert_eq!(grid.winner[only_tbw], 1);
        assert!((0..grid.mask.len())
            .filter(|&i| grid.mask[i] == 0)
            .all(|i| grid.winner[i] == u8::MAX));
        let (r, _) = detect_composite(&cuts, &DetectionParams::default());
        assert_eq!(r.cells.len(), 2, "{:?}", r.cells);
        let near = |at: (f64, f64)| {
            r.cells
                .iter()
                .find(|c| km(at, (c.lat, c.lon)) < 3.0)
                .expect("cell")
        };
        assert_eq!(near(STORM).observed_ms, Some(2_000));
        assert_eq!(near(WEST).observed_ms, Some(1_000));
        // One radar's sweep carries no cut time: the result's is the sweep's.
        let polar = super::super::detect_cells(&tbw.sweep(), &DetectionParams::default());
        assert!(polar.cells.iter().all(|c| c.observed_ms.is_none()));
    }

    #[wasm_bindgen_test]
    fn a_far_cut_leaves_a_storm_it_does_not_cover_unchanged() {
        // KTBW sees the storm; KXXX's 100 km disc is far from it, and its
        // echo moves between two of its cuts, growing the grid's box west
        // and north.
        let a = Cut::new(27.7, -82.4, 150, storm(STORM, 6.0, 55.0));
        let far: (f64, f64) = (29.5, -83.6);
        let cos = far.0.to_radians().cos();
        let patch = |north: f64, east: f64| {
            storm(
                (
                    far.0 + north / KM_PER_DEG,
                    far.1 + east / (KM_PER_DEG * cos),
                ),
                4.0,
                50.0,
            )
        };
        let b1 = Cut::new(far.0, far.1, 100, patch(20.0, 0.0));
        let b2 = Cut::new(far.0, far.1, 100, patch(30.0, -60.0));
        let cell_near_storm = |b: &Cut| {
            let cuts = [
                CompositeCut {
                    site: "KTBW",
                    time_ms: 0,
                    sweep: a.sweep(),
                },
                CompositeCut {
                    site: "KXXX",
                    time_ms: 0,
                    sweep: b.sweep(),
                },
            ];
            let grid = resample(&cuts, 35.0).unwrap();
            let (r, _) = detect_composite(&cuts, &DetectionParams::default());
            let cell = r
                .cells
                .into_iter()
                .find(|c| km(STORM, (c.lat, c.lon)) < 3.0)
                .expect("the storm");
            (grid.extent, cell)
        };
        let (e1, c1) = cell_near_storm(&b1);
        let (e2, c2) = cell_near_storm(&b2);
        // The boxes differ, the lattice does not.
        assert!(e2.origin_lon < e1.origin_lon && e2.cols > e1.cols);
        assert_eq!(e1.cos_lat, e2.cos_lat);
        let step = CELL_KM / (KM_PER_DEG * e1.cos_lat);
        let shift = (e1.origin_lon - e2.origin_lon) / step;
        assert!((shift - shift.round()).abs() < 1e-6, "{shift}");
        assert!((c1.lat - c2.lat).abs() < 1e-9 && (c1.lon - c2.lon).abs() < 1e-9);
        assert_eq!(c1.area_km2, c2.area_km2);
        assert_eq!(c1.peak_dbz, c2.peak_dbz);
        assert_eq!(c1.gate_count, c2.gate_count);
    }
}
