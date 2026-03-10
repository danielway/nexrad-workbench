//! Core key types for storage.
//!
//! These types provide strongly-typed identifiers for the storage layer:
//! - `SiteId`: Radar site identifier (e.g., "KDMX")
//! - `UnixMillis`: Timestamp in milliseconds since Unix epoch
//! - `ScanKey`: Identifies a complete volume scan
//! - `SweepDataKey`: Identifies a pre-computed sweep (scan + elevation + product)

use serde::{Deserialize, Serialize};
use std::fmt;

/// Radar site identifier (4-character ICAO code).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SiteId(pub String);

impl SiteId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for SiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for SiteId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for SiteId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Unix timestamp in milliseconds.
///
/// Using milliseconds provides sub-second precision for record-level timing
/// while maintaining compatibility with JavaScript Date.now().
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UnixMillis(pub i64);

impl UnixMillis {
    pub fn now() -> Self {
        Self(js_sys::Date::now() as i64)
    }

    pub fn from_secs(secs: i64) -> Self {
        Self(secs * 1000)
    }

    pub fn as_secs(&self) -> i64 {
        self.0 / 1000
    }
}

impl fmt::Display for UnixMillis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identifies a complete volume scan.
///
/// A scan is uniquely identified by site + start time. The start time
/// is derived from the first record's timestamp (from VCP metadata or
/// first radial collection time).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScanKey {
    pub site: SiteId,
    pub scan_start: UnixMillis,
}

impl ScanKey {
    pub fn new(site: impl Into<SiteId>, scan_start: UnixMillis) -> Self {
        Self {
            site: site.into(),
            scan_start,
        }
    }

    /// Convert to storage key string: "KDMX|1700000000000"
    pub fn to_storage_key(&self) -> String {
        format!("{}|{}", self.site.0, self.scan_start.0)
    }

    /// Parse from storage key string.
    pub fn from_storage_key(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.split('|').collect();
        if parts.len() != 2 {
            return None;
        }
        let scan_start = parts[1].parse::<i64>().ok()?;
        Some(Self {
            site: SiteId(parts[0].to_string()),
            scan_start: UnixMillis(scan_start),
        })
    }

    /// Creates a ScanKey from a site ID and timestamp in seconds.
    pub fn from_secs(site_id: &str, timestamp_secs: i64) -> Self {
        Self {
            site: SiteId(site_id.to_string()),
            scan_start: UnixMillis::from_secs(timestamp_secs),
        }
    }
}

impl fmt::Display for ScanKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.site, self.scan_start)
    }
}

/// Identifies a pre-computed sweep blob in the `sweeps` IDB store.
///
/// Key format: "SITE|SCAN_MS|ELEV_NUM|PRODUCT"
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SweepDataKey {
    pub scan: ScanKey,
    pub elevation_number: u8,
    pub product: String,
}

impl SweepDataKey {
    pub fn new(scan: ScanKey, elevation_number: u8, product: impl Into<String>) -> Self {
        Self {
            scan,
            elevation_number,
            product: product.into(),
        }
    }

    pub fn to_storage_key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.scan.site.0, self.scan.scan_start.0, self.elevation_number, self.product
        )
    }
}

impl fmt::Display for SweepDataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}#{}",
            self.scan, self.elevation_number, self.product
        )
    }
}

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
    pub sweep_start_secs: f64,
    pub sweep_end_secs: f64,
    pub azimuths: Vec<f32>,
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
///  45..48   reserved (3 bytes)
///  48..52   mean_elevation (f32)
///  52..56   reserved (4 bytes, f64 alignment pad)
///  56..64   sweep_start_secs (f64)
///  64..72   sweep_end_secs (f64)
const HEADER_SIZE: usize = 72;

impl PrecomputedSweep {
    /// Serialize to binary blob for IDB storage.
    pub fn to_bytes(&self) -> Vec<u8> {
        let az = self.azimuth_count as usize;
        let gc = self.gate_count as usize;
        let ws = self.gate_values.word_size() as usize;
        let size = HEADER_SIZE
            + az * 4             // azimuths (f32)
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
        buf.extend_from_slice(&[0u8; 3]); // 45..48 reserved
        buf.extend_from_slice(&self.mean_elevation.to_le_bytes()); // 48..52
        buf.extend_from_slice(&[0u8; 4]); // 52..56 alignment pad
        buf.extend_from_slice(&self.sweep_start_secs.to_le_bytes()); // 56..64
        buf.extend_from_slice(&self.sweep_end_secs.to_le_bytes()); // 64..72

        // Azimuths
        for &a in &self.azimuths {
            buf.extend_from_slice(&a.to_le_bytes());
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
    let mean_elevation = f32::from_le_bytes(data[48..52].try_into().unwrap());
    let sweep_start_secs = f64::from_le_bytes(data[56..64].try_into().unwrap());
    let sweep_end_secs = f64::from_le_bytes(data[64..72].try_into().unwrap());

    let az = azimuth_count as usize;

    let azimuths_offset = HEADER_SIZE;
    let gate_values_offset = azimuths_offset + az * 4;

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
        gate_values_offset: gate_values_offset as u32,
    })
}

/// Completeness state for a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanCompleteness {
    /// No records present for this scan.
    Missing,
    /// Some records present but no VCP metadata (can't determine expected count).
    PartialNoVcp,
    /// Some records present with VCP metadata (can determine expected count).
    PartialWithVcp,
    /// All expected records present.
    Complete,
}

impl ScanCompleteness {
    /// Compute completeness from scan index entry.
    pub fn from_counts(has_vcp: bool, present: u32, expected: Option<u32>) -> Self {
        if present == 0 {
            return Self::Missing;
        }

        match expected {
            Some(exp) if present >= exp => Self::Complete,
            Some(_) if has_vcp => Self::PartialWithVcp,
            Some(_) => Self::PartialNoVcp,
            None if has_vcp => Self::PartialWithVcp,
            None => Self::PartialNoVcp,
        }
    }
}

/// Lightweight sweep metadata persisted in the scan index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepMeta {
    /// Start timestamp (Unix seconds with sub-second precision).
    pub start: f64,
    /// End timestamp (Unix seconds with sub-second precision).
    pub end: f64,
    /// Elevation angle in degrees.
    pub elevation: f32,
    /// Elevation number (index used for selective record querying).
    pub elevation_number: u8,
}

/// A single elevation cut extracted from a VCP message (Message Type 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedVcpElevation {
    /// Elevation angle in degrees.
    pub angle: f32,
    /// Waveform type: "CS", "CDW", "CDWO", "B", "SPP".
    pub waveform: String,
    /// Surveillance PRF number (1-8), relates to unambiguous range.
    pub prf_number: u8,
    /// SAILS (Supplemental Adaptive Intra-Volume Low-Level Scan) cut.
    pub is_sails: bool,
    /// MRLE (Mid-Volume Rescan of Low-Level Elevations) cut.
    pub is_mrle: bool,
    /// BASE TILT cut.
    pub is_base_tilt: bool,
}

/// Full Volume Coverage Pattern extracted from a NEXRAD VCP message (Type 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedVcp {
    /// VCP number (e.g., 215, 35, 212).
    pub number: u16,
    /// Ordered elevation cuts in this VCP.
    pub elevations: Vec<ExtractedVcpElevation>,
}

/// Metadata for a scan stored in the scan index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIndexEntry {
    pub scan: ScanKey,
    /// Whether VCP metadata record (record 0) is present.
    pub has_vcp: bool,
    /// Full Volume Coverage Pattern extracted from the scan data.
    #[serde(default)]
    pub vcp: Option<ExtractedVcp>,
    /// Expected number of records if known from VCP.
    pub expected_records: Option<u32>,
    /// Number of records currently stored.
    pub present_records: u32,
    /// File name from archive (if downloaded from archive).
    pub file_name: Option<String>,
    /// Total size of all stored records in bytes.
    pub total_size_bytes: u64,
    /// When this entry was last updated.
    pub updated_at: UnixMillis,
    /// When this entry was last accessed (for LRU eviction).
    #[serde(default = "UnixMillis::now")]
    pub last_accessed_at: UnixMillis,
    /// Actual scan end timestamp (Unix seconds), populated after decode.
    #[serde(default)]
    pub end_timestamp_secs: Option<i64>,
    /// Sweep metadata, populated after decode.
    #[serde(default)]
    pub sweeps: Option<Vec<SweepMeta>>,
    /// Whether pre-computed sweep blobs are stored for this scan.
    #[serde(default)]
    pub has_precomputed_sweeps: bool,
}

impl ScanIndexEntry {
    pub fn new(scan: ScanKey) -> Self {
        let now = UnixMillis::now();
        Self {
            scan,
            has_vcp: false,
            vcp: None,
            expected_records: None,
            present_records: 0,
            file_name: None,
            total_size_bytes: 0,
            updated_at: now,
            last_accessed_at: now,
            end_timestamp_secs: None,
            sweeps: None,
            has_precomputed_sweeps: false,
        }
    }

    pub fn completeness(&self) -> ScanCompleteness {
        ScanCompleteness::from_counts(self.has_vcp, self.present_records, self.expected_records)
    }

    /// Convert to storage key string.
    pub fn storage_key(&self) -> String {
        self.scan.to_storage_key()
    }
}

/// All known radar products for sweep pre-computation.
pub const ALL_PRODUCTS: &[&str] = &[
    "reflectivity",
    "velocity",
    "spectrum_width",
    "differential_reflectivity",
    "correlation_coefficient",
    "differential_phase",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_key_storage_format() {
        let key = ScanKey::new("KDMX", UnixMillis(1700000000000));
        assert_eq!(key.to_storage_key(), "KDMX|1700000000000");

        let parsed = ScanKey::from_storage_key("KDMX|1700000000000").unwrap();
        assert_eq!(parsed, key);
    }

    #[test]
    fn test_scan_key_from_secs() {
        let key = ScanKey::from_secs("KDMX", 1700000000);
        assert_eq!(key.scan_start.0, 1700000000000);
        assert_eq!(key.to_storage_key(), "KDMX|1700000000000");
    }

    #[test]
    fn test_sweep_data_key_storage_format() {
        let scan = ScanKey::new("KDMX", UnixMillis(1700000000000));
        let key = SweepDataKey::new(scan, 1, "reflectivity");
        assert_eq!(key.to_storage_key(), "KDMX|1700000000000|1|reflectivity");
    }

    #[test]
    fn test_completeness_computation() {
        // Missing
        assert_eq!(
            ScanCompleteness::from_counts(false, 0, None),
            ScanCompleteness::Missing
        );

        // Partial without VCP
        assert_eq!(
            ScanCompleteness::from_counts(false, 5, None),
            ScanCompleteness::PartialNoVcp
        );

        // Partial with VCP
        assert_eq!(
            ScanCompleteness::from_counts(true, 5, Some(10)),
            ScanCompleteness::PartialWithVcp
        );

        // Complete
        assert_eq!(
            ScanCompleteness::from_counts(true, 10, Some(10)),
            ScanCompleteness::Complete
        );

        // Complete with more than expected
        assert_eq!(
            ScanCompleteness::from_counts(true, 12, Some(10)),
            ScanCompleteness::Complete
        );
    }

    #[test]
    fn test_scan_key_from_storage_key_invalid() {
        assert!(ScanKey::from_storage_key("").is_none());
        assert!(ScanKey::from_storage_key("KDMX").is_none());
        assert!(ScanKey::from_storage_key("KDMX|not_a_number").is_none());
        assert!(ScanKey::from_storage_key("A|B|C").is_none());
    }

    #[test]
    fn test_scan_key_roundtrip() {
        let key = ScanKey::new("KFWS", UnixMillis(1609459200000));
        let serialized = key.to_storage_key();
        let parsed = ScanKey::from_storage_key(&serialized).unwrap();
        assert_eq!(parsed.site.0, "KFWS");
        assert_eq!(parsed.scan_start.0, 1609459200000);
    }

    #[test]
    fn test_sweep_data_key_roundtrip() {
        let scan = ScanKey::new("KLOT", UnixMillis(1700000000000));
        let key = SweepDataKey::new(scan, 3, "velocity");
        assert_eq!(key.to_storage_key(), "KLOT|1700000000000|3|velocity");
        assert_eq!(key.elevation_number, 3);
        assert_eq!(key.product, "velocity");
    }

    #[test]
    fn test_unix_millis_conversion() {
        let ms = UnixMillis::from_secs(1700000000);
        assert_eq!(ms.0, 1700000000000);
        assert_eq!(ms.as_secs(), 1700000000);
    }

    #[test]
    fn test_site_id_from_conversions() {
        let s1: SiteId = "KDMX".into();
        let s2: SiteId = String::from("KDMX").into();
        assert_eq!(s1, s2);
        assert_eq!(format!("{}", s1), "KDMX");
    }

    #[test]
    fn test_precomputed_sweep_header_roundtrip() {
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
        assert_eq!(header.gate_values_offset, 72 + 720 * 4);
    }

    #[test]
    fn test_parse_sweep_header_too_small() {
        let data = vec![0u8; 50];
        assert!(parse_sweep_header(&data).is_err());
    }

    #[test]
    fn test_gate_values_word_size() {
        assert_eq!(GateValues::U8(vec![]).word_size(), 1);
        assert_eq!(GateValues::U16(vec![]).word_size(), 2);
    }

    #[test]
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
            gate_values: GateValues::U16(vec![100, 200, 300, 400, 500, 600, 700, 800]),
        };

        let bytes = sweep.to_bytes();
        let header = parse_sweep_header(&bytes).unwrap();
        assert_eq!(header.data_word_size, 2);
        assert_eq!(header.azimuth_count, 4);
        assert_eq!(header.gate_count, 2);
    }
}
