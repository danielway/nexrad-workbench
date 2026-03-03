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

/// Pre-computed sweep data ready for GPU rendering.
///
/// Binary layout (little-endian):
/// - Header (44 bytes): azimuth_count, gate_count, first_gate_range_km,
///   gate_interval_km, max_range_km, scale, offset, radial_count
/// - Per-radial metadata (sorted by azimuth):
///   azimuths (f32), timestamps (f64, Unix ms), elevation_angles (f32)
/// - Gate data: gate_values (f32, row-major azimuth×gate)
pub struct PrecomputedSweep {
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub scale: f32,
    pub offset: f32,
    pub radial_count: u32,
    pub azimuths: Vec<f32>,
    pub timestamps: Vec<f64>,
    pub elevation_angles: Vec<f32>,
    pub gate_values: Vec<f32>,
}

const HEADER_SIZE: usize = 44;

impl PrecomputedSweep {
    /// Serialize to binary blob for IDB storage.
    pub fn to_bytes(&self) -> Vec<u8> {
        let az = self.azimuth_count as usize;
        let total_floats = az * self.gate_count as usize;
        let size = HEADER_SIZE
            + az * 4             // azimuths (f32)
            + az * 8             // timestamps (f64)
            + az * 4             // elevation_angles (f32)
            + total_floats * 4;  // gate_values (f32)
        let mut buf = Vec::with_capacity(size);

        // Header
        buf.extend_from_slice(&self.azimuth_count.to_le_bytes());
        buf.extend_from_slice(&self.gate_count.to_le_bytes());
        buf.extend_from_slice(&self.first_gate_range_km.to_le_bytes());
        buf.extend_from_slice(&self.gate_interval_km.to_le_bytes());
        buf.extend_from_slice(&self.max_range_km.to_le_bytes());
        buf.extend_from_slice(&self.scale.to_le_bytes());
        buf.extend_from_slice(&self.offset.to_le_bytes());
        buf.extend_from_slice(&self.radial_count.to_le_bytes());

        // Per-radial metadata
        for &a in &self.azimuths {
            buf.extend_from_slice(&a.to_le_bytes());
        }
        for &t in &self.timestamps {
            buf.extend_from_slice(&t.to_le_bytes());
        }
        for &e in &self.elevation_angles {
            buf.extend_from_slice(&e.to_le_bytes());
        }

        // Gate data
        for &v in &self.gate_values {
            buf.extend_from_slice(&v.to_le_bytes());
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

        let az = azimuth_count as usize;
        let gc = gate_count as usize;
        let expected = HEADER_SIZE + az * 4 + az * 8 + az * 4 + az * gc * 4;
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

        let timestamps = read_f64_slice(data, pos, az);
        pos += az * 8;

        let elevation_angles = read_f32_slice(data, pos, az);
        pos += az * 4;

        let gate_values = read_f32_slice(data, pos, az * gc);

        Ok(Self {
            azimuth_count,
            gate_count,
            first_gate_range_km,
            gate_interval_km,
            max_range_km,
            scale,
            offset,
            radial_count,
            azimuths,
            timestamps,
            elevation_angles,
            gate_values,
        })
    }

    /// Total size in bytes when serialized.
    pub fn byte_size(&self) -> usize {
        let az = self.azimuth_count as usize;
        HEADER_SIZE + az * 4 + az * 8 + az * 4 + az * self.gate_count as usize * 4
    }
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

fn read_f64_slice(data: &[u8], offset: usize, count: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = offset + i * 8;
        out.push(f64::from_le_bytes(
            data[start..start + 8].try_into().unwrap(),
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

/// Metadata for a scan stored in the scan index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIndexEntry {
    pub scan: ScanKey,
    /// Whether VCP metadata record (record 0) is present.
    pub has_vcp: bool,
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
