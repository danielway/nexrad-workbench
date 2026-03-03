//! Per-record decode utilities and sweep extraction.
//!
//! Provides functions to decompress and decode individual LDM records
//! into radials, and to extract pre-computed sweep data from radials.

use crate::data::keys::PrecomputedSweep;
use ::nexrad::model::data::Radial;
use nexrad_model::data::DataMoment;
use nexrad_data::volume::Record;
use nexrad_render::Product;

/// Sub-timings for the decode pipeline.
#[allow(dead_code)]
pub struct DecodeTimings {
    /// Time spent on bzip2 decompression (ms).
    pub decompress_ms: f64,
    /// Time spent decoding radials from decompressed messages (ms).
    pub decode_ms: f64,
}

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
    let (radials, _) = decode_record_to_radials_timed(record_bytes)?;
    Ok(radials)
}

/// Like [`decode_record_to_radials`] but also returns sub-timings for
/// decompression and radial decoding.
pub fn decode_record_to_radials_timed(
    record_bytes: &[u8],
) -> Result<(Vec<Radial>, DecodeTimings), String> {
    let record = Record::from_slice(record_bytes);

    if !record.compressed() {
        // Uncompressed record (e.g. legacy CTM) — decode directly
        let t_decode = web_time::Instant::now();
        let radials = record
            .radials()
            .map_err(|e| format!("Failed to decode uncompressed record: {}", e))?;
        let decode_ms = t_decode.elapsed().as_secs_f64() * 1000.0;
        return Ok((
            radials,
            DecodeTimings {
                decompress_ms: 0.0,
                decode_ms,
            },
        ));
    }

    let t_decompress = web_time::Instant::now();
    let decompressed = record
        .decompress()
        .map_err(|e| format!("Failed to decompress record: {}", e))?;
    let decompress_ms = t_decompress.elapsed().as_secs_f64() * 1000.0;

    let t_decode = web_time::Instant::now();
    let radials = decompressed
        .radials()
        .map_err(|e| format!("Failed to decode record radials: {}", e))?;
    let decode_ms = t_decode.elapsed().as_secs_f64() * 1000.0;

    Ok((radials, DecodeTimings { decompress_ms, decode_ms }))
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

/// Extract a pre-computed sweep from decoded radials for a given elevation and product.
///
/// Filters radials to the target elevation/product, sorts by azimuth, and
/// bulk-converts raw gate values to f32. Returns `None` if no matching radials.
pub fn extract_sweep_data(
    radials: &[Radial],
    elevation_number: u8,
    product: Product,
) -> Option<PrecomputedSweep> {
    // Filter to matching elevation + product
    let mut target: Vec<&Radial> = radials
        .iter()
        .filter(|r| {
            r.elevation_number() == elevation_number
                && (product.moment_data(r).is_some() || product.cfp_moment_data(r).is_some())
        })
        .collect();

    if target.is_empty() {
        return None;
    }

    // Sort by azimuth for GPU rendering efficiency
    target.sort_by(|a, b| {
        a.azimuth_angle_degrees()
            .partial_cmp(&b.azimuth_angle_degrees())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let radial_count = target.len();

    // Extract gate params + scale/offset from first radial's moment
    let (first_gate_range_km, gate_interval_km, gate_count, scale, offset) = {
        let r = target[0];
        if let Some(m) = product.moment_data(r) {
            (
                m.first_gate_range_km(),
                m.gate_interval_km(),
                m.gate_count() as usize,
                m.scale(),
                m.offset(),
            )
        } else if let Some(m) = product.cfp_moment_data(r) {
            (
                m.first_gate_range_km(),
                m.gate_interval_km(),
                m.gate_count() as usize,
                m.scale(),
                m.offset(),
            )
        } else {
            return None;
        }
    };

    let azimuth_count = target.len();
    let total = azimuth_count * gate_count;
    let mut azimuths = Vec::with_capacity(azimuth_count);
    let mut timestamps = Vec::with_capacity(azimuth_count);
    let mut elevation_angles = Vec::with_capacity(azimuth_count);
    let mut gate_values: Vec<f32> = vec![0.0; total]; // 0.0 = below threshold sentinel

    for (row, radial) in target.iter().enumerate() {
        azimuths.push(radial.azimuth_angle_degrees());
        timestamps.push(radial.collection_timestamp() as f64);
        elevation_angles.push(radial.elevation_angle_degrees());

        let row_offset = row * gate_count;

        // Get raw byte slice and word size, then bulk-convert
        let (bytes, word_size) = if let Some(m) = product.moment_data(radial) {
            (m.raw_values(), m.data_word_size())
        } else if let Some(m) = product.cfp_moment_data(radial) {
            (m.raw_values(), m.data_word_size())
        } else {
            continue;
        };

        let dest = &mut gate_values[row_offset..row_offset + gate_count];
        if word_size == 16 {
            let n = (bytes.len() / 2).min(gate_count);
            for i in 0..n {
                let raw = u16::from_be_bytes([bytes[i * 2], bytes[i * 2 + 1]]);
                dest[i] = raw as f32;
            }
        } else {
            let n = bytes.len().min(gate_count);
            for i in 0..n {
                dest[i] = bytes[i] as f32;
            }
        }
    }

    let max_range_km = first_gate_range_km + (gate_count as f64) * gate_interval_km;

    Some(PrecomputedSweep {
        azimuth_count: azimuth_count as u32,
        gate_count: gate_count as u32,
        first_gate_range_km,
        gate_interval_km,
        max_range_km,
        scale,
        offset,
        radial_count: radial_count as u32,
        azimuths,
        timestamps,
        elevation_angles,
        gate_values,
    })
}
