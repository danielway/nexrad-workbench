//! Sweep-blob wire format.
//!
//! Defines the binary layout of pre-computed sweep blobs stored in the
//! `sweeps` IDB store: the 72-byte little-endian header, the encode side
//! ([`PrecomputedSweep::to_bytes`]) and the zero-copy decode side
//! ([`parse_sweep_header`]).

/// Gate values stored in their native NEXRAD word size.
pub enum GateValues {
    /// 8-bit raw gate values (most base moments: REF, VEL, SW).
    U8(Vec<u8>),
    /// 16-bit raw gate values (dual-pol on newer radars, CFP).
    U16(Vec<u16>),
}

impl GateValues {
    /// Bytes per gate value (1 or 2).
    pub fn word_size(&self) -> u8 {
        match self {
            GateValues::U8(_) => 1,
            GateValues::U16(_) => 2,
        }
    }
}

/// Pre-computed sweep data ready for GPU rendering.
///
/// Binary layout (little-endian, 72-byte header):
/// - Header (72 bytes): azimuth_count, gate_count, first_gate_range_km,
///   gate_interval_km, max_range_km, scale, offset, radial_count,
///   data_word_size, mean_elevation, sweep_start_secs, sweep_end_secs
/// - Azimuths: f32 × azimuth_count (sorted)
/// - Gate data: u8 or u16 × azimuth_count × gate_count (row-major)
pub struct PrecomputedSweep {
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub scale: f32,
    pub offset: f32,
    pub radial_count: u32,
    pub mean_elevation: f32,
    /// ACTUAL category: earliest radial collection time (Unix seconds).
    /// Parsed directly from radial headers, so this is the authoritative
    /// start-of-sweep time used throughout the timeline and canvas.
    pub sweep_start_secs: f64,
    /// ACTUAL category: latest radial collection time (Unix seconds).
    pub sweep_end_secs: f64,
    pub azimuths: Vec<f32>,
    /// ACTUAL category: per-radial collection timestamps in Unix seconds,
    /// parallel to `azimuths`.
    pub radial_times: Vec<f64>,
    pub gate_values: GateValues,
}

/// Header size: 72 bytes.
///
/// Layout:
///   0..4    azimuth_count (u32)
///   4..8    gate_count (u32)
///   8..16   first_gate_range_km (f64)
///  16..24   gate_interval_km (f64)
///  24..32   max_range_km (f64)
///  32..36   scale (f32)
///  36..40   offset (f32)
///  40..44   radial_count (u32)
///  44..45   data_word_size (u8: 1 or 2)
///  45..46   format_version (u8: 0 = legacy, 1 = has radial_times)
///  46..48   reserved (2 bytes)
///  48..52   mean_elevation (f32)
///  52..56   reserved (4 bytes, f64 alignment pad)
///  56..64   sweep_start_secs (f64)
///  64..72   sweep_end_secs (f64)
///
/// Array layout (version 0):
///   72..                azimuths (f32 × azimuth_count)
///   72 + az*4..         gate_values
///
/// Array layout (version 1):
///   72..                azimuths (f32 × azimuth_count)
///   72 + az*4..         radial_times (f64 × azimuth_count)
///   72 + az*4 + az*8..  gate_values
const HEADER_SIZE: usize = 72;

impl PrecomputedSweep {
    /// Serialize to binary blob for IDB storage.
    pub fn to_bytes(&self) -> Vec<u8> {
        let az = self.azimuth_count as usize;
        let gc = self.gate_count as usize;
        let ws = self.gate_values.word_size() as usize;
        let has_times = !self.radial_times.is_empty();
        let format_version: u8 = if has_times { 1 } else { 0 };
        let times_size = if has_times { az * 8 } else { 0 };
        let size = HEADER_SIZE
            + az * 4             // azimuths (f32)
            + times_size         // radial_times (f64), version 1 only
            + az * gc * ws; // gate_values (u8 or u16)
        let mut buf = Vec::with_capacity(size);

        // Header (72 bytes)
        buf.extend_from_slice(&self.azimuth_count.to_le_bytes()); // 0..4
        buf.extend_from_slice(&self.gate_count.to_le_bytes()); // 4..8
        buf.extend_from_slice(&self.first_gate_range_km.to_le_bytes()); // 8..16
        buf.extend_from_slice(&self.gate_interval_km.to_le_bytes()); // 16..24
        buf.extend_from_slice(&self.max_range_km.to_le_bytes()); // 24..32
        buf.extend_from_slice(&self.scale.to_le_bytes()); // 32..36
        buf.extend_from_slice(&self.offset.to_le_bytes()); // 36..40
        buf.extend_from_slice(&self.radial_count.to_le_bytes()); // 40..44
        buf.push(self.gate_values.word_size()); // 44
        buf.push(format_version); // 45
        buf.extend_from_slice(&[0u8; 2]); // 46..48 reserved
        buf.extend_from_slice(&self.mean_elevation.to_le_bytes()); // 48..52
        buf.extend_from_slice(&[0u8; 4]); // 52..56 alignment pad
        buf.extend_from_slice(&self.sweep_start_secs.to_le_bytes()); // 56..64
        buf.extend_from_slice(&self.sweep_end_secs.to_le_bytes()); // 64..72

        // Azimuths
        for &a in &self.azimuths {
            buf.extend_from_slice(&a.to_le_bytes());
        }

        // Radial times (version 1 only)
        if has_times {
            for &t in &self.radial_times {
                buf.extend_from_slice(&t.to_le_bytes());
            }
        }

        // Gate data (native word size)
        match &self.gate_values {
            GateValues::U8(vals) => {
                buf.extend_from_slice(vals);
            }
            GateValues::U16(vals) => {
                for &v in vals {
                    buf.extend_from_slice(&v.to_le_bytes());
                }
            }
        }

        buf
    }
}

/// Parsed header from a serialized sweep blob, with byte offsets for zero-copy access.
pub struct SweepHeader {
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub scale: f32,
    pub offset: f32,
    pub radial_count: u32,
    /// Bytes per gate value (1 for u8, 2 for u16).
    pub data_word_size: u8,
    pub mean_elevation: f32,
    pub sweep_start_secs: f64,
    pub sweep_end_secs: f64,
    /// Byte offset to azimuths array (f32 × azimuth_count)
    pub azimuths_offset: u32,
    /// Byte offset to radial_times array (f64 × azimuth_count), or 0 if absent.
    pub radial_times_offset: u32,
    /// Byte offset to gate_values array (u8 or u16 × azimuth_count × gate_count)
    pub gate_values_offset: u32,
}

/// Parse only the 72-byte header from a serialized sweep blob.
/// Returns scalar metadata and byte offsets for each array section,
/// without allocating or copying any array data.
pub fn parse_sweep_header(data: &[u8]) -> Result<SweepHeader, String> {
    if data.len() < HEADER_SIZE {
        return Err(format!(
            "Sweep blob too small: {} < {} header",
            data.len(),
            HEADER_SIZE
        ));
    }

    let azimuth_count = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let gate_count = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let first_gate_range_km = f64::from_le_bytes(data[8..16].try_into().unwrap());
    let gate_interval_km = f64::from_le_bytes(data[16..24].try_into().unwrap());
    let max_range_km = f64::from_le_bytes(data[24..32].try_into().unwrap());
    let scale = f32::from_le_bytes(data[32..36].try_into().unwrap());
    let offset = f32::from_le_bytes(data[36..40].try_into().unwrap());
    let radial_count = u32::from_le_bytes(data[40..44].try_into().unwrap());
    let data_word_size = data[44];
    let format_version = data[45];
    let mean_elevation = f32::from_le_bytes(data[48..52].try_into().unwrap());
    let sweep_start_secs = f64::from_le_bytes(data[56..64].try_into().unwrap());
    let sweep_end_secs = f64::from_le_bytes(data[64..72].try_into().unwrap());

    let az = azimuth_count as usize;

    let azimuths_offset = HEADER_SIZE;
    let (radial_times_offset, gate_values_offset) = if format_version >= 1 {
        let rt_off = azimuths_offset + az * 4;
        let gv_off = rt_off + az * 8;
        (rt_off, gv_off)
    } else {
        (0, azimuths_offset + az * 4)
    };

    Ok(SweepHeader {
        azimuth_count,
        gate_count,
        first_gate_range_km,
        gate_interval_km,
        max_range_km,
        scale,
        offset,
        radial_count,
        data_word_size,
        mean_elevation,
        sweep_start_secs,
        sweep_end_secs,
        azimuths_offset: azimuths_offset as u32,
        radial_times_offset: radial_times_offset as u32,
        gate_values_offset: gate_values_offset as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn test_precomputed_sweep_header_roundtrip() {
        let radial_times: Vec<f64> = (0..720).map(|i| 1700000000.5 + i as f64 * 0.028).collect();
        let sweep = PrecomputedSweep {
            azimuth_count: 720,
            gate_count: 1832,
            first_gate_range_km: 2.125,
            gate_interval_km: 0.25,
            max_range_km: 460.125,
            scale: 2.0,
            offset: 66.0,
            radial_count: 720,
            mean_elevation: 0.5,
            sweep_start_secs: 1700000000.5,
            sweep_end_secs: 1700000020.3,
            azimuths: (0..720).map(|i| i as f32 * 0.5).collect(),
            radial_times: radial_times.clone(),
            gate_values: GateValues::U8(vec![0u8; 720 * 1832]),
        };

        let bytes = sweep.to_bytes();
        let header = parse_sweep_header(&bytes).unwrap();

        assert_eq!(header.azimuth_count, 720);
        assert_eq!(header.gate_count, 1832);
        assert!((header.first_gate_range_km - 2.125).abs() < 1e-10);
        assert!((header.gate_interval_km - 0.25).abs() < 1e-10);
        assert!((header.max_range_km - 460.125).abs() < 1e-10);
        assert!((header.scale - 2.0).abs() < 1e-6);
        assert!((header.offset - 66.0).abs() < 1e-6);
        assert_eq!(header.radial_count, 720);
        assert_eq!(header.data_word_size, 1);
        assert!((header.mean_elevation - 0.5).abs() < 1e-6);
        assert!((header.sweep_start_secs - 1700000000.5).abs() < 1e-10);
        assert!((header.sweep_end_secs - 1700000020.3).abs() < 1e-10);
        assert_eq!(header.azimuths_offset, 72);
        assert!(header.radial_times_offset > 0);
        assert_eq!(header.radial_times_offset, 72 + 720 * 4);
        assert_eq!(header.gate_values_offset, 72 + 720 * 4 + 720 * 8);
    }

    #[wasm_bindgen_test]
    fn test_precomputed_sweep_legacy_no_radial_times() {
        let sweep = PrecomputedSweep {
            azimuth_count: 4,
            gate_count: 2,
            first_gate_range_km: 1.0,
            gate_interval_km: 0.5,
            max_range_km: 2.0,
            scale: 1.0,
            offset: 0.0,
            radial_count: 4,
            mean_elevation: 0.5,
            sweep_start_secs: 100.0,
            sweep_end_secs: 110.0,
            azimuths: vec![0.0, 90.0, 180.0, 270.0],
            radial_times: Vec::new(),
            gate_values: GateValues::U8(vec![0u8; 8]),
        };

        let bytes = sweep.to_bytes();
        let header = parse_sweep_header(&bytes).unwrap();

        // Version 0: no radial times
        assert_eq!(header.radial_times_offset, 0);
        assert_eq!(header.gate_values_offset, 72 + 4 * 4);
    }

    #[wasm_bindgen_test]
    fn test_parse_sweep_header_too_small() {
        let data = vec![0u8; 50];
        assert!(parse_sweep_header(&data).is_err());
    }

    #[wasm_bindgen_test]
    fn test_gate_values_word_size() {
        assert_eq!(GateValues::U8(vec![]).word_size(), 1);
        assert_eq!(GateValues::U16(vec![]).word_size(), 2);
    }

    #[wasm_bindgen_test]
    fn test_precomputed_sweep_u16_roundtrip() {
        let sweep = PrecomputedSweep {
            azimuth_count: 4,
            gate_count: 2,
            first_gate_range_km: 1.0,
            gate_interval_km: 0.5,
            max_range_km: 2.0,
            scale: 1.0,
            offset: 0.0,
            radial_count: 4,
            mean_elevation: 1.3,
            sweep_start_secs: 100.0,
            sweep_end_secs: 110.0,
            azimuths: vec![0.0, 90.0, 180.0, 270.0],
            radial_times: vec![100.0, 102.5, 105.0, 107.5],
            gate_values: GateValues::U16(vec![100, 200, 300, 400, 500, 600, 700, 800]),
        };

        let bytes = sweep.to_bytes();
        let header = parse_sweep_header(&bytes).unwrap();
        assert_eq!(header.data_word_size, 2);
        assert_eq!(header.azimuth_count, 4);
        assert_eq!(header.gate_count, 2);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── parse_sweep_header boundary / version-1 with zero azimuths ────────────

    #[wasm_bindgen_test]
    fn parse_sweep_header_exact_header_size_zero_arrays() {
        // A sweep with zero azimuths/gates serializes to exactly HEADER_SIZE
        // (72) bytes for version 0, and parses at the boundary (len < 72 is the
        // only error case).
        let sweep = PrecomputedSweep {
            azimuth_count: 0,
            gate_count: 0,
            first_gate_range_km: 1.0,
            gate_interval_km: 0.5,
            max_range_km: 2.0,
            scale: 3.0,
            offset: 4.0,
            radial_count: 0,
            mean_elevation: 7.5,
            sweep_start_secs: 100.0,
            sweep_end_secs: 110.0,
            azimuths: vec![],
            radial_times: vec![],
            gate_values: GateValues::U8(vec![]),
        };
        let bytes = sweep.to_bytes();
        assert_eq!(bytes.len(), 72);
        let header = parse_sweep_header(&bytes).unwrap();
        assert_eq!(header.azimuth_count, 0);
        assert_eq!(header.gate_count, 0);
        assert_eq!(header.data_word_size, 1);
        assert!((header.scale - 3.0).abs() < 1e-6);
        assert!((header.offset - 4.0).abs() < 1e-6);
        assert!((header.mean_elevation - 7.5).abs() < 1e-6);
        // Version 0 (no radial_times): radial_times_offset == 0 and
        // gate_values_offset sits right after the (empty) azimuth array.
        assert_eq!(header.radial_times_offset, 0);
        assert_eq!(header.azimuths_offset, 72);
        assert_eq!(header.gate_values_offset, 72);
    }

    #[wasm_bindgen_test]
    fn parse_sweep_header_too_small_by_one_byte() {
        // 71 bytes is one short of the header and must error.
        let data = vec![0u8; HEADER_SIZE - 1];
        assert!(parse_sweep_header(&data).is_err());
        // The error message names both lengths. (SweepHeader has no Debug, so
        // unwrap_err() won't compile — match the Err out instead.)
        let msg = match parse_sweep_header(&data) {
            Err(m) => m,
            Ok(_) => panic!("expected a too-small error"),
        };
        assert!(msg.contains("71"));
        assert!(msg.contains("72"));
    }
}
