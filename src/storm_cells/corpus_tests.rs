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
        assert!(c.peak_dbz >= c.level_dbz);
        if let Some(g) = c.group_id {
            assert!(r.groups[g as usize].cell_ids.contains(&c.id));
        }
    }
    for (i, g) in r.groups.iter().enumerate() {
        assert_eq!(g.id as usize, i);
        assert!(g.cell_ids.len() >= 2);
        assert!(g.outline.len() >= 5 && g.outline.len() <= super::model::MAX_OUTLINE_VERTICES + 1);
        for &c in &g.cell_ids {
            assert_eq!(r.cells[c as usize].group_id, Some(g.id));
        }
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
    // Cass), four of them reaching 55 dBZ after the median filter.
    assert!((5..=8).contains(&line.len()), "{line:#?}");
    assert!(line.iter().filter(|c| c.peak_dbz >= 55.0).count() >= 4);
    assert!(
        line.iter().all(|c| c.area_km2 < 600.0),
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
    0 g1 L45 peak 58.5 area 439 at 39.35,-94.71
    1 g0 L55 peak 58.5 area 17 at 42.32,-94.67
    2 g0 L50 peak 58.0 area 464 at 41.27,-94.73
    3 g2 L45 peak 58.0 area 48 at 40.50,-94.67
    4 g0 L55 peak 58.0 area 23 at 42.01,-94.82
    5 g- L35 peak 57.5 area 376 at 42.42,-91.01
    6 g- L35 peak 56.0 area 491 at 43.60,-94.75
    7 g- L35 peak 55.5 area 208 at 43.02,-94.26
    8 g0 L50 peak 55.5 area 121 at 41.64,-94.89
    9 g0 L50 peak 55.0 area 15 at 43.24,-94.31
    10 g0 L40 peak 54.0 area 217 at 42.96,-94.66
    11 g0 L45 peak 53.5 area 174 at 42.67,-94.70
    12 g0 L45 peak 53.5 area 80 at 43.38,-94.88
    13 g0 L40 peak 53.0 area 135 at 43.11,-94.66
    14 g0 L45 peak 53.0 area 76 at 40.87,-94.71
    15 g3 L50 peak 53.0 area 41 at 39.97,-94.66
    16 g3 L50 peak 53.0 area 20 at 40.02,-94.62
    17 g0 L50 peak 53.0 area 18 at 43.28,-94.51
    18 g0 L50 peak 53.0 area 11 at 41.51,-94.82
    19 g- L35 peak 50.5 area 228 at 43.92,-92.91
    20 g- L35 peak 50.5 area 119 at 42.85,-94.12
    21 g3 L40 peak 50.0 area 164 at 39.67,-94.69
    22 g- L35 peak 49.5 area 24 at 42.64,-94.40
    23 g2 L45 peak 49.5 area 15 at 40.55,-94.63
    24 g1 L45 peak 48.8 area 18 at 39.19,-94.82
    25 g0 L45 peak 48.5 area 24 at 43.34,-94.74
    26 g4 L40 peak 48.0 area 273 at 43.85,-94.29
    27 g- L35 peak 48.0 area 112 at 43.36,-94.11
    28 g- L35 peak 47.0 area 248 at 43.19,-95.18
    29 g- L35 peak 46.5 area 120 at 43.73,-92.96
    30 g- L35 peak 46.0 area 73 at 43.49,-94.28
    31 g4 L40 peak 45.0 area 105 at 44.02,-93.76
    32 g4 L40 peak 45.0 area 72 at 44.17,-93.35
    group 0 peak 58.5 area 6325 cells [1, 2, 4, 8, 9, 10, 11, 12, 13, 14, 17, 18, 25]
    group 1 peak 58.5 area 949 cells [0, 24]
    group 2 peak 58.0 area 190 cells [3, 23]
    group 3 peak 53.0 area 1407 cells [15, 16, 21]
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
    // Not one 250 km ellipse over Fort Worth: several cores, one group.
    assert!(
        edge.iter().filter(|c| c.peak_dbz >= 55.0).count() >= 3,
        "{edge:#?}"
    );
    assert!(edge.iter().all(|c| c.area_km2 < 500.0), "{edge:#?}");
    let group = edge[0].group_id.expect("edge cells are grouped");
    assert!(edge.iter().all(|c| c.group_id == Some(group)));
}

// --- KTBW 2024-10-09 19:59Z, Hurricane Milton -----------------------------

#[wasm_bindgen_test::wasm_bindgen_test]
fn milton_shield_is_groups_of_bands_not_one_cell() {
    let (r, radar) = detect(MILTON, 35.0);
    check_invariants(&r);
    // The shield around Tampa Bay is carried by groups with many cells each,
    // and no cell near the radar is shield-sized (the study's 330 km cell).
    let near: Vec<&StormCell> = r
        .cells
        .iter()
        .filter(|c| km((c.lat, c.lon), radar) < 120.0)
        .collect();
    assert!(near.len() >= 10, "{}", near.len());
    assert!(near.iter().all(|c| c.area_km2 < 1500.0), "{near:#?}");
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

// --- Tracking: KDMX 20:17Z → 20:24Z ---------------------------------------

const GREENFIELD_NEXT: &[u8] =
    include_bytes!("../../tests/fixtures/storm_cells/KDMX20240521_202422_V06_el1.bin");

/// Volume start times of the two fixtures.
const T_2017: i64 = 1_716_322_647_000;
const T_2024: i64 = 1_716_323_062_000;

/// The Greenfield line seven minutes later: the strong storms keep their
/// tracks and move north-east, as they did that afternoon, and no match
/// jumps farther than the search radius.
#[wasm_bindgen_test::wasm_bindgen_test]
fn greenfield_storms_keep_their_tracks_into_the_next_volume() {
    use super::track::{bearing_distance, distance_km, TrackParams, Tracker};
    let (a, _) = detect(GREENFIELD, 35.0);
    let (b, _) = detect(GREENFIELD_NEXT, 35.0);
    check_invariants(&b);
    let run = || {
        let mut tr = Tracker::default();
        let f0 = tr.observe(T_2017, &a, false).unwrap();
        let f1 = tr.observe(T_2024, &b, false).unwrap();
        (f0, f1)
    };
    let (f0, f1) = run();
    assert_eq!((f0.clone(), f1.clone()), run(), "deterministic");
    assert!(f0.cell_tracks.iter().all(Option::is_some));
    assert!(f1.cell_tracks.iter().all(Option::is_some));

    let params = TrackParams::default();
    let radius = params.base_radius_km + params.max_speed_kmh * (T_2024 - T_2017) as f64 / 3.6e6;
    for cell in &b.cells {
        let track = f1.track_of_cell(cell.id).unwrap();
        if track.age_volumes == 2 {
            let (lat, lon) = track.trail[0];
            assert!(distance_km((lat, lon), (cell.lat, cell.lon)) <= radius);
        }
    }

    for (name, pos) in [
        ("Calhoun", (42.32, -94.67)),
        ("Carroll", (42.01, -94.82)),
        ("Cass/Adair (Greenfield)", (41.28, -94.73)),
        ("Adams", (40.87, -94.71)),
    ] {
        let before = a
            .cells
            .iter()
            .find(|c| km((c.lat, c.lon), pos) < 12.0 && c.peak_dbz >= 50.0)
            .unwrap_or_else(|| panic!("{name} at 20:17"));
        let id = f0.cell_tracks[before.id as usize].unwrap();
        let track = f1.track(id).unwrap_or_else(|| panic!("{name} track"));
        let now = track
            .cell_id
            .unwrap_or_else(|| panic!("{name} matched at 20:24"));
        let now = &b.cells[now as usize];
        let (bearing, dist) = bearing_distance((before.lat, before.lon), (now.lat, now.lon));
        assert!(dist <= radius, "{name}: {dist} km");
        assert!(
            (0.0..=120.0).contains(&bearing),
            "{name}: moved toward {bearing}"
        );
        assert!(now.peak_dbz >= 50.0, "{name}: {now:?}");
        assert_eq!(track.age_volumes, 2);
    }
}
