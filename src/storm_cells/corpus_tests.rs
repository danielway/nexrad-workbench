//! Corpus tests (#164): the detector on decimated real sweeps from the
//! storm cell study's four volumes, with reviewed expectations.
//!
//! Fixtures live in `tests/fixtures/storm_cells/` (format and how to
//! regenerate them: README there, generator in `tools/corpus/`). Each is the
//! lowest reflectivity surveillance cut (elevation 1, 0.5°) of a NOAA
//! archive volume, decimated 2× in azimuth (1° radials) and 2× in range
//! (0.5 km gates), cropped to 300 km. Peaks after the median filter run a
//! few dB below the full-resolution labels quoted in the study.
//!
//! Boxes are (min_lat, max_lat, min_lon, max_lon).

use std::io::Read;

use super::{detect_cells, DetectionInput, DetectionParams, StormCell, StormCellsResult};

struct Fixture {
    radar_lat: f64,
    radar_lon: f64,
    first_gate_km: f64,
    gate_km: f64,
    scale: f32,
    offset: f32,
    azimuths: Vec<f32>,
    gates: Vec<f32>,
    gate_count: usize,
}

fn load(bytes: &[u8]) -> Fixture {
    let mut p = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .read_to_end(&mut p)
        .expect("fixture inflates");
    assert_eq!(&p[0..4], b"NXSC");
    assert_eq!(p[4], 1, "fixture version");
    let f64_at = |o: usize| f64::from_le_bytes(p[o..o + 8].try_into().unwrap());
    let f32_at = |o: usize| f32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    let u16_at = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]) as usize;
    // magic 4, version 1, site 4
    let mut o = 9;
    let (radar_lat, radar_lon) = (f64_at(o), f64_at(o + 8));
    o += 16;
    let _elevation = f32_at(o);
    let first_gate_km = f32_at(o + 4) as f64;
    let gate_km = f32_at(o + 8) as f64;
    let (scale, offset) = (f32_at(o + 12), f32_at(o + 16));
    o += 20;
    let (az_count, gate_count) = (u16_at(o), u16_at(o + 2));
    o += 4;
    let azimuths = (0..az_count).map(|i| f32_at(o + 4 * i)).collect();
    o += 4 * az_count;
    let gates = p[o..o + az_count * gate_count]
        .iter()
        .map(|&b| b as f32)
        .collect();
    Fixture {
        radar_lat,
        radar_lon,
        first_gate_km,
        gate_km,
        scale,
        offset,
        azimuths,
        gates,
        gate_count,
    }
}

/// Detect on a fixture; also returns the radar position.
fn detect(bytes: &[u8], threshold_dbz: f32) -> (StormCellsResult, (f64, f64)) {
    let f = load(bytes);
    let input = DetectionInput {
        azimuths: &f.azimuths,
        gate_values: &f.gates,
        azimuth_count: f.azimuths.len(),
        gate_count: f.gate_count,
        first_gate_km: f.first_gate_km,
        gate_interval_km: f.gate_km,
        data_scale: f.scale,
        data_offset: f.offset,
        radar_lat: f.radar_lat,
        radar_lon: f.radar_lon,
        azimuth_spacing_deg: None,
    };
    let r = detect_cells(
        &input,
        &DetectionParams {
            threshold_dbz,
            ..DetectionParams::default()
        },
    );
    (r, (f.radar_lat, f.radar_lon))
}

const GREENFIELD: &[u8] =
    include_bytes!("../../tests/fixtures/storm_cells/KDMX20240521_201727_V06_el1.bin");
const MCS: &[u8] =
    include_bytes!("../../tests/fixtures/storm_cells/KFWS20240528_102649_V06_el1.bin");
const MILTON: &[u8] =
    include_bytes!("../../tests/fixtures/storm_cells/KTBW20241009_195926_V06_el1.bin");
const CLEAR_AIR: &[u8] =
    include_bytes!("../../tests/fixtures/storm_cells/KDMX20240715_115714_V06_el1.bin");

fn inside(c: &StormCell, (lat0, lat1, lon0, lon1): (f64, f64, f64, f64)) -> bool {
    (lat0..=lat1).contains(&c.lat) && (lon0..=lon1).contains(&c.lon)
}

/// Rough ground distance in km (flat, fine at these scales).
fn km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dy = (a.0 - b.0) * 111.0;
    let dx = (a.1 - b.1) * 111.0 * a.0.to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}

/// Longest edge of a closed (lat, lon) ring, in km.
fn longest_edge_km(ring: &[(f64, f64)]) -> f64 {
    ring.windows(2).map(|w| km(w[0], w[1])).fold(0.0, f64::max)
}

/// Share of its group's envelope that a grouped cell's footprint covers.
fn group_share(r: &StormCellsResult, c: &StormCell) -> f32 {
    c.group_id
        .map_or(1.0, |g| c.area_km2 / r.groups[g as usize].area_km2)
}

/// Structural invariants every result must hold.
fn check_invariants(r: &StormCellsResult) {
    for (i, c) in r.cells.iter().enumerate() {
        assert_eq!(c.id as usize, i);
        if i > 0 {
            assert!(r.cells[i - 1].peak_dbz >= c.peak_dbz, "sorted by peak");
        }
        assert!(c.outline.len() >= 5, "cell {i} outline {:?}", c.outline);
        assert!(c.outline.len() <= super::model::MAX_OUTLINE_VERTICES + 1);
        assert_eq!(c.outline.first(), c.outline.last());
        // Simplification never cuts a long chord across the footprint
        // (10 km in the outline's projection; this one differs slightly).
        assert!(longest_edge_km(&c.outline) < 10.5, "cell {i}");
        // A grouped cell carries its core ring; a lone cell draws its
        // footprint and carries none.
        if c.group_id.is_some() {
            assert!(c.core_outline.len() >= 5, "cell {i}");
            assert_eq!(c.core_outline.first(), c.core_outline.last());
            assert!(longest_edge_km(&c.core_outline) < 10.5, "cell {i}");
        } else {
            assert!(c.core_outline.is_empty(), "cell {i}");
        }
        assert!(c.peak_dbz >= c.level_dbz);
        if let Some(g) = c.group_id {
            assert!(r.groups[g as usize].cell_ids.contains(&c.id));
        }
    }
    for (i, g) in r.groups.iter().enumerate() {
        assert_eq!(g.id as usize, i);
        assert!(g.cell_ids.len() >= 2);
        assert!(g.outline.len() >= 5 && g.outline.len() <= super::model::MAX_OUTLINE_VERTICES + 1);
        assert!(longest_edge_km(&g.outline) < 10.5, "group {i}");
        for &c in &g.cell_ids {
            assert_eq!(r.cells[c as usize].group_id, Some(g.id));
        }
        // The cells' footprints tile their envelope.
        let sum: f32 = g
            .cell_ids
            .iter()
            .map(|&c| r.cells[c as usize].area_km2)
            .sum();
        assert!((sum - g.area_km2).abs() <= 1e-3 * g.area_km2, "group {i}");
    }
}

// --- KDMX 2024-05-21 20:17Z, Greenfield supercell line ------------------

/// The line from Adams to Calhoun County, 70-110 km west of KDMX.
const LINE: (f64, f64, f64, f64) = (40.75, 42.45, -95.1, -94.4);

#[wasm_bindgen_test::wasm_bindgen_test]
fn greenfield_line_resolves_into_its_storms_inside_one_group() {
    let (r, _) = detect(GREENFIELD, 35.0);
    check_invariants(&r);
    let line: Vec<&StormCell> = r
        .cells
        .iter()
        .filter(|c| inside(c, LINE) && c.peak_dbz >= 50.0)
        .collect();

    // At one 35 dBZ threshold this line was a single 270 km cell. Now it is
    // its storms: 5-8 cells (with a small extra core between Audubon and
    // Cass), four of them reaching 55 dBZ after the median filter. Each
    // holds its own stretch of the line's envelope, none the whole line:
    // the largest (Cass/Adair) about a fifth of it.
    assert!((5..=8).contains(&line.len()), "{line:#?}");
    assert!(line.iter().filter(|c| c.peak_dbz >= 55.0).count() >= 4);
    assert!(
        line.iter()
            .all(|c| c.area_km2 < 1500.0 && group_share(&r, c) < 0.25),
        "no line-sized cell"
    );

    // One cell per named storm (study, case 2), within 12 km.
    for (name, pos) in [
        ("Calhoun", (42.32, -94.67)),
        ("Carroll", (42.01, -94.82)),
        ("Audubon", (41.64, -94.89)),
        ("Cass/Adair (Greenfield)", (41.28, -94.73)),
        ("Adams", (40.87, -94.71)),
    ] {
        let near = line
            .iter()
            .filter(|c| km((c.lat, c.lon), pos) < 12.0)
            .count();
        assert_eq!(near, 1, "{name}: {near} cells");
    }

    // All of them inside exactly one group.
    let group = line[0].group_id.expect("line cells are grouped");
    assert!(line.iter().all(|c| c.group_id == Some(group)));
}

/// Snapshot of the full Greenfield result, so any change to segmentation is
/// visible in review. Regenerate deliberately when the algorithm changes.
#[wasm_bindgen_test::wasm_bindgen_test]
fn greenfield_snapshot() {
    let (r, _) = detect(GREENFIELD, 35.0);
    let got: Vec<String> = r
        .cells
        .iter()
        .map(|c| {
            format!(
                "{} g{} L{:.0} peak {:.1} area {:.0} at {:.2},{:.2}",
                c.id,
                c.group_id.map_or("-".to_string(), |g| g.to_string()),
                c.level_dbz,
                c.peak_dbz,
                c.area_km2,
                c.lat,
                c.lon
            )
        })
        .chain(r.groups.iter().map(|g| {
            format!(
                "group {} peak {:.1} area {:.0} cells {:?}",
                g.id, g.peak_dbz, g.area_km2, g.cell_ids
            )
        }))
        .collect();
    let expected: Vec<&str> = GREENFIELD_SNAPSHOT
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(got, expected, "\n{}", got.join("\n"));
}

const GREENFIELD_SNAPSHOT: &str = "
    0 g1 L45 peak 58.5 area 845 at 39.35,-94.70
    1 g0 L55 peak 58.5 area 629 at 42.34,-94.65
    2 g0 L50 peak 58.0 area 1297 at 41.27,-94.72
    3 g0 L55 peak 58.0 area 1235 at 41.98,-94.80
    4 g2 L45 peak 58.0 area 123 at 40.50,-94.67
    5 g- L35 peak 57.5 area 376 at 42.42,-91.01
    6 g- L35 peak 56.0 area 491 at 43.60,-94.75
    7 g0 L50 peak 55.5 area 583 at 41.61,-94.90
    8 g- L35 peak 55.5 area 208 at 43.02,-94.26
    9 g0 L50 peak 55.0 area 394 at 43.21,-94.39
    10 g0 L40 peak 54.0 area 351 at 42.96,-94.67
    11 g0 L45 peak 53.5 area 474 at 42.65,-94.70
    12 g0 L45 peak 53.5 area 278 at 43.38,-94.88
    13 g3 L50 peak 53.0 area 739 at 39.88,-94.63
    14 g3 L50 peak 53.0 area 367 at 40.06,-94.61
    15 g0 L40 peak 53.0 area 282 at 43.12,-94.67
    16 g0 L50 peak 53.0 area 242 at 41.55,-94.77
    17 g0 L50 peak 53.0 area 241 at 43.28,-94.58
    18 g0 L45 peak 53.0 area 193 at 40.87,-94.72
    19 g- L35 peak 50.5 area 228 at 43.92,-92.91
    20 g- L35 peak 50.5 area 119 at 42.85,-94.12
    21 g3 L40 peak 50.0 area 302 at 39.66,-94.68
    22 g2 L45 peak 49.5 area 67 at 40.55,-94.63
    23 g- L35 peak 49.5 area 24 at 42.64,-94.40
    24 g1 L45 peak 48.8 area 105 at 39.19,-94.80
    25 g0 L45 peak 48.5 area 126 at 43.33,-94.75
    26 g4 L40 peak 48.0 area 687 at 43.85,-94.31
    27 g- L35 peak 48.0 area 112 at 43.36,-94.11
    28 g- L35 peak 47.0 area 248 at 43.19,-95.18
    29 g- L35 peak 46.5 area 120 at 43.73,-92.96
    30 g- L35 peak 46.0 area 73 at 43.49,-94.28
    31 g4 L40 peak 45.0 area 287 at 44.02,-93.80
    32 g4 L40 peak 45.0 area 193 at 44.16,-93.39
    group 0 peak 58.5 area 6325 cells [1, 2, 3, 7, 9, 10, 11, 12, 15, 16, 17, 18, 25]
    group 1 peak 58.5 area 949 cells [0, 24]
    group 2 peak 58.0 area 190 cells [4, 22]
    group 3 peak 53.0 area 1407 cells [13, 14, 21]
    group 4 peak 48.0 area 1168 cells [26, 31, 32]
";

// --- KFWS 2024-05-28 10:27Z, MCS north of the Metroplex -----------------

/// Trailing stratiform rain (Marshall, Bryan, Grayson): the old detector
/// found about ten 45-47 dBZ "cells" here at 45 dBZ.
const STRATIFORM: (f64, f64, f64, f64) = (33.6, 34.3, -97.4, -96.35);
/// Convective leading edge over Wise, Denton and Collin.
const LEADING_EDGE: (f64, f64, f64, f64) = (33.0, 33.45, -97.7, -96.4);

#[wasm_bindgen_test::wasm_bindgen_test]
fn mcs_stratiform_maxima_are_not_cells() {
    for threshold in [35.0, 45.0] {
        let (r, _) = detect(MCS, threshold);
        check_invariants(&r);
        let weak: Vec<&StormCell> = r
            .cells
            .iter()
            .filter(|c| inside(c, STRATIFORM) && c.peak_dbz < 50.0)
            .collect();
        assert!(weak.is_empty(), "at {threshold}: {weak:#?}");
    }
}

#[wasm_bindgen_test::wasm_bindgen_test]
fn mcs_leading_edge_is_a_group_of_cores() {
    let (r, _) = detect(MCS, 35.0);
    let edge: Vec<&StormCell> = r.cells.iter().filter(|c| inside(c, LEADING_EDGE)).collect();
    // Not one 250 km ellipse over Fort Worth: several cores, one group,
    // none holding most of it. The strongest core's footprint includes the
    // stratiform rain behind it (no cell of its own), so it is big, but
    // still under half the group.
    assert!(
        edge.iter().filter(|c| c.peak_dbz >= 55.0).count() >= 3,
        "{edge:#?}"
    );
    assert!(
        edge.iter()
            .all(|c| c.area_km2 < 4000.0 && group_share(&r, c) < 0.5),
        "{edge:#?}"
    );
    let group = edge[0].group_id.expect("edge cells are grouped");
    assert!(edge.iter().all(|c| c.group_id == Some(group)));
}

// --- KTBW 2024-10-09 19:59Z, Hurricane Milton -----------------------------

#[wasm_bindgen_test::wasm_bindgen_test]
fn milton_shield_is_groups_of_bands_not_one_cell() {
    let (r, radar) = detect(MILTON, 35.0);
    check_invariants(&r);
    // The shield around Tampa Bay is carried by groups with many cells each,
    // and no cell near the radar is shield-sized (the study's 330 km cell):
    // each band holds its share of a 13-15 000 km² envelope, at most about
    // a third of it.
    let near: Vec<&StormCell> = r
        .cells
        .iter()
        .filter(|c| km((c.lat, c.lon), radar) < 120.0)
        .collect();
    assert!(near.len() >= 10, "{}", near.len());
    assert!(near.iter().all(|c| c.area_km2 < 5000.0), "{near:#?}");
    assert!(
        near.iter()
            .filter(|c| c
                .group_id
                .is_some_and(|g| r.groups[g as usize].area_km2 > 5000.0))
            .all(|c| group_share(&r, c) < 0.4),
        "{near:#?}"
    );
    assert!(near.iter().filter(|c| c.group_id.is_some()).count() >= near.len() - 1);
    let big_groups = r
        .groups
        .iter()
        .filter(|g| g.area_km2 > 5000.0 && g.cell_ids.len() >= 5)
        .count();
    assert!(big_groups >= 1, "{:#?}", r.groups);
    // The 73 dBZ label came from one gate; the filtered peak is far lower.
    assert!(r.cells.iter().all(|c| c.peak_dbz <= 70.0));
}

// --- KDMX 2024-07-15 11:57Z, clear-air control -------------------------

#[wasm_bindgen_test::wasm_bindgen_test]
fn clear_air_control_finds_only_minnesota_storms_and_known_clutter() {
    let (r, radar) = detect(CLEAR_AIR, 35.0);
    check_invariants(&r);
    let (near, far): (Vec<&StormCell>, Vec<&StormCell>) = r
        .cells
        .iter()
        .partition(|c| km((c.lat, c.lon), radar) < 150.0);
    // Real convection: the southern edge of the Minnesota complex, at the
    // 300 km crop.
    assert!(!far.is_empty());
    assert!(far.iter().all(|c| c.lat > 43.8), "{far:#?}");
    // Clutter: the Greene County wind-farm cluster (318°, 55 km) survives
    // the median filter; the Adair/Guthrie clusters do not. A CC filter
    // (later PR) should bring this to zero.
    assert_eq!(near.len(), 1, "{near:#?}");
    assert!(km((near[0].lat, near[0].lon), (42.11, -94.16)) < 10.0);
}
