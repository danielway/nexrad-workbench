//! Storm cell detection.
//!
//! A leaf module: it imports nothing from the rest of the crate (only `std`,
//! plus serde derives so results can cross the worker boundary), so it can
//! move upstream into the nexrad crates. The decode worker's `detect` entry
//! point (`nexrad::decode::worker_api::detect`) feeds it a stored or live
//! sweep, and `core` re-exports its result types.
//!
//! Pipeline, all in index space (rows × columns, here azimuth × gate). Only
//! the geometry (`geometry`) knows where a pixel is and how big.
//!
//! 1. Decode raw gate codes to dBZ and apply a 3×3 median filter
//!    (`median`), so single-gate spikes cannot set a cell's peak. The peak of a cell is the maximum of the filtered field.
//! 2. Label 8-connected components (with azimuth wrap) at a ladder of
//!    levels from the user's envelope threshold up in 5 dB steps, build the
//!    containment tree and resolve it into cells and groups (`segment`).
//! 3. Share each envelope among its cells (`watershed`): every cell's core
//!    (its component at the level where it became distinct) floods outward
//!    over the envelope, strongest echo first, so a cell's footprint is its
//!    storm's share of the envelope and the cells of a group tile it. A lone
//!    cell's footprint is its whole envelope.
//! 4. Summarize each cell's and group's footprint (`features`) and trace
//!    its outline (`outline`).
//!
//! The envelope threshold (the user's slider, default 35 dBZ) is the weakest
//! echo that can belong to a cell; a cell needs a core of envelope + 10 dB.

mod components;
#[cfg(test)]
mod corpus_tests;
mod features;
mod geometry;
mod median;
mod model;
mod outline;
mod segment;
pub(crate) mod track;
mod watershed;

use geometry::Grid;
pub(crate) use model::{DetectionSource, StormCell, StormCellsResult, StormGroup};

/// Approximate kilometres per degree of latitude: the flat
/// (equirectangular) projection around the radar that the 2D canvas uses.
const KM_PER_DEG: f64 = 111.0;

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
    /// Angular width of one radial in degrees. The renderer knows the
    /// sweep's real spacing; a partial live sweep holds only the radials
    /// received so far, so `360 / azimuth_count` would overstate it. When
    /// `None`, the median gap between the supplied azimuths is used.
    pub azimuth_spacing_deg: Option<f32>,
}

/// Tuning knobs for the detector.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DetectionParams {
    /// Envelope level in dBZ (the user's threshold): the weakest echo that
    /// can belong to a cell, and the bottom of the level ladder.
    pub threshold_dbz: f32,
    /// Spacing of the level ladder.
    pub level_step_dbz: f32,
    /// Highest ladder level (inclusive).
    pub top_level_dbz: f32,
    /// A component (envelope or child) needs at least this much area.
    /// Area is summed from gate footprints, so the floor means the same
    /// ground size at every range.
    pub min_area_km2: f32,
    /// ...and at least this many gates.
    pub min_gate_count: u32,
    /// A child component qualifies only if its peak is at least this far
    /// above its own level. Drops stratiform local maxima that barely clear
    /// a level. 3 dB rather than a full 5 dB rung: on the corpus the
    /// median-filtered peaks of adjacent supercells (Calhoun and Carroll on
    /// the Greenfield line) sit about 3.5 dB above the level that separates
    /// them, and a full rung would merge them.
    pub prominence_dbz: f32,
    /// A component with no qualifying children is a cell only if its peak
    /// reaches `threshold_dbz + core_margin_dbz`.
    pub core_margin_dbz: f32,
}

impl Default for DetectionParams {
    fn default() -> Self {
        Self {
            threshold_dbz: 35.0,
            level_step_dbz: 5.0,
            top_level_dbz: 65.0,
            min_area_km2: 10.0,
            min_gate_count: 4,
            prominence_dbz: 3.0,
            core_margin_dbz: 10.0,
        }
    }
}

/// Run detection over the provided sweep. The result's `source` carries
/// `params`; the caller fills in the sweep identity it knows.
pub(crate) fn detect_cells(input: &DetectionInput, params: &DetectionParams) -> StormCellsResult {
    detect(input.into(), params)
}

fn detect(input: Grid<'_>, params: &DetectionParams) -> StormCellsResult {
    let mut result = StormCellsResult {
        source: DetectionSource {
            params: params.clone(),
            ..DetectionSource::default()
        },
        ..StormCellsResult::default()
    };
    let Some(values) = input.values() else {
        return result;
    };
    let (_, gate_count) = input.shape();

    let az_adjacent = input.row_adjacency();
    let decoder = input.decoder();
    let row_valid = input.row_valid();
    let filtered = median::median3x3(
        values,
        &decoder,
        &row_valid,
        &az_adjacent,
        gate_count,
        params.threshold_dbz,
    );

    let gate_area_km2 = input.column_area_km2();

    let seg = segment::segment(&filtered, &az_adjacent, gate_count, &gate_area_km2, params);
    let half_spacing = input.half_spacing_deg();
    let mut shed = watershed::Watershed::new(&filtered, &az_adjacent, gate_count);
    // A lone storm's cell is its envelope; its drawn core is the component
    // around its peak at the core level (the lowest rung at or above the
    // envelope + core margin), labeled here because segmentation keeps
    // only components that split.
    let core_level = seg
        .levels
        .iter()
        .copied()
        .find(|l| *l >= params.threshold_dbz + params.core_margin_dbz);
    let mut core_labeler = components::Labeler::new(&filtered, &az_adjacent, gate_count);

    // Cells, tagged with the index of their envelope while it is a group.
    let mut cells: Vec<(StormCell, Option<usize>)> = Vec::new();
    let mut groups: Vec<StormGroup> = Vec::new();
    for (envelope, members) in &seg.envelopes {
        let group_idx = (members.len() >= 2).then(|| {
            let comp = &seg.components[*envelope];
            // The centroid weight is measured from the envelope level (the
            // lowest dBZ a member gate can have).
            let fp = features::summarize(&comp.pixels, &filtered, input, params.threshold_dbz);
            groups.push(StormGroup {
                id: 0,
                level_dbz: params.threshold_dbz,
                peak_dbz: comp.peak,
                area_km2: fp.area_km2,
                cell_ids: Vec::new(),
                lat: fp.lat,
                lon: fp.lon,
                outline: outline::outline(
                    &comp.pixels,
                    input,
                    half_spacing,
                    outline::GROUP_TOLERANCE_KM,
                ),
            });
            groups.len() - 1
        });
        let cores: Vec<&[components::Pixel]> = members
            .iter()
            .map(|&c| seg.components[c].pixels.as_slice())
            .collect();
        let footprints = shed.partition(
            &seg.components[*envelope].pixels,
            &cores,
            params.threshold_dbz,
        );
        for (&c, pixels) in members.iter().zip(&footprints) {
            let level = seg.levels[seg.components[c].level_idx];
            // Weights are measured from the envelope level, the weakest
            // value a footprint gate can have, as for groups.
            let fp = features::summarize(pixels, &filtered, input, params.threshold_dbz);
            // The drawn core: the component the cell was found at when that
            // is above the envelope (every grouped cell), else the core-level
            // component around the footprint's peak. None only if the peak
            // never reaches the core level, which the core minimum rules out.
            let core: Option<Vec<components::Pixel>> = if level > params.threshold_dbz {
                Some(seg.components[c].pixels.clone())
            } else {
                let peak_gate = pixels
                    .iter()
                    .map(|&(a, g)| a as usize * gate_count + g as usize)
                    .max_by(|&x, &y| filtered[x].total_cmp(&filtered[y]));
                core_level
                    .zip(peak_gate)
                    .and_then(|(l, seed)| core_labeler.fill(seed, l, 1))
            };
            let cell = StormCell {
                id: 0,
                group_id: None,
                level_dbz: level,
                peak_dbz: fp.max_dbz,
                mean_dbz: fp.mean_dbz,
                area_km2: fp.area_km2,
                gate_count: fp.gate_count,
                lat: fp.lat,
                lon: fp.lon,
                bounds: fp.bounds,
                bearing_from_radar_deg: fp.bearing_from_radar_deg,
                range_from_radar_km: fp.range_from_radar_km,
                orientation_deg: fp.orientation_deg,
                elongation: fp.elongation,
                // A lone cell's footprint is its envelope, drawn as a faint
                // ring like a group's, so it is simplified like one.
                outline: outline::outline(
                    pixels,
                    input,
                    half_spacing,
                    if group_idx.is_some() {
                        outline::TOLERANCE_KM
                    } else {
                        outline::GROUP_TOLERANCE_KM
                    },
                ),
                core_outline: core.map_or_else(Vec::new, |px| {
                    outline::outline(&px, input, half_spacing, outline::TOLERANCE_KM)
                }),
            };
            cells.push((cell, group_idx));
        }
    }

    // Strongest first, then largest, then position, so ids are deterministic.
    let order = |a: (f32, f32, f64, f64), b: (f32, f32, f64, f64)| {
        b.0.total_cmp(&a.0)
            .then(b.1.total_cmp(&a.1))
            .then(a.2.total_cmp(&b.2))
            .then(a.3.total_cmp(&b.3))
    };
    let mut group_order: Vec<usize> = (0..groups.len()).collect();
    group_order.sort_by(|&a, &b| {
        let key = |g: &StormGroup| (g.peak_dbz, g.area_km2, g.lat, g.lon);
        order(key(&groups[a]), key(&groups[b]))
    });
    let mut group_id = vec![0u32; groups.len()];
    for (new_id, &old) in group_order.iter().enumerate() {
        group_id[old] = new_id as u32;
    }
    cells.sort_by(|(a, _), (b, _)| {
        order(
            (a.peak_dbz, a.area_km2, a.lat, a.lon),
            (b.peak_dbz, b.area_km2, b.lat, b.lon),
        )
    });
    for (i, (cell, group)) in cells.iter_mut().enumerate() {
        cell.id = i as u32;
        if let Some(g) = group {
            cell.group_id = Some(group_id[*g]);
            groups[*g].cell_ids.push(i as u32);
        }
    }
    for (old, g) in groups.iter_mut().enumerate() {
        g.id = group_id[old];
    }
    groups.sort_by_key(|g| g.id);

    result.cells = cells.into_iter().map(|(c, _)| c).collect();
    result.groups = groups;
    result
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Build a `DetectionInput` over raw==physical gates (data_scale 0).
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
            azimuth_spacing_deg: None,
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

    /// Size floors relaxed so tests exercise segmentation, not geometry.
    fn permissive() -> DetectionParams {
        DetectionParams {
            min_area_km2: 0.0,
            min_gate_count: 1,
            ..DetectionParams::default()
        }
    }

    #[wasm_bindgen_test]
    fn empty_input_yields_no_cells() {
        let input = mk_input(&[], &[], 0);
        assert!(detect_cells(&input, &permissive()).is_empty());
    }

    #[wasm_bindgen_test]
    fn truncated_gate_buffer_is_rejected() {
        let az = even_az(2);
        let gates = vec![45.0; 4];
        let input = mk_input(&az, &gates, 4);
        assert!(detect_cells(&input, &permissive()).is_empty());
    }

    #[wasm_bindgen_test]
    fn single_block_is_one_cell_with_expected_summary() {
        let (gates, gc) = flatten(vec![
            vec![50.0, 50.0, 0.0, 0.0],
            vec![50.0, 50.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0],
        ]);
        let az = even_az(4);
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive()).cells;
        assert_eq!(cells.len(), 1);
        let c = &cells[0];
        assert_eq!(c.gate_count, 4);
        assert_eq!(c.peak_dbz, 50.0);
        assert!((c.mean_dbz - 50.0).abs() < 1e-3);
        assert!(c.range_from_radar_km > 0.0);
        assert!((0.0..360.0).contains(&c.bearing_from_radar_deg));
        assert!((0.0..180.0).contains(&c.orientation_deg));
        assert!(c.elongation >= 1.0);
        let (min_lat, min_lon, max_lat, max_lon) = c.bounds;
        assert!(min_lat <= max_lat && min_lon <= max_lon);
    }

    #[wasm_bindgen_test]
    fn single_gate_spike_does_not_set_the_peak() {
        // 5×5 of 50 dBZ with one 73 dBZ gate in the middle.
        let mut rows = vec![vec![50.0; 5]; 5];
        rows[2][2] = 73.0;
        let (gates, gc) = flatten(rows);
        let az = vec![0.0_f32, 1.0, 2.0, 3.0, 4.0];
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive()).cells;
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].peak_dbz, 50.0);
    }

    #[wasm_bindgen_test]
    fn echo_without_a_core_is_dropped() {
        // 42 dBZ everywhere: inside the 35 envelope but below 35 + 10.
        let (gates, gc) = flatten(vec![vec![42.0; 4]; 3]);
        let az = vec![0.0_f32, 1.0, 2.0];
        let input = mk_input(&az, &gates, gc);
        assert!(detect_cells(&input, &permissive()).is_empty());
    }

    #[wasm_bindgen_test]
    fn two_disjoint_blocks_are_two_cells_sorted_strongest_first() {
        let (gates, gc) = flatten(vec![
            vec![50.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![50.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 60.0, 60.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 60.0, 60.0, 0.0],
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ]);
        let az = even_az(6);
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive()).cells;
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].peak_dbz, 60.0);
        assert_eq!(cells[1].peak_dbz, 50.0);
    }

    #[wasm_bindgen_test]
    fn line_of_two_storms_splits_at_the_level_between_them() {
        // Two 60 dBZ storms along one radial pair, joined by 40 dBZ echo:
        // one envelope at 35, two cells.
        let row = vec![
            0.0, 45.0, 55.0, 60.0, 60.0, 55.0, 45.0, 40.0, 40.0, 45.0, 55.0, 60.0, 60.0, 55.0,
            45.0, 0.0,
        ];
        let (gates, gc) = flatten(vec![row.clone(), row.clone(), row]);
        let az = vec![0.0_f32, 1.0, 2.0];
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive()).cells;
        assert_eq!(cells.len(), 2, "{cells:?}");
    }

    #[wasm_bindgen_test]
    fn groups_link_their_cells_and_ids_are_strongest_first() {
        // Two storms (60 and 55 dBZ) in one envelope plus a lone 50 dBZ cell.
        let row = vec![
            0.0, 45.0, 55.0, 55.0, 55.0, 45.0, 40.0, 40.0, 45.0, 60.0, 60.0, 60.0, 45.0, 0.0, 0.0,
            0.0, 50.0, 50.0, 50.0, 0.0,
        ];
        let (gates, gc) = flatten(vec![row.clone(), row.clone(), row]);
        let az = vec![0.0_f32, 1.0, 2.0];
        let input = mk_input(&az, &gates, gc);
        let r = detect_cells(&input, &permissive());
        let peaks: Vec<f32> = r.cells.iter().map(|c| c.peak_dbz).collect();
        assert_eq!(peaks, vec![60.0, 55.0, 50.0]);
        for (i, c) in r.cells.iter().enumerate() {
            assert_eq!(c.id as usize, i);
            assert!(c.outline.len() >= 4 && c.outline.first() == c.outline.last());
        }
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.groups[0].cell_ids, vec![0, 1]);
        assert_eq!(r.groups[0].peak_dbz, 60.0);
        assert_eq!(r.cells[0].group_id, Some(0));
        assert_eq!(r.cells[1].group_id, Some(0));
        assert_eq!(r.cells[2].group_id, None);
        // The lone cell keeps its envelope; grouped cells their split level.
        assert_eq!(r.cells[2].level_dbz, 35.0);
        assert!(r.cells[0].level_dbz > 35.0);
        // The two grouped cells tile their envelope (12 gates on each of 3
        // radials); the lone cell has its 9.
        assert_eq!(r.cells[0].gate_count + r.cells[1].gate_count, 36);
        assert_eq!(r.cells[2].gate_count, 9);
        // Every cell carries its core ring, the lone one too (its 50 dBZ
        // block reaches the 45 dBZ core level).
        assert!(r.cells.iter().all(|c| c.core_outline.len() >= 4));
        assert_eq!(r.source.params, permissive());
    }

    #[wasm_bindgen_test]
    fn azimuth_seam_wraps_into_single_cell() {
        let mut rows = vec![vec![0.0; 6]; 8];
        rows[0][2] = 50.0;
        rows[0][3] = 50.0;
        rows[7][2] = 50.0;
        rows[7][3] = 50.0;
        let (gates, gc) = flatten(rows);
        let az = even_az(8);
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive()).cells;
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].gate_count, 4);
    }

    #[wasm_bindgen_test]
    fn large_angular_gap_does_not_wrap() {
        let mut rows = vec![vec![0.0; 6]; 4];
        rows[0][2] = 50.0;
        rows[0][3] = 50.0;
        rows[3][2] = 50.0;
        rows[3][3] = 50.0;
        let (gates, gc) = flatten(rows);
        // The wrap gap 60→0 is 300° ≫ 2× median(20), so no seam.
        let az = vec![0.0_f32, 20.0, 40.0, 60.0];
        let input = mk_input(&az, &gates, gc);
        assert_eq!(detect_cells(&input, &permissive()).cells.len(), 2);
    }

    #[wasm_bindgen_test]
    fn min_gate_count_filter_rejects_small_components() {
        let (gates, gc) = flatten(vec![vec![50.0, 50.0, 0.0], vec![0.0, 0.0, 0.0]]);
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
        let (gates, gc) = flatten(vec![vec![50.0, 50.0, 0.0], vec![50.0, 50.0, 0.0]]);
        let az = even_az(2);
        let input = mk_input(&az, &gates, gc);
        assert_eq!(detect_cells(&input, &permissive()).cells.len(), 1);
        let params = DetectionParams {
            min_area_km2: 1.0e9,
            ..permissive()
        };
        assert!(detect_cells(&input, &params).is_empty());
    }

    #[wasm_bindgen_test]
    fn padded_azimuth_rows_are_ignored() {
        let (gates, gc) = flatten(vec![
            vec![0.0, 0.0, 0.0, 0.0],
            vec![50.0, 50.0, 50.0, 50.0],
            vec![0.0, 0.0, 0.0, 0.0],
        ]);
        let az = vec![0.0_f32, -1.0, 90.0];
        let input = mk_input(&az, &gates, gc);
        assert!(detect_cells(&input, &permissive()).is_empty());
    }

    #[wasm_bindgen_test]
    fn sentinel_and_scaled_codes_decode_before_detection() {
        // Raw codes with scale 2 / offset 66: 166 → 50 dBZ, sentinels 0/1.
        let row = vec![0.0, 166.0, 166.0, 1.0];
        let (gates, gc) = flatten(vec![row.clone(), row.clone(), row]);
        let az = vec![0.0_f32, 1.0, 2.0];
        let input = DetectionInput {
            data_scale: 2.0,
            data_offset: 66.0,
            ..mk_input(&az, &gates, gc)
        };
        let cells = detect_cells(&input, &permissive()).cells;
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].gate_count, 6);
        assert!((cells[0].peak_dbz - 50.0).abs() < 1e-3);
    }

    /// A 4-radial × 4-gate core starting at azimuth 10°, in a sweep of
    /// `n_radials` radials spaced 0.5° apart from 0°.
    fn half_degree_sweep(n_radials: usize) -> (Vec<f32>, Vec<f32>, usize) {
        let gate_count = 20;
        let az: Vec<f32> = (0..n_radials).map(|i| i as f32 * 0.5).collect();
        let mut gates = vec![0.0_f32; n_radials * gate_count];
        for a in 20..24 {
            for g in 10..14 {
                gates[a * gate_count + g] = 45.0;
            }
        }
        (az, gates, gate_count)
    }

    #[wasm_bindgen_test]
    fn quarter_sweep_area_matches_full_sweep() {
        // A live partial holding a quarter of a 0.5° sweep must give each
        // gate the same 0.5° width as the full sweep, not 360/180 = 2°.
        let (full_az, full_gates, gc) = half_degree_sweep(720);
        let (q_az, q_gates, _) = half_degree_sweep(180);
        let area = |input: &DetectionInput| {
            let r = detect_cells(input, &permissive());
            assert_eq!(r.cells.len(), 1);
            r.cells[0].area_km2
        };

        let mut full = mk_input(&full_az, &full_gates, gc);
        full.azimuth_spacing_deg = Some(0.5);
        let full_area = area(&full);

        // Spacing supplied by the renderer.
        let mut quarter = mk_input(&q_az, &q_gates, gc);
        quarter.azimuth_spacing_deg = Some(0.5);
        let q_area = area(&quarter);
        assert!((q_area - full_area).abs() < 1e-4, "{q_area} vs {full_area}");

        // Spacing not supplied: the median gap of the azimuths is used.
        let q_area = area(&mk_input(&q_az, &q_gates, gc));
        assert!((q_area - full_area).abs() < 1e-4, "{q_area} vs {full_area}");
        let f_area = area(&mk_input(&full_az, &full_gates, gc));
        assert!((f_area - full_area).abs() < 1e-4, "{f_area} vs {full_area}");
    }

    #[wasm_bindgen_test]
    fn pca_produces_finite_elongation_and_bounded_orientation() {
        let az = even_az(4);
        let (gates, gc) = flatten(vec![
            vec![0.0; 8],
            vec![50.0; 8],
            vec![50.0; 8],
            vec![0.0; 8],
        ]);
        let input = mk_input(&az, &gates, gc);
        let cells = detect_cells(&input, &permissive()).cells;
        assert_eq!(cells.len(), 1);
        let c = &cells[0];
        assert!(c.elongation.is_finite());
        assert!(c.elongation >= 1.0);
        assert!((0.0..180.0).contains(&c.orientation_deg));
    }
}
