//! Core key types for record-based storage.
//!
//! These types provide strongly-typed identifiers for the storage layer:
//! - `SiteId`: Radar site identifier (e.g., "KDMX")
//! - `UnixMillis`: Timestamp in milliseconds since Unix epoch
//! - `ScanKey`: Identifies a complete volume scan
//! - `RecordKey`: Identifies an individual record within a scan
//!
//! ## Record ID Derivation
//!
//! Records within a scan are identified by a sequence number (0-based).
//! For archive files, this is the order of bzip2 blocks in the file.
//! For realtime streaming, this is the chunk sequence number.
//!
//! The first record (id=0) typically contains LDM/VCP metadata needed
//! to interpret the scan structure.

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
        #[cfg(target_arch = "wasm32")]
        {
            Self(js_sys::Date::now() as i64)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            let duration = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            Self(duration.as_millis() as i64)
        }
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

    /// Convert from legacy ScanKey format (site_id + timestamp in seconds).
    pub fn from_legacy(site_id: &str, timestamp_secs: i64) -> Self {
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

/// Identifies an individual record within a scan.
///
/// Records are the atomic unit of storage. Each record contains a bzip2-compressed
/// chunk of radar data, typically covering a portion of one sweep.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordKey {
    pub scan: ScanKey,
    /// Record sequence number within the scan (0-based).
    /// Record 0 typically contains VCP/LDM metadata.
    pub record_id: u32,
}

impl RecordKey {
    pub fn new(scan: ScanKey, record_id: u32) -> Self {
        Self { scan, record_id }
    }

    /// Convert to storage key string: "KDMX|1700000000000|12"
    pub fn to_storage_key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.scan.site.0, self.scan.scan_start.0, self.record_id
        )
    }

    /// Parse from storage key string.
    pub fn from_storage_key(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.split('|').collect();
        if parts.len() != 3 {
            return None;
        }
        let scan_start = parts[1].parse::<i64>().ok()?;
        let record_id = parts[2].parse::<u32>().ok()?;
        Some(Self {
            scan: ScanKey {
                site: SiteId(parts[0].to_string()),
                scan_start: UnixMillis(scan_start),
            },
            record_id,
        })
    }
}

impl fmt::Display for RecordKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.scan, self.record_id)
    }
}

/// A record blob containing compressed radar data.
///
/// Note: This struct is NOT serialized to JSON for IndexedDB storage.
/// The `data` field is stored directly as an ArrayBuffer in IndexedDB.
/// This struct is used for in-memory representation only.
#[derive(Debug, Clone)]
pub struct RecordBlob {
    pub key: RecordKey,
    /// Raw bzip2-compressed bytes.
    pub data: Vec<u8>,
}

impl RecordBlob {
    pub fn new(key: RecordKey, data: Vec<u8>) -> Self {
        Self { key, data }
    }

    pub fn size_bytes(&self) -> u32 {
        self.data.len() as u32
    }
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
}

impl ScanIndexEntry {
    pub fn new(scan: ScanKey) -> Self {
        Self {
            scan,
            has_vcp: false,
            expected_records: None,
            present_records: 0,
            file_name: None,
            total_size_bytes: 0,
            updated_at: UnixMillis::now(),
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

/// Metadata for an individual record stored in the record index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordIndexEntry {
    pub key: RecordKey,
    /// Precise timestamp for this record if derivable from header.
    /// If None, use scan_start + record ordering.
    pub record_time: Option<UnixMillis>,
    /// Compressed size in bytes.
    pub size_bytes: u32,
    /// Whether this record contains VCP/LDM metadata.
    pub has_vcp: bool,
    /// When this record was stored.
    pub stored_at: UnixMillis,
}

impl RecordIndexEntry {
    pub fn new(key: RecordKey, size_bytes: u32) -> Self {
        Self {
            key,
            record_time: None,
            size_bytes,
            has_vcp: false,
            stored_at: UnixMillis::now(),
        }
    }

    pub fn with_time(mut self, time: UnixMillis) -> Self {
        self.record_time = Some(time);
        self
    }

    pub fn with_vcp(mut self, has_vcp: bool) -> Self {
        self.has_vcp = has_vcp;
        self
    }

    /// Convert to storage key string.
    pub fn storage_key(&self) -> String {
        self.key.to_storage_key()
    }

    /// Get the effective time for this record (record_time or scan_start).
    pub fn effective_time(&self) -> UnixMillis {
        self.record_time.unwrap_or(self.key.scan.scan_start)
    }
}

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
    fn test_record_key_storage_format() {
        let scan = ScanKey::new("KDMX", UnixMillis(1700000000000));
        let key = RecordKey::new(scan, 12);
        assert_eq!(key.to_storage_key(), "KDMX|1700000000000|12");

        let parsed = RecordKey::from_storage_key("KDMX|1700000000000|12").unwrap();
        assert_eq!(parsed, key);
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
