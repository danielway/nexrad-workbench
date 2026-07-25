//! Per-record decode utilities and sweep extraction.
//!
//! Provides functions to decompress and decode individual LDM records
//! into radials, and to extract pre-computed sweep data from radials.

use crate::data::keys::{GateValues, PrecomputedSweep};
use ::nexrad::model::data::{DataMoment, Radial};
use nexrad_data::volume::Record;
use nexrad_render::Product;

/// Decode a single LDM record into radials.
///
/// Accepts either compressed records (4-byte size prefix + bzip2 data, as
/// produced by `nexrad_data::volume::File::records()`) or decompressed
/// records (raw message bytes, as stored in IndexedDB). Compressed records
/// are decompressed automatically.
///
/// Returns the decoded radials (may be empty if the record contains only
/// non-radial messages like VCP metadata).
pub fn decode_record_to_radials(record_bytes: &[u8]) -> Result<Vec<Radial>, String> {
    let record = Record::from_slice(record_bytes);

    if !record.compressed() {
        return record
            .radials()
            .map_err(|e| format!("Failed to decode uncompressed record: {}", e));
    }

    let decompressed = record
        .decompress()
        .map_err(|e| format!("Failed to decompress record: {}", e))?;

    decompressed
        .radials()
        .map_err(|e| format!("Failed to decode record radials: {}", e))
}

/// Extract the volume start time from decoded radials.
///
/// Looks for a radial whose status is `ScanStart` (the first radial of a new
/// volume scan) and returns its collection timestamp in Unix seconds. Returns
/// `None` if no such radial is present in this set.
pub fn extract_volume_start_time(radials: &[Radial]) -> Option<f64> {
    use ::nexrad::model::data::RadialStatus;
    radials
        .iter()
        .find(|r| matches!(r.radial_status(), RadialStatus::ScanStart))
        .map(|r| r.collection_timestamp() as f64 / 1000.0)
}

/// Extract a pre-computed sweep from radials already filtered to one elevation
/// and sorted by azimuth. Only filters by product availability.
///
/// This avoids redundant full-array scans and per-product sorting when
/// extracting multiple products from the same elevation group.
pub fn extract_sweep_data_from_sorted(
    sorted_radials: &[&Radial],
    product: Product,
) -> Option<PrecomputedSweep> {
    let target: Vec<&Radial> = sorted_radials
        .iter()
        .filter(|r| product.moment_data(r).is_some() || product.cfp_moment_data(r).is_some())
        .copied()
        .collect();

    if target.is_empty() {
        return None;
    }

    build_precomputed_sweep(&target, product)
}

/// Extract gate params from a radial's moment data for a given product.
/// Returns (first_gate_range_km, gate_interval_km, gate_count, scale, offset, data_word_size).
fn moment_params(product: Product, radial: &Radial) -> Option<(f64, f64, usize, f32, f32, u8)> {
    if let Some(m) = product.moment_data(radial) {
        Some((
            m.first_gate_range_km(),
            m.gate_interval_km(),
            m.gate_count() as usize,
            m.scale(),
            m.offset(),
            m.data_word_size(),
        ))
    } else {
        product.cfp_moment_data(radial).map(|m| {
            (
                m.first_gate_range_km(),
                m.gate_interval_km(),
                m.gate_count() as usize,
                m.scale(),
                m.offset(),
                m.data_word_size(),
            )
        })
    }
}

/// Get raw byte slice from a radial's moment data for a given product.
fn moment_raw_values(product: Product, radial: &Radial) -> Option<&[u8]> {
    if let Some(m) = product.moment_data(radial) {
        Some(m.raw_values())
    } else if let Some(m) = product.cfp_moment_data(radial) {
        Some(m.raw_values())
    } else {
        None
    }
}

/// Build a PrecomputedSweep from a filtered, sorted list of radials.
fn build_precomputed_sweep(target: &[&Radial], product: Product) -> Option<PrecomputedSweep> {
    let (first_gate_range_km, gate_interval_km, gate_count, scale, offset, data_word_size) =
        moment_params(product, target[0])?;

    let azimuth_count = target.len();
    let total = azimuth_count * gate_count;
    let mut azimuths = Vec::with_capacity(azimuth_count);
    let mut radial_times = Vec::with_capacity(azimuth_count);
    let mut min_ts = f64::INFINITY;
    let mut max_ts = f64::NEG_INFINITY;
    let mut elev_sum: f64 = 0.0;

    // Collect shared radial metadata in a single pass
    for radial in target.iter() {
        azimuths.push(radial.azimuth_angle_degrees());
        let ts = radial.collection_timestamp() as f64;
        radial_times.push(ts / 1000.0);
        if ts < min_ts {
            min_ts = ts;
        }
        if ts > max_ts {
            max_ts = ts;
        }
        elev_sum += radial.elevation_angle_degrees() as f64;
    }

    // Extract gate values — branch only for the word-size-specific fill
    let gate_values = if data_word_size == 16 {
        let mut vals: Vec<u16> = vec![0; total]; // 0 = below threshold sentinel
        for (row, radial) in target.iter().enumerate() {
            if let Some(bytes) = moment_raw_values(product, radial) {
                let dest = &mut vals[row * gate_count..(row + 1) * gate_count];
                let n = (bytes.len() / 2).min(gate_count);
                for i in 0..n {
                    dest[i] = u16::from_be_bytes([bytes[i * 2], bytes[i * 2 + 1]]);
                }
            }
        }
        GateValues::U16(vals)
    } else {
        let mut vals: Vec<u8> = vec![0; total]; // 0 = below threshold sentinel
        for (row, radial) in target.iter().enumerate() {
            if let Some(bytes) = moment_raw_values(product, radial) {
                let dest = &mut vals[row * gate_count..(row + 1) * gate_count];
                let n = bytes.len().min(gate_count);
                dest[..n].copy_from_slice(&bytes[..n]);
            }
        }
        GateValues::U8(vals)
    };

    let max_range_km = first_gate_range_km + (gate_count as f64) * gate_interval_km;
    let mean_elevation = (elev_sum / azimuth_count as f64) as f32;

    Some(PrecomputedSweep {
        azimuth_count: azimuth_count as u32,
        gate_count: gate_count as u32,
        first_gate_range_km,
        gate_interval_km,
        max_range_km,
        scale,
        offset,
        radial_count: azimuth_count as u32,
        mean_elevation,
        sweep_start_secs: min_ts / 1000.0,
        sweep_end_secs: max_ts / 1000.0,
        azimuths,
        radial_times,
        gate_values,
    })
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    use ::nexrad::model::data::{CFPMomentData, MomentData, RadialStatus};

    /// Build an 8-bit reflectivity radial with the given metadata and raw gate
    /// bytes. `first_gate_range`/`gate_interval` are in meters (the moment
    /// stores them as integer millis-of-km, i.e. `* 0.001` => km).
    fn refl_radial(ts_ms: i64, azimuth: f32, elevation: f32, raw: Vec<u8>) -> Radial {
        let gate_count = raw.len() as u16;
        let m = MomentData::from_fixed_point(
            gate_count, // gate_count
            2000,       // first_gate_range -> 2.0 km
            250,        // gate_interval -> 0.25 km
            8,          // data_word_size (8-bit)
            2.0,        // scale
            66.0,       // offset
            raw,
        );
        Radial::new(
            ts_ms,
            0,       // azimuth_number
            azimuth, // azimuth_angle_degrees
            0.5,     // azimuth_spacing_degrees
            RadialStatus::IntermediateRadialData,
            1,         // elevation_number
            elevation, // elevation_angle_degrees
            Some(m),   // reflectivity
            None,      // velocity
            None,      // spectrum_width
            None,      // differential_reflectivity
            None,      // differential_phase
            None,      // correlation_coefficient
            None,      // clutter_filter_power
        )
    }

    /// Build a bare radial with the given status and no moment data.
    fn status_radial(ts_ms: i64, status: RadialStatus) -> Radial {
        Radial::new(
            ts_ms, 0, 0.0, 0.5, status, 1, 0.5, None, None, None, None, None, None, None,
        )
    }

    // ── extract_volume_start_time ───────────────────────────────────────────

    #[wasm_bindgen_test]
    fn volume_start_time_empty_is_none() {
        let radials: Vec<Radial> = Vec::new();
        assert_eq!(extract_volume_start_time(&radials), None);
    }

    #[wasm_bindgen_test]
    fn volume_start_time_no_scan_start_is_none() {
        let radials = vec![
            status_radial(1_700_000_000_500, RadialStatus::IntermediateRadialData),
            status_radial(1_700_000_001_000, RadialStatus::ElevationStart),
            status_radial(1_700_000_002_000, RadialStatus::ScanEnd),
        ];
        assert_eq!(extract_volume_start_time(&radials), None);
    }

    #[wasm_bindgen_test]
    fn volume_start_time_finds_scan_start_in_seconds() {
        // collection_timestamp is in ms; the function divides by 1000 -> secs.
        let radials = vec![
            status_radial(1_700_000_000_000, RadialStatus::IntermediateRadialData),
            status_radial(1_700_000_123_456, RadialStatus::ScanStart),
            status_radial(1_700_000_999_000, RadialStatus::ScanStart),
        ];
        let got = extract_volume_start_time(&radials).expect("scan-start present");
        // First matching radial wins (find), so 1_700_000_123_456 ms -> secs.
        assert!((got - 1_700_000_123.456).abs() < 1e-3, "got {got}");
    }

    // ── extract_sweep_data_from_sorted (build_precomputed_sweep) ────────────

    #[wasm_bindgen_test]
    fn sweep_no_matching_product_is_none() {
        // Radials carry only reflectivity; query for velocity -> nothing.
        let r0 = refl_radial(1_700_000_000_000, 10.0, 0.5, vec![10, 20, 30]);
        let r1 = refl_radial(1_700_000_002_000, 20.0, 0.7, vec![40, 50, 60]);
        let sorted: Vec<&Radial> = vec![&r0, &r1];
        assert!(extract_sweep_data_from_sorted(&sorted, Product::Velocity).is_none());
    }

    #[wasm_bindgen_test]
    fn sweep_empty_input_is_none() {
        let sorted: Vec<&Radial> = Vec::new();
        assert!(extract_sweep_data_from_sorted(&sorted, Product::Reflectivity).is_none());
    }

    #[wasm_bindgen_test]
    fn sweep_u8_reflectivity_builds_expected() {
        let r0 = refl_radial(1_700_000_000_000, 10.0, 0.5, vec![10, 20, 30]);
        let r1 = refl_radial(1_700_000_002_000, 20.0, 0.7, vec![40, 50, 60]);
        let sorted: Vec<&Radial> = vec![&r0, &r1];

        let sweep = extract_sweep_data_from_sorted(&sorted, Product::Reflectivity)
            .expect("reflectivity present");

        assert_eq!(sweep.azimuth_count, 2);
        assert_eq!(sweep.radial_count, 2);
        assert_eq!(sweep.gate_count, 3);

        // Header params come from the first radial's moment.
        assert!((sweep.first_gate_range_km - 2.0).abs() < 1e-9);
        assert!((sweep.gate_interval_km - 0.25).abs() < 1e-9);
        // max_range = first_gate_range + gate_count * gate_interval = 2 + 3*0.25 = 2.75
        assert!((sweep.max_range_km - 2.75).abs() < 1e-9);
        assert!((sweep.scale - 2.0).abs() < 1e-6);
        assert!((sweep.offset - 66.0).abs() < 1e-6);

        // mean_elevation = (0.5 + 0.7) / 2 = 0.6
        assert!(
            (sweep.mean_elevation - 0.6).abs() < 1e-5,
            "got {}",
            sweep.mean_elevation
        );

        // Azimuths preserved in input order.
        assert_eq!(sweep.azimuths.len(), 2);
        assert!((sweep.azimuths[0] - 10.0).abs() < 1e-6);
        assert!((sweep.azimuths[1] - 20.0).abs() < 1e-6);

        // sweep_start/end: timestamps (ms) / 1000 -> secs.
        assert!((sweep.sweep_start_secs - 1_700_000_000.0).abs() < 1e-3);
        assert!((sweep.sweep_end_secs - 1_700_000_002.0).abs() < 1e-3);

        // radial_times parallel to azimuths, also in secs.
        assert_eq!(sweep.radial_times.len(), 2);
        assert!((sweep.radial_times[0] - 1_700_000_000.0).abs() < 1e-3);
        assert!((sweep.radial_times[1] - 1_700_000_002.0).abs() < 1e-3);

        // Row-major gate values, native u8 word size.
        assert_eq!(sweep.gate_values.word_size(), 1);
        match &sweep.gate_values {
            GateValues::U8(vals) => {
                assert_eq!(vals, &vec![10u8, 20, 30, 40, 50, 60]);
            }
            GateValues::U16(_) => panic!("expected u8 gate values"),
        }
    }

    #[wasm_bindgen_test]
    fn sweep_u16_reflectivity_decodes_big_endian() {
        // 16-bit moment: raw_values are big-endian u16 pairs.
        // gate_count = 2, values = [0x01,0x02, 0x03,0x04] -> [258, 772].
        let m0 =
            MomentData::from_fixed_point(2, 1000, 500, 16, 1.0, 0.0, vec![0x01, 0x02, 0x03, 0x04]);
        let m1 =
            MomentData::from_fixed_point(2, 1000, 500, 16, 1.0, 0.0, vec![0x10, 0x20, 0x30, 0x40]);
        let r0 = Radial::new(
            1_000,
            0,
            0.0,
            0.5,
            RadialStatus::IntermediateRadialData,
            1,
            1.0,
            Some(m0),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let r1 = Radial::new(
            3_000,
            0,
            1.0,
            0.5,
            RadialStatus::IntermediateRadialData,
            1,
            1.0,
            Some(m1),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let sorted: Vec<&Radial> = vec![&r0, &r1];

        let sweep = extract_sweep_data_from_sorted(&sorted, Product::Reflectivity)
            .expect("reflectivity present");

        assert_eq!(sweep.azimuth_count, 2);
        assert_eq!(sweep.gate_count, 2);
        assert_eq!(sweep.gate_values.word_size(), 2);
        match &sweep.gate_values {
            GateValues::U16(vals) => {
                // row 0: 0x0102=258, 0x0304=772 ; row 1: 0x1020=4128, 0x3040=12352
                assert_eq!(vals, &vec![258u16, 772, 4128, 12352]);
            }
            GateValues::U8(_) => panic!("expected u16 gate values"),
        }
    }

    #[wasm_bindgen_test]
    fn sweep_cfp_product_uses_cfp_branch() {
        // ClutterFilterPower: moment_data() is always None, so the CFP branch
        // in moment_params/moment_raw_values must supply the data.
        let cfp = CFPMomentData::from_fixed_point(3, 2000, 250, 8, 1.0, 0.0, vec![8, 9, 10]);
        let r0 = Radial::new(
            5_000,
            0,
            30.0,
            0.5,
            RadialStatus::IntermediateRadialData,
            1,
            0.5,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(cfp),
        );
        let sorted: Vec<&Radial> = vec![&r0];

        let sweep = extract_sweep_data_from_sorted(&sorted, Product::ClutterFilterPower)
            .expect("CFP present");
        assert_eq!(sweep.azimuth_count, 1);
        assert_eq!(sweep.gate_count, 3);
        assert_eq!(sweep.gate_values.word_size(), 1);
        match &sweep.gate_values {
            GateValues::U8(vals) => assert_eq!(vals, &vec![8u8, 9, 10]),
            GateValues::U16(_) => panic!("expected u8 gate values"),
        }
        // A non-CFP product against a CFP-only radial finds nothing.
        assert!(extract_sweep_data_from_sorted(&sorted, Product::Reflectivity).is_none());
    }

    #[wasm_bindgen_test]
    fn sweep_short_raw_values_zero_filled() {
        // raw bytes shorter than gate_count*word: tail stays at the 0 sentinel.
        // gate_count declared 4 but only 2 bytes provided.
        let m = MomentData::from_fixed_point(4, 0, 1000, 8, 1.0, 0.0, vec![7, 8]);
        let r0 = Radial::new(
            9_000,
            0,
            0.0,
            0.5,
            RadialStatus::IntermediateRadialData,
            1,
            2.0,
            Some(m),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let sorted: Vec<&Radial> = vec![&r0];
        let sweep = extract_sweep_data_from_sorted(&sorted, Product::Reflectivity)
            .expect("reflectivity present");
        assert_eq!(sweep.gate_count, 4);
        match &sweep.gate_values {
            GateValues::U8(vals) => assert_eq!(vals, &vec![7u8, 8, 0, 0]),
            GateValues::U16(_) => panic!("expected u8 gate values"),
        }
    }
}
