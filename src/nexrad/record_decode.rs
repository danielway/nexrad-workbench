//! Per-record decode utilities and sweep extraction.
//!
//! Provides functions to decompress and decode individual LDM records
//! into radials, and to extract pre-computed sweep data from radials.

use crate::data::keys::{GateValues, PrecomputedSweep};
use ::nexrad::model::data::Radial;
use nexrad_data::volume::Record;
use nexrad_model::data::DataMoment;
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

/// Extract sorted, deduplicated elevation numbers from already-decoded radials.
///
/// Use this when radials have already been decoded (e.g. after decompressing a
/// record during ingest) to avoid redundant decompression.
pub fn extract_elevation_numbers(radials: &[Radial]) -> Vec<u8> {
    let mut elevations: Vec<u8> = radials.iter().map(|r| r.elevation_number()).collect();
    elevations.sort_unstable();
    elevations.dedup();
    elevations
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

    let gate_values = if data_word_size == 16 {
        let mut vals: Vec<u16> = vec![0; total]; // 0 = below threshold sentinel
        for (row, radial) in target.iter().enumerate() {
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
