//! Per-record decode utilities.
//!
//! Provides functions to decompress and decode individual LDM records
//! into radials, without constructing full Scan/Volume objects.
//! Used by the worker thread for selective elevation-based decoding.

use ::nexrad::model::data::Radial;
use nexrad_data::volume::Record;

/// Decompress and decode a single LDM record into radials.
///
/// The input bytes must be a complete LDM record (4-byte big-endian size
/// prefix followed by bzip2-compressed data). Records in this format are
/// produced by `nexrad_data::volume::File::records()`.
///
/// Returns the decoded radials (may be empty if the record contains only
/// non-radial messages like VCP metadata).
pub fn decode_record_to_radials(record_bytes: &[u8]) -> Result<Vec<Radial>, String> {
    let record = Record::from_slice(record_bytes);

    if !record.compressed() {
        // Uncompressed record (e.g. legacy CTM) — decode directly
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

/// Probe a compressed LDM record for elevation numbers without retaining radial data.
///
/// Decompresses the record and peeks at each radial's elevation_number field.
/// Returns a sorted, deduplicated list of elevation numbers found in the record.
///
/// This is used at ingest time to populate `RecordIndexEntry.elevation_numbers`,
/// enabling selective record fetching by elevation.
pub fn probe_record_elevations(record_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let radials = decode_record_to_radials(record_bytes)?;

    let mut elevations: Vec<u8> = radials
        .iter()
        .map(|r: &Radial| r.elevation_number())
        .collect();

    elevations.sort_unstable();
    elevations.dedup();

    Ok(elevations)
}
