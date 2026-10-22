//! Vertical structure of the cells: each cell's column through the volume.
//!
//! Cells are found on the volume's lowest reflectivity cut. Their columns
//! come from the higher tilts sampled over the same ground footprint: every
//! gate of a higher tilt is placed on the ground with the standard 4/3
//! effective earth radius beam model and looked up in a raster of the
//! lowest cut's footprints (by azimuth and ground range). So a column is the
//! echo stacked over a cell's footprint, not a 3D object of its own: a storm
//! that leans far outside its low-level footprint is sampled only where it
//! overhangs it.
//!
//! Per cell and tilt the sampler keeps the strongest echo and the highest
//! sample at or above [`ECHO_TOP_DBZ`]. Values pass a three-gate median
//! along the radial first, so a single-gate spike cannot set a column's peak
//! or its top, and a tilt counts towards the echo top only with at least
//! [`MIN_ECHO_TOP_GATES`] such gates in the footprint.
//!
//! The crate's `nexrad-process` has a composite reflectivity product and
//! `nexrad-model` a beam height model, but both work on its `Scan` /
//! `SweepField` / `Site` types and a Cartesian output grid; this leaf takes
//! raw gate arrays and stays `std`-only, so the two small formulas live here.

use super::components::Pixel;
use super::model::{CellColumn, VolumeStructure, VolumeTilt};
use super::DetectionInput;

/// Echo top threshold (dBZ): the NWS enhanced echo top uses 18 dBZ.
pub(crate) const ECHO_TOP_DBZ: f32 = 18.0;

/// A tilt's echo top sample counts only with this many gates at or above
/// [`ECHO_TOP_DBZ`] in the footprint (after the radial median).
pub(crate) const MIN_ECHO_TOP_GATES: u32 = 3;

/// Standard 4/3 effective earth radius, km.
const EFFECTIVE_EARTH_RADIUS_KM: f64 = 6371.0 * 4.0 / 3.0;

/// Height of the beam centre above the radar (km) at `slant_km` along a beam
/// raised `elevation_deg` above the horizon (4/3 earth radius model).
pub(crate) fn beam_height_km(slant_km: f64, elevation_deg: f64) -> f64 {
    let re = EFFECTIVE_EARTH_RADIUS_KM;
    let el = elevation_deg.to_radians();
    (slant_km * slant_km + re * re + 2.0 * slant_km * re * el.sin()).sqrt() - re
}

/// Distance along the ground (km) from the radar to the point under the
/// beam centre at `slant_km` (4/3 earth radius model).
pub(crate) fn ground_range_km(slant_km: f64, elevation_deg: f64) -> f64 {
    let re = EFFECTIVE_EARTH_RADIUS_KM;
    let h = beam_height_km(slant_km, elevation_deg);
    re * ((slant_km * elevation_deg.to_radians().cos()) / (re + h)).asin()
}

/// Raw gate values of one tilt, in the word size it was stored with.
#[derive(Clone, Copy)]
pub(crate) enum TiltValues<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    F32(&'a [f32]),
}

impl TiltValues<'_> {
    fn len(&self) -> usize {
        match self {
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
            Self::F32(v) => v.len(),
        }
    }

    #[inline]
    fn raw(&self, i: usize) -> f32 {
        match self {
            Self::U8(v) => v[i] as f32,
            Self::U16(v) => v[i] as f32,
            Self::F32(v) => v[i],
        }
    }
}

/// Borrowed view of one reflectivity tilt of the volume.
pub(crate) struct TiltInput<'a> {
    pub elevation_number: u8,
    /// Measured elevation angle, degrees.
    pub elevation_deg: f32,
    /// Azimuths (degrees); negative values mark padding and are skipped.
    pub azimuths: &'a [f32],
    /// Row-major as `az_idx * gate_count + gate_idx`; sentinels 0 and 1.
    pub values: TiltValues<'a>,
    pub azimuth_count: usize,
    pub gate_count: usize,
    pub first_gate_km: f64,
    pub gate_interval_km: f64,
    /// `physical = (raw - offset) / scale`; `scale == 0` means raw is
    /// already physical.
    pub data_scale: f32,
    pub data_offset: f32,
}

/// No cell at this lowest-cut gate.
const NONE: u16 = u16::MAX;
/// Azimuth lookup resolution: bins per degree.
const AZ_BINS_PER_DEG: usize = 10;

/// Per cell and tilt, what the sampler has seen.
#[derive(Clone, Copy, Default)]
struct TiltAcc {
    /// Strongest filtered value and the beam height it was seen at.
    max: Option<(f32, f32)>,
    /// Gates at or above the echo top threshold, and the highest of them.
    top_gates: u32,
    top_km: f32,
}

/// Samples tilts over the footprints of the cells detected on the lowest
/// cut. Built by [`super::detect_cells_for_volume`]; feed it each higher
/// tilt with [`VolumeSampler::add_tilt`] (one at a time, so a caller never
/// holds the whole volume) and [`VolumeSampler::finish`] for the columns.
pub(crate) struct VolumeSampler {
    cell_count: usize,
    /// Lowest-cut gate → cell id, `NONE` outside every footprint.
    labels: Vec<u16>,
    gate_count: usize,
    /// Ground range of each lowest-cut gate centre, ascending.
    ground_km: Vec<f64>,
    /// Azimuth bin → nearest lowest-cut radial, `None` across a gap.
    az_lookup: Vec<Option<u32>>,
    /// Lowest-cut radials holding any footprint gate.
    row_used: Vec<bool>,
    /// Lowest-cut gate span that holds footprint gates.
    gate_span: (usize, usize),
    tilts: Vec<VolumeTilt>,
    /// `acc[tilt][cell]`.
    acc: Vec<Vec<TiltAcc>>,
}

impl VolumeSampler {
    /// A sampler over `footprints[cell_id]` of the lowest cut `input`
    /// (taken at `elevation_deg`, elevation number `elevation_number`),
    /// which is sampled as the first tilt.
    pub(super) fn new(
        input: &DetectionInput,
        elevation_number: u8,
        elevation_deg: f32,
        footprints: &[Vec<Pixel>],
    ) -> Self {
        let gate_count = input.gate_count;
        let azimuth_count = input.azimuth_count;
        let mut labels = vec![NONE; azimuth_count * gate_count];
        let mut row_used = vec![false; azimuth_count];
        let mut gate_span = (usize::MAX, 0);
        for (id, pixels) in footprints.iter().enumerate().take(NONE as usize) {
            for &(a, g) in pixels {
                let (a, g) = (a as usize, g as usize);
                labels[a * gate_count + g] = id as u16;
                row_used[a] = true;
                gate_span = (gate_span.0.min(g), gate_span.1.max(g + 1));
            }
        }
        let el = elevation_deg as f64;
        let ground_km = (0..gate_count)
            .map(|g| {
                let slant = input.first_gate_km + (g as f64 + 0.5) * input.gate_interval_km;
                ground_range_km(slant, el)
            })
            .collect();
        let spacing = super::components::median_spacing_deg(input.azimuths, azimuth_count);
        let az_lookup = az_lookup(&input.azimuths[..azimuth_count], spacing);
        let mut sampler = Self {
            cell_count: footprints.len(),
            labels,
            gate_count,
            ground_km,
            az_lookup,
            row_used,
            gate_span,
            tilts: Vec::new(),
            acc: Vec::new(),
        };
        sampler.add_tilt(&TiltInput {
            elevation_number,
            elevation_deg,
            azimuths: input.azimuths,
            values: TiltValues::F32(input.gate_values),
            azimuth_count,
            gate_count,
            first_gate_km: input.first_gate_km,
            gate_interval_km: input.gate_interval_km,
            data_scale: input.data_scale,
            data_offset: input.data_offset,
        });
        sampler
    }

    /// Sample one tilt over the footprints. A tilt whose buffers are short
    /// is skipped.
    pub(crate) fn add_tilt(&mut self, tilt: &TiltInput) {
        let gc = tilt.gate_count;
        if tilt.azimuths.len() < tilt.azimuth_count || tilt.values.len() < tilt.azimuth_count * gc {
            return;
        }
        let mut acc = vec![TiltAcc::default(); self.cell_count];
        let el = tilt.elevation_deg as f64;
        // Tilt gate → lowest-cut gate (by ground range), and its height.
        let (lo, hi) = self.gate_span;
        let mut map: Vec<u32> = Vec::with_capacity(gc);
        let mut height_km: Vec<f32> = Vec::with_capacity(gc);
        let mut span = (usize::MAX, 0);
        for g in 0..gc {
            let slant = tilt.first_gate_km + (g as f64 + 0.5) * tilt.gate_interval_km;
            let ground = ground_range_km(slant, el);
            let lg = nearest_gate(&self.ground_km, ground);
            match lg.filter(|&lg| (lo..hi).contains(&lg)) {
                Some(lg) => {
                    map.push(lg as u32);
                    span = (span.0.min(g), span.1.max(g + 1));
                }
                None => map.push(u32::MAX),
            }
            height_km.push(beam_height_km(slant, el) as f32);
        }
        if span.0 < span.1 && self.cell_count > 0 {
            let decode = |raw: f32| -> f32 {
                if raw <= 1.0 {
                    f32::NAN
                } else if tilt.data_scale == 0.0 {
                    raw
                } else {
                    (raw - tilt.data_offset) / tilt.data_scale
                }
            };
            let bins = self.az_lookup.len();
            for a in 0..tilt.azimuth_count {
                let az = tilt.azimuths[a];
                if az.is_nan() || az < 0.0 {
                    continue;
                }
                let bin = (az.rem_euclid(360.0) as f64 * AZ_BINS_PER_DEG as f64) as usize % bins;
                let Some(la) = self.az_lookup[bin] else {
                    continue;
                };
                let la = la as usize;
                if !self.row_used[la] {
                    continue;
                }
                let row = a * gc;
                let lrow = la * self.gate_count;
                // Values of the gates either side of g, for the radial median.
                let value = |g: usize| decode(tilt.values.raw(row + g));
                let mut prev = if span.0 > 0 {
                    value(span.0 - 1)
                } else {
                    f32::NAN
                };
                let mut cur = value(span.0);
                for g in span.0..span.1 {
                    let next = if g + 1 < gc { value(g + 1) } else { f32::NAN };
                    let lg = map[g];
                    if lg != u32::MAX && !cur.is_nan() {
                        let id = self.labels[lrow + lg as usize];
                        if id != NONE {
                            let v = median3(prev, cur, next);
                            if !v.is_nan() {
                                let h = height_km[g];
                                let c = &mut acc[id as usize];
                                if c.max.is_none_or(|(m, _)| v > m) {
                                    c.max = Some((v, h));
                                }
                                if v >= ECHO_TOP_DBZ {
                                    c.top_gates += 1;
                                    c.top_km = c.top_km.max(h);
                                }
                            }
                        }
                    }
                    prev = cur;
                    cur = next;
                }
            }
        }
        self.tilts.push(VolumeTilt {
            elevation_number: tilt.elevation_number,
            elevation_deg: tilt.elevation_deg,
        });
        self.acc.push(acc);
    }

    /// The columns, tilts ordered by angle (lowest first). `complete` says
    /// whether these are all the volume's tilts; the caller knows the plan.
    pub(crate) fn finish(self, complete: bool) -> VolumeStructure {
        let mut order: Vec<usize> = (0..self.tilts.len()).collect();
        order.sort_by(|&a, &b| {
            self.tilts[a]
                .elevation_deg
                .total_cmp(&self.tilts[b].elevation_deg)
                .then(
                    self.tilts[a]
                        .elevation_number
                        .cmp(&self.tilts[b].elevation_number),
                )
        });
        let top_tilt = order.last().copied();
        let cells = (0..self.cell_count)
            .map(|c| {
                let mut column = CellColumn::default();
                let mut top: Option<(f32, usize)> = None;
                for &t in &order {
                    let a = self.acc[t][c];
                    column.profile.push(a.max.map(|(v, _)| v));
                    if let Some((v, h)) = a.max {
                        if column.max_dbz.is_none_or(|m| v > m) {
                            column.max_dbz = Some(v);
                            column.max_dbz_height_km = Some(h);
                        }
                    }
                    if a.top_gates >= MIN_ECHO_TOP_GATES && top.is_none_or(|(h, _)| a.top_km > h) {
                        top = Some((a.top_km, t));
                    }
                }
                column.echo_top_km = top.map(|(h, _)| h);
                // Echo at the top tilt: the top is above what was sampled.
                column.echo_top_capped = top_tilt.is_some_and(|tt| {
                    self.tilts.len() > 1 && self.acc[tt][c].top_gates >= MIN_ECHO_TOP_GATES
                });
                column
            })
            .collect();
        VolumeStructure {
            tilts: order.iter().map(|&t| self.tilts[t].clone()).collect(),
            cells,
            complete,
        }
    }
}

/// Median of three values with NaN (no data) lowest: the gate's value when a
/// neighbour along the radial is at least as strong, else the stronger
/// neighbour. NaN when two of the three have no data.
#[inline]
fn median3(a: f32, b: f32, c: f32) -> f32 {
    let key = |v: f32| if v.is_nan() { f32::NEG_INFINITY } else { v };
    let (ka, kb, kc) = (key(a), key(b), key(c));
    let m = ka.max(kb).min(ka.min(kb).max(kc));
    if m == f32::NEG_INFINITY {
        f32::NAN
    } else {
        m
    }
}

/// Index of the gate whose centre is nearest `ground` (km), `None` beyond
/// half a gate outside the cut.
fn nearest_gate(ground_km: &[f64], ground: f64) -> Option<usize> {
    let n = ground_km.len();
    if n == 0 {
        return None;
    }
    let half = if n > 1 {
        (ground_km[1] - ground_km[0]) * 0.5
    } else {
        f64::INFINITY
    };
    if ground < ground_km[0] - half || ground > ground_km[n - 1] + half {
        return None;
    }
    let i = ground_km.partition_point(|&x| x < ground);
    if i == 0 {
        return Some(0);
    }
    if i == n {
        return Some(n - 1);
    }
    Some(if ground - ground_km[i - 1] <= ground_km[i] - ground {
        i - 1
    } else {
        i
    })
}

/// For each 1/[`AZ_BINS_PER_DEG`]° bin, the nearest radial of `azimuths`
/// within one radial spacing, so a gap in a partial cut stays a gap.
fn az_lookup(azimuths: &[f32], spacing_deg: f64) -> Vec<Option<u32>> {
    let mut sorted: Vec<(f64, u32)> = azimuths
        .iter()
        .enumerate()
        .filter(|(_, a)| **a >= 0.0)
        .map(|(i, a)| ((*a as f64).rem_euclid(360.0), i as u32))
        .collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let bins = 360 * AZ_BINS_PER_DEG;
    if sorted.is_empty() {
        return vec![None; bins];
    }
    let reach = if spacing_deg > 0.0 { spacing_deg } else { 1.0 };
    let angular = |a: f64, b: f64| {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    };
    (0..bins)
        .map(|b| {
            let az = (b as f64 + 0.5) / AZ_BINS_PER_DEG as f64;
            let i = sorted.partition_point(|x| x.0 < az);
            let after = sorted[i % sorted.len()];
            let before = sorted[(i + sorted.len() - 1) % sorted.len()];
            let (d_after, d_before) = (angular(after.0, az), angular(before.0, az));
            let (d, idx) = if d_before <= d_after {
                (d_before, before.1)
            } else {
                (d_after, after.1)
            };
            (d <= reach).then_some(idx)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{detect_cells_for_volume, DetectionParams};
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// A synthetic volume over 1 km gates and 1° radials, raw == dBZ.
    struct Field {
        az: Vec<f32>,
        gates: Vec<f32>,
        gate_count: usize,
    }

    fn field(gate_count: usize, f: impl Fn(usize, usize) -> f32) -> Field {
        let az: Vec<f32> = (0..360).map(|i| i as f32).collect();
        let mut gates = vec![0.0; 360 * gate_count];
        for a in 0..360 {
            for g in 0..gate_count {
                gates[a * gate_count + g] = f(a, g);
            }
        }
        Field {
            az,
            gates,
            gate_count,
        }
    }

    fn lowest(f: &Field) -> DetectionInput<'_> {
        DetectionInput {
            azimuths: &f.az,
            gate_values: &f.gates,
            azimuth_count: f.az.len(),
            gate_count: f.gate_count,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            data_scale: 0.0,
            data_offset: 0.0,
            radar_lat: 41.7,
            radar_lon: -93.7,
            azimuth_spacing_deg: None,
        }
    }

    fn tilt(f: &Field, n: u8, deg: f32) -> TiltInput<'_> {
        TiltInput {
            elevation_number: n,
            elevation_deg: deg,
            azimuths: &f.az,
            values: TiltValues::F32(&f.gates),
            azimuth_count: f.az.len(),
            gate_count: f.gate_count,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            data_scale: 0.0,
            data_offset: 0.0,
        }
    }

    /// A storm: azimuths 40..50, ground ranges 60..70 km.
    fn storm(a: usize, g: usize, dbz: f32) -> f32 {
        if (40..50).contains(&a) && (60..70).contains(&g) {
            dbz
        } else {
            0.0
        }
    }

    #[wasm_bindgen_test]
    fn beam_height_matches_the_four_thirds_model() {
        // 0.5° at 100 km: ~0.87 km beam rise + ~0.59 km of earth curvature.
        let h = beam_height_km(100.0, 0.5);
        assert!((h - 1.46).abs() < 0.02, "{h}");
        assert!(beam_height_km(100.0, 10.0) > 17.0);
        assert!((ground_range_km(100.0, 0.5) - 99.99).abs() < 0.05);
        // A steep beam covers less ground.
        assert!(ground_range_km(100.0, 19.5) < 95.0);
    }

    #[wasm_bindgen_test]
    fn columns_read_echo_top_and_the_height_of_the_peak() {
        let low = field(120, |a, g| storm(a, g, 50.0));
        // 4°: 60 dBZ over the storm; 10°: 25 dBZ; 15°: nothing.
        let mid = field(120, |a, g| storm(a, g, 60.0));
        let high = field(120, |a, g| storm(a, g, 25.0));
        let empty = field(120, |_, _| 0.0);
        let input = lowest(&low);
        let (result, mut sampler) =
            detect_cells_for_volume(&input, &DetectionParams::default(), 1, 0.5);
        assert_eq!(result.cells.len(), 1);
        sampler.add_tilt(&tilt(&high, 3, 10.0));
        sampler.add_tilt(&tilt(&empty, 4, 15.0));
        sampler.add_tilt(&tilt(&mid, 2, 4.0));
        let v = sampler.finish(true);
        let angles: Vec<f32> = v.tilts.iter().map(|t| t.elevation_deg).collect();
        assert_eq!(angles, vec![0.5, 4.0, 10.0, 15.0], "sorted by angle");
        let c = &v.cells[0];
        assert_eq!(c.profile, vec![Some(50.0), Some(60.0), Some(25.0), None]);
        assert_eq!(c.max_dbz, Some(60.0));
        // The 60 dBZ sits on the 4° tilt over 60-70 km: ~4.4-5.2 km up.
        let h = c.max_dbz_height_km.unwrap();
        assert!((4.0..5.5).contains(&h), "{h}");
        // The echo top is the 10° tilt's highest gate over the footprint.
        let top = c.echo_top_km.unwrap();
        assert!((11.0..13.5).contains(&top), "{top}");
        assert!(!c.echo_top_capped);
        assert!(v.complete);
    }

    #[wasm_bindgen_test]
    fn echo_at_the_top_tilt_marks_the_top_as_a_lower_bound() {
        let low = field(120, |a, g| storm(a, g, 50.0));
        let high = field(120, |a, g| storm(a, g, 30.0));
        let input = lowest(&low);
        let (_, mut sampler) = detect_cells_for_volume(&input, &DetectionParams::default(), 1, 0.5);
        sampler.add_tilt(&tilt(&high, 2, 6.0));
        let v = sampler.finish(false);
        assert!(v.cells[0].echo_top_capped);
        assert!(!v.complete);
    }

    #[wasm_bindgen_test]
    fn a_single_gate_spike_sets_neither_peak_nor_top() {
        let low = field(120, |a, g| storm(a, g, 50.0));
        // 8°: a lone 70 dBZ gate over the storm, nothing else.
        let spike = field(120, |a, g| if a == 45 && g == 65 { 70.0 } else { 0.0 });
        let input = lowest(&low);
        let (_, mut sampler) = detect_cells_for_volume(&input, &DetectionParams::default(), 1, 0.5);
        sampler.add_tilt(&tilt(&spike, 2, 8.0));
        let c = &sampler.finish(true).cells[0];
        assert_eq!(c.max_dbz, Some(50.0));
        assert_eq!(c.profile[1], None);
        // The top comes from the lowest tilt alone.
        assert!(c.echo_top_km.unwrap() < 2.0);
    }

    #[wasm_bindgen_test]
    fn echo_outside_the_footprint_does_not_count() {
        let low = field(120, |a, g| storm(a, g, 50.0));
        // 5°: strong echo, but 30° of azimuth away from the storm.
        let aside = field(120, |a, g| storm((a + 330) % 360, g, 65.0));
        let input = lowest(&low);
        let (_, mut sampler) = detect_cells_for_volume(&input, &DetectionParams::default(), 1, 0.5);
        sampler.add_tilt(&tilt(&aside, 2, 5.0));
        let c = &sampler.finish(true).cells[0];
        assert_eq!(c.profile, vec![Some(50.0), None]);
        assert_eq!(c.max_dbz, Some(50.0));
    }

    #[wasm_bindgen_test]
    fn two_storms_keep_their_own_columns() {
        // A tall storm at 40-50° and a shallow one at 200-210°.
        let other = |a: usize, g: usize, dbz: f32| storm((a + 200) % 360, g, dbz);
        let low = field(120, |a, g| storm(a, g, 55.0).max(other(a, g, 50.0)));
        let high = field(120, |a, g| storm(a, g, 40.0));
        let input = lowest(&low);
        let (result, mut sampler) =
            detect_cells_for_volume(&input, &DetectionParams::default(), 1, 0.5);
        assert_eq!(result.cells.len(), 2);
        sampler.add_tilt(&tilt(&high, 2, 9.0));
        let v = sampler.finish(true);
        let tall = &v.cells[0];
        let shallow = &v.cells[1];
        assert_eq!(result.cells[0].peak_dbz, 55.0);
        assert_eq!(tall.profile, vec![Some(55.0), Some(40.0)]);
        assert_eq!(shallow.profile, vec![Some(50.0), None]);
        assert!(tall.echo_top_km.unwrap() > shallow.echo_top_km.unwrap() + 5.0);
    }

    #[wasm_bindgen_test]
    fn coarser_upper_tilts_and_stored_codes_map_onto_the_footprint() {
        let low = field(240, |a, g| storm(a, g / 2, 50.0));
        // The lowest cut at 0.5 km gates; the upper tilt in u8 codes
        // (scale 2, offset 66) at 1 km gates and 2° radials.
        let input = DetectionInput {
            gate_interval_km: 0.5,
            ..lowest(&low)
        };
        let az: Vec<f32> = (0..180).map(|i| i as f32 * 2.0 + 1.0).collect();
        let mut codes = vec![0u8; 180 * 120];
        for (i, a) in az.iter().enumerate() {
            for g in 0..120 {
                if storm(*a as usize, g, 1.0) > 0.0 {
                    codes[i * 120 + g] = 186; // (186 - 66) / 2 = 60 dBZ
                }
            }
        }
        let (_, mut sampler) = detect_cells_for_volume(&input, &DetectionParams::default(), 1, 0.5);
        sampler.add_tilt(&TiltInput {
            elevation_number: 2,
            elevation_deg: 3.0,
            azimuths: &az,
            values: TiltValues::U8(&codes),
            azimuth_count: 180,
            gate_count: 120,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            data_scale: 2.0,
            data_offset: 66.0,
        });
        let c = &sampler.finish(true).cells[0];
        assert_eq!(c.profile[1], Some(60.0));
    }

    #[wasm_bindgen_test]
    fn short_buffers_and_gaps_are_skipped() {
        let low = field(120, |a, g| storm(a, g, 50.0));
        let input = lowest(&low);
        let (_, mut sampler) = detect_cells_for_volume(&input, &DetectionParams::default(), 1, 0.5);
        let az = vec![45.0_f32; 2];
        sampler.add_tilt(&TiltInput {
            elevation_number: 2,
            elevation_deg: 3.0,
            azimuths: &az,
            values: TiltValues::F32(&[60.0; 10]),
            azimuth_count: 2,
            gate_count: 120,
            first_gate_km: 0.0,
            gate_interval_km: 1.0,
            data_scale: 0.0,
            data_offset: 0.0,
        });
        // The short tilt is not recorded at all.
        assert_eq!(sampler.finish(true).tilts.len(), 1);

        // Radials far from any lowest-cut radial do not map.
        let lookup = az_lookup(&[10.0, 11.0, 12.0], 1.0);
        assert_eq!(lookup[100], Some(0));
        assert_eq!(lookup[1805], None);
    }

    #[wasm_bindgen_test]
    fn median3_rejects_lone_gates() {
        let nan = f32::NAN;
        assert_eq!(median3(10.0, 70.0, 12.0), 12.0);
        assert_eq!(median3(nan, 70.0, 60.0), 60.0);
        assert!(median3(nan, 70.0, nan).is_nan());
        assert_eq!(median3(50.0, 50.0, nan), 50.0);
    }
}
