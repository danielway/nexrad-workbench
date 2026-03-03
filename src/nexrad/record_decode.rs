//! Per-record decode utilities.
//!
//! Provides functions to decompress and decode individual LDM records
//! into radials, without constructing full Scan/Volume objects.
//! Used by the worker thread for selective elevation-based decoding.

use ::nexrad::model::data::Radial;
use nexrad_data::volume::Record;

/// Sub-timings for the decode pipeline.
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
