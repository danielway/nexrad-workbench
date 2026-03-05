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

    pub fn as_str(&self) -> &str {
        &self.0
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

    pub fn as_millis(&self) -> i64 {
        self.0
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

    /// Returns the key prefix for all sweeps in a scan: "SITE|SCAN_MS|"
    pub fn prefix_for_scan(scan: &ScanKey) -> String {
        format!("{}|{}|", scan.site.0, scan.scan_start.0)
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
    /// Number of gate values.
    pub fn len(&self) -> usize {
        match self {
            GateValues::U8(v) => v.len(),
            GateValues::U16(v) => v.len(),
        }
    }

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
            + az * gc * ws;      // gate_values (u8 or u16)
        let mut buf = Vec::with_capacity(size);

        // Header (72 bytes)
        buf.extend_from_slice(&self.azimuth_count.to_le_bytes());       // 0..4
        buf.extend_from_slice(&self.gate_count.to_le_bytes());          // 4..8
        buf.extend_from_slice(&self.first_gate_range_km.to_le_bytes()); // 8..16
        buf.extend_from_slice(&self.gate_interval_km.to_le_bytes());    // 16..24
        buf.extend_from_slice(&self.max_range_km.to_le_bytes());        // 24..32
        buf.extend_from_slice(&self.scale.to_le_bytes());               // 32..36
        buf.extend_from_slice(&self.offset.to_le_bytes());              // 36..40
        buf.extend_from_slice(&self.radial_count.to_le_bytes());        // 40..44
        buf.push(self.gate_values.word_size());                         // 44
        buf.extend_from_slice(&[0u8; 3]);                               // 45..48 reserved
        buf.extend_from_slice(&self.mean_elevation.to_le_bytes());      // 48..52
        buf.extend_from_slice(&[0u8; 4]);                               // 52..56 alignment pad
        buf.extend_from_slice(&self.sweep_start_secs.to_le_bytes());    // 56..64
        buf.extend_from_slice(&self.sweep_end_secs.to_le_bytes());      // 64..72

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

    /// Deserialize from binary blob.
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
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
        // 45..48 reserved
        let mean_elevation = f32::from_le_bytes(data[48..52].try_into().unwrap());
        // 52..56 alignment pad
        let sweep_start_secs = f64::from_le_bytes(data[56..64].try_into().unwrap());
        let sweep_end_secs = f64::from_le_bytes(data[64..72].try_into().unwrap());

        let az = azimuth_count as usize;
        let gc = gate_count as usize;
        let ws = data_word_size as usize;
        let expected = HEADER_SIZE + az * 4 + az * gc * ws;
        if data.len() < expected {
            return Err(format!(
                "Sweep blob too small: {} < {} expected",
                data.len(),
                expected
            ));
        }

        let mut pos = HEADER_SIZE;

        let azimuths = read_f32_slice(data, pos, az);
        pos += az * 4;

        let gate_values = if data_word_size == 1 {
            GateValues::U8(data[pos..pos + az * gc].to_vec())
        } else {
            GateValues::U16(read_u16_slice(data, pos, az * gc))
        };

        Ok(Self {
            azimuth_count,
            gate_count,
            first_gate_range_km,
            gate_interval_km,
            max_range_km,
            scale,
            offset,
            radial_count,
            mean_elevation,
            sweep_start_secs,
            sweep_end_secs,
            azimuths,
            gate_values,
        })
    }

    /// Total size in bytes when serialized.
    pub fn byte_size(&self) -> usize {
        let az = self.azimuth_count as usize;
        let ws = self.gate_values.word_size() as usize;
        HEADER_SIZE + az * 4 + az * self.gate_count as usize * ws
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

fn read_f32_slice(data: &[u8], offset: usize, count: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = offset + i * 4;
        out.push(f32::from_le_bytes(
            data[start..start + 4].try_into().unwrap(),
        ));
    }
    out
}

fn read_u16_slice(data: &[u8], offset: usize, count: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = offset + i * 2;
        out.push(u16::from_le_bytes(
            data[start..start + 2].try_into().unwrap(),
        ));
    }
    out
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

/// A time range with start and end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: UnixMillis,
    pub end: UnixMillis,
}

impl TimeRange {
    pub fn new(start: UnixMillis, end: UnixMillis) -> Self {
        Self { start, end }
    }

    pub fn contains(&self, time: UnixMillis) -> bool {
        time >= self.start && time <= self.end
    }

    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    pub fn duration_millis(&self) -> i64 {
        self.end.0 - self.start.0
    }
}

/// Merge adjacent/overlapping time ranges.
pub fn merge_time_ranges(mut ranges: Vec<TimeRange>, gap_threshold_ms: i64) -> Vec<TimeRange> {
    if ranges.is_empty() {
        return ranges;
    }

    ranges.sort_by_key(|r| r.start.0);

    let mut merged = Vec::with_capacity(ranges.len());
    let mut current = ranges[0];

    for range in ranges.into_iter().skip(1) {
        // Merge if overlapping or within gap threshold
        if range.start.0 <= current.end.0 + gap_threshold_ms {
            current.end = UnixMillis(current.end.0.max(range.end.0));
        } else {
            merged.push(current);
            current = range;
        }
    }
    merged.push(current);

    merged
}

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
        assert_eq!(key.scan_start.as_millis(), 1700000000000);
        assert_eq!(key.to_storage_key(), "KDMX|1700000000000");
    }

    #[test]
    fn test_sweep_data_key_storage_format() {
        let scan = ScanKey::new("KDMX", UnixMillis(1700000000000));
        let key = SweepDataKey::new(scan, 1, "reflectivity");
        assert_eq!(
            key.to_storage_key(),
            "KDMX|1700000000000|1|reflectivity"
        );
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
    fn test_merge_time_ranges() {
        let ranges = vec![
            TimeRange::new(UnixMillis(1000), UnixMillis(2000)),
            TimeRange::new(UnixMillis(1500), UnixMillis(2500)), // Overlapping
            TimeRange::new(UnixMillis(5000), UnixMillis(6000)), // Gap
            TimeRange::new(UnixMillis(6100), UnixMillis(7000)), // Small gap (100ms)
        ];

        // With 200ms gap threshold, should merge last two
        let merged = merge_time_ranges(ranges.clone(), 200);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].start.0, 1000);
        assert_eq!(merged[0].end.0, 2500);
        assert_eq!(merged[1].start.0, 5000);
        assert_eq!(merged[1].end.0, 7000);

        // With 50ms gap threshold, should not merge last two
        let merged = merge_time_ranges(ranges, 50);
        assert_eq!(merged.len(), 3);
    }
}
