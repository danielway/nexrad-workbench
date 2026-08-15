//! Core key types for storage.
//!
//! These types provide strongly-typed identifiers for the storage layer:
//! - `SiteId`: Radar site identifier (e.g., "KDMX")
//! - `UnixMillis`: Timestamp in milliseconds since Unix epoch
//! - `ScanKey`: Identifies a complete volume scan
//! - `SweepDataKey`: Identifies a pre-computed sweep (scan + elevation + product)

use crate::data::vcp_timing::ExtractedVcp;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Upper-bound sentinel for IDB prefix ranges. Sorts after any character
/// that appears in real storage keys (decimal digits + `|`), so
/// `"PREFIX|" .. "PREFIX|\u{FFFF}"` captures every key with that prefix
/// without spilling into the lexicographic neighbor.
pub const PREFIX_RANGE_UPPER: char = '\u{FFFF}';

/// Why a storage-key string failed to parse back into a typed key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyParseError {
    WrongFieldCount { expected: usize, got: usize },
    BadTimestamp(String),
    BadElevation(String),
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyParseError::WrongFieldCount { expected, got } => {
                write!(f, "expected {expected} '|'-separated fields, got {got}")
            }
            KeyParseError::BadTimestamp(s) => write!(f, "bad timestamp field {s:?}"),
            KeyParseError::BadElevation(s) => write!(f, "bad elevation field {s:?}"),
        }
    }
}

/// Radar site identifier (4-character ICAO code).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SiteId(pub String);

impl SiteId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Inclusive lower / sentinel upper bounds for the scan-index key range
    /// covering all entries for this site (`"SITE|<ms>"`). Real IDB uses
    /// these via `IdbKeyRange::bound`.
    pub fn idb_prefix_bounds(&self) -> (String, String) {
        let lower = format!("{}|", self.0);
        let upper = format!("{}|{}", self.0, PREFIX_RANGE_UPPER);
        (lower, upper)
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
        use web_time::{SystemTime, UNIX_EPOCH};
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self(ms)
    }

    pub fn from_secs(secs: i64) -> Self {
        Self(secs * 1000)
    }

    /// Construct from sub-second-precision Unix seconds. Preserves the
    /// fractional part as milliseconds (rounded), so a value parsed from a
    /// volume header at 1700000000.789 lands at 1_700_000_000_789 rather
    /// than the truncated 1_700_000_000_000.
    pub fn from_secs_f64(secs: f64) -> Self {
        Self((secs * 1000.0).round() as i64)
    }

    pub fn as_secs(&self) -> i64 {
        self.0 / 1000
    }

    /// Convert back to fractional Unix seconds. Round-trips
    /// `from_secs_f64` to within float precision.
    pub fn as_secs_f64(&self) -> f64 {
        self.0 as f64 / 1000.0
    }
}

impl fmt::Display for UnixMillis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identifies a complete volume scan.
///
/// A scan is uniquely identified by site + start time, where the start
/// time is the volume-header collection time (whole seconds) decoded from
/// the volume's Start chunk. Both ingest paths derive it the same way —
/// the archive path in `worker_ingest`, the real-time path via
/// `realtime::streaming::volume_header_start_secs` — so the same physical
/// volume always resolves to the same key regardless of how it was
/// acquired. No filename string, S3 upload time, or lag estimate is
/// involved in the identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScanKey {
    pub site: SiteId,
    /// ACTUAL category: Unix-millis volume collection start time, decoded
    /// from the volume header. Authoritative for display and comparison.
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
    pub fn from_storage_key(key: &str) -> Result<Self, KeyParseError> {
        let parts: Vec<&str> = key.split('|').collect();
        if parts.len() != 2 {
            return Err(KeyParseError::WrongFieldCount {
                expected: 2,
                got: parts.len(),
            });
        }
        let scan_start = parts[1]
            .parse::<i64>()
            .map_err(|_| KeyParseError::BadTimestamp(parts[1].to_string()))?;
        Ok(Self {
            site: SiteId(parts[0].to_string()),
            scan_start: UnixMillis(scan_start),
        })
    }

    /// Bounds for the sweep-store key range covering every blob belonging
    /// to this scan (`"SITE|MS|<elev>|<product>"`). Used to delete an
    /// entire scan's blobs in one IDB range-delete without enumerating
    /// product names.
    pub fn idb_prefix_bounds(&self) -> (String, String) {
        let prefix = format!("{}|", self.to_storage_key());
        let upper = format!("{prefix}{PREFIX_RANGE_UPPER}");
        (prefix, upper)
    }

    /// Creates a ScanKey from a site ID and timestamp in seconds.
    pub fn from_secs(site_id: &str, timestamp_secs: i64) -> Self {
        Self {
            site: SiteId(site_id.to_string()),
            scan_start: UnixMillis::from_secs(timestamp_secs),
        }
    }

    /// Creates a ScanKey from a site ID and sub-second-precision Unix
    /// seconds. Use this on the streaming-loop path where the provisional
    /// or radial-parsed start is known to fractional precision; the
    /// fractional part survives into the IDB key.
    pub fn from_secs_f64(site_id: &str, timestamp_secs: f64) -> Self {
        Self {
            site: SiteId(site_id.to_string()),
            scan_start: UnixMillis::from_secs_f64(timestamp_secs),
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

    /// Parse from storage key string ("SITE|SCAN_MS|ELEV_NUM|PRODUCT").
    /// The product is the final field and may itself contain `|`.
    ///
    /// Symmetric inverse of [`Self::to_storage_key`]. No production caller
    /// reconstructs sweep keys today (the worker boundary ships scan keys);
    /// kept so the key vocabulary parses both ways — exercised by the
    /// `data::keys` tests.
    #[allow(dead_code)] // Doc above: symmetric parse kept deliberately; test-exercised only.
    pub fn from_storage_key(key: &str) -> Result<Self, KeyParseError> {
        let parts: Vec<&str> = key.splitn(4, '|').collect();
        if parts.len() != 4 {
            return Err(KeyParseError::WrongFieldCount {
                expected: 4,
                got: parts.len(),
            });
        }
        let scan_start = parts[1]
            .parse::<i64>()
            .map_err(|_| KeyParseError::BadTimestamp(parts[1].to_string()))?;
        let elevation_number = parts[2]
            .parse::<u8>()
            .map_err(|_| KeyParseError::BadElevation(parts[2].to_string()))?;
        Ok(Self {
            scan: ScanKey {
                site: SiteId(parts[0].to_string()),
                scan_start: UnixMillis(scan_start),
            },
            elevation_number,
            product: parts[3].to_string(),
        })
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
    /// Compute completeness from cached vs planned sweep counts.
    pub fn from_counts(has_vcp: bool, cached: u32, planned: Option<u32>) -> Self {
        if cached == 0 {
            return Self::Missing;
        }

        match planned {
            Some(exp) if cached >= exp => Self::Complete,
            Some(_) if has_vcp => Self::PartialWithVcp,
            Some(_) => Self::PartialNoVcp,
            None if has_vcp => Self::PartialWithVcp,
            None => Self::PartialNoVcp,
        }
    }
}

/// Per-sweep metadata for one cached sweep (the realized state of a single
/// VCP cut). Stored in `ScanIndexEntry::cached_sweeps`.
///
/// The VCP describes the *plan* (angle, waveform, PRF rates); a `CachedSweep`
/// describes what actually got ingested and stored — measured-from-radial
/// timing plus the list of products whose blobs we successfully wrote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSweep {
    /// ACTUAL category: sweep start time (Unix seconds, sub-second
    /// precision), derived from the earliest radial's collection
    /// timestamp. Authoritative — the canvas and timeline use this
    /// directly for placing the sweep.
    pub start: f64,
    /// ACTUAL category: sweep end time (Unix seconds, sub-second
    /// precision), latest radial's collection timestamp.
    pub end: f64,
    /// Elevation angle in degrees.
    pub elevation: f32,
    /// Elevation number (index used for selective record querying).
    pub elevation_number: u8,
    /// Azimuth angle (degrees) of the chronologically first radial in this sweep.
    #[serde(default)]
    pub start_azimuth: f32,
    /// Product names (matching `SweepDataKey` product strings) for which a
    /// sweep blob was successfully extracted and stored under this scan key.
    /// Empty when loaded from legacy index entries that predate product
    /// tracking — callers should treat empty as "unknown" and skip
    /// product-availability checks.
    #[serde(default)]
    pub cached_products: Vec<String>,
}

/// Timing metadata for a single elevation cut. Used as input to
/// `IndexedDbStore::upsert_scan`, which derives the persisted
/// [`CachedSweep`] from it.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepTiming {
    /// Sweep start time (Unix seconds, sub-second precision).
    pub start_secs: f64,
    /// Sweep end time (Unix seconds, sub-second precision).
    pub end_secs: f64,
    /// Mean elevation angle in degrees across the sweep's radials.
    pub elevation_angle: f32,
    /// Azimuth angle (degrees) of the chronologically first radial.
    pub start_azimuth: f32,
}

/// One product's pre-computed sweep blob, paired with its product name.
/// The IDB layer derives the storage key from the surrounding context
/// (`ScanKey` + `elevation_number` + this product).
#[derive(Debug, Clone)]
pub struct ProductBlob {
    /// Product name string (matches the `&str` constants in
    /// `ingest_phases::PRODUCTS`, e.g. `"reflectivity"`).
    pub product: &'static str,
    /// Raw sweep bytes (`PrecomputedSweep::to_bytes()` output).
    pub bytes: Vec<u8>,
}

/// One elevation's contribution to an upsert: the timing the IDB layer
/// needs to derive a [`CachedSweep`], plus the per-product blobs whose
/// storage keys it derives. An `ElevationUpload` with `blobs.is_empty()`
/// is dropped by the IDB layer — phantom manifest entries become
/// structurally impossible.
#[derive(Debug, Clone)]
pub struct ElevationUpload {
    pub elevation_number: u8,
    pub timing: SweepTiming,
    pub blobs: Vec<ProductBlob>,
}

impl ElevationUpload {
    /// Derive the `CachedSweep` manifest entry the IDB layer will persist
    /// for this upload. Exposed so callers can build a response payload
    /// (e.g. the worker's `IngestResponse.sweeps` field) using the same
    /// derivation the IDB layer uses internally — single source of truth.
    pub fn to_cached_sweep(&self) -> CachedSweep {
        CachedSweep {
            start: self.timing.start_secs,
            end: self.timing.end_secs,
            elevation: self.timing.elevation_angle,
            elevation_number: self.elevation_number,
            start_azimuth: self.timing.start_azimuth,
            cached_products: self.blobs.iter().map(|b| b.product.to_string()).collect(),
        }
    }
}

/// Everything a caller knows or has just learned about a scan in a single
/// `upsert_scan` call. `vcp` and `file_name` are interpreted as
/// "set on first write, fill-in-if-`None` on merge" — callers pass what
/// they currently know without branching on whether the scan exists.
#[derive(Debug, Clone)]
pub struct ScanHeader {
    pub scan: ScanKey,
    pub vcp: Option<ExtractedVcp>,
    pub file_name: Option<String>,
}

/// Metadata for a scan stored in the scan index.
///
/// Two roles in this struct:
///
/// - **Plan**: `vcp` describes what the radar *intends* to scan (ordered
///   elevation cuts with waveform/PRF/azimuth-rate metadata). Static, comes
///   from the Message Type 5 record.
/// - **Cached state**: `cached_sweeps` lists the sweeps we've actually
///   ingested and stored — the realized subset of the VCP plan, with
///   measured timing and the products whose blobs we successfully wrote.
///
/// These are correlated but neither derives from the other; the join key is
/// `elevation_number`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIndexEntry {
    /// Storage key, kept in the value for self-description after `get_all`.
    pub scan: ScanKey,
    /// Volume Coverage Pattern: the full ordered plan for the scan. `None`
    /// until the Message Type 5 (volume header) record is decoded.
    #[serde(default)]
    pub vcp: Option<ExtractedVcp>,
    /// Source file name from archive ingest, or a synthetic
    /// `live_<site>_<ts>.nexrad` for real-time. Used in user-facing labels.
    pub file_name: Option<String>,
    /// Sweeps that have been ingested and stored under this scan key. Each
    /// entry corresponds to one VCP cut whose data was successfully decoded
    /// into a sweep blob.
    #[serde(default)]
    pub cached_sweeps: Vec<CachedSweep>,
    /// Total size of all stored sweep blobs for this scan (bytes). Drives
    /// `total_cache_size` and LRU eviction sizing.
    pub total_size_bytes: u64,
}

impl ScanIndexEntry {
    /// Whether the VCP metadata record has been ingested.
    pub fn has_vcp(&self) -> bool {
        self.vcp.is_some()
    }

    /// Number of sweeps the VCP plans for this volume, or `None` if the VCP
    /// hasn't been ingested yet.
    pub fn planned_sweep_count(&self) -> Option<u32> {
        self.vcp.as_ref().map(|v| v.elevations.len() as u32)
    }

    /// Number of sweeps actually stored.
    pub fn cached_sweep_count(&self) -> u32 {
        self.cached_sweeps
            .iter()
            .map(|sweep| sweep.elevation_number)
            .collect::<std::collections::HashSet<_>>()
            .len() as u32
    }

    /// Latest radial collection timestamp (Unix seconds) across all cached
    /// sweeps, or `None` if no sweeps have been ingested yet.
    pub fn end_timestamp_secs(&self) -> Option<i64> {
        self.cached_sweeps.iter().map(|s| s.end as i64).max()
    }

    pub fn completeness(&self) -> ScanCompleteness {
        ScanCompleteness::from_counts(
            self.has_vcp(),
            self.cached_sweep_count(),
            self.planned_sweep_count(),
        )
    }

    /// Whether a sweep for the given elevation number has already been stored
    /// under this scan.
    ///
    /// This is the dedup granularity for **filter-scoped** archive fetches.
    /// `completeness()` can't answer "do I already have this cut?" for a
    /// scoped scan — a scan that deliberately stores a subset of the VCP plan
    /// never reaches `Complete`, so a completeness check would re-download the
    /// whole file on every revisit. Ingest stores every product it can extract
    /// for a decoded elevation together, so elevation presence (not per-product)
    /// is the right unit: if the cut is present, all of its available products
    /// are present.
    pub fn has_elevation(&self, elevation_number: u8) -> bool {
        self.cached_sweeps
            .iter()
            .any(|s| s.elevation_number == elevation_number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::vcp_timing::ExtractedVcpElevation;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn test_scan_key_storage_format() {
        let key = ScanKey::new("KDMX", UnixMillis(1700000000000));
        assert_eq!(key.to_storage_key(), "KDMX|1700000000000");

        let parsed = ScanKey::from_storage_key("KDMX|1700000000000").unwrap();
        assert_eq!(parsed, key);
    }

    #[wasm_bindgen_test]
    fn test_scan_key_from_secs() {
        let key = ScanKey::from_secs("KDMX", 1700000000);
        assert_eq!(key.scan_start.0, 1700000000000);
        assert_eq!(key.to_storage_key(), "KDMX|1700000000000");
    }

    #[wasm_bindgen_test]
    fn test_scan_key_from_secs_f64_preserves_subsecond() {
        let key = ScanKey::from_secs_f64("KDMX", 1_700_000_000.789);
        assert_eq!(key.scan_start.0, 1_700_000_000_789);
        assert_eq!(key.to_storage_key(), "KDMX|1700000000789");
    }

    /// Identity-equality guard for the archive↔realtime scan-fracture fix:
    /// both paths key a scan by the whole-second volume-header time, but
    /// reach `ScanKey` through different constructors — the archive worker
    /// via `from_secs_f64(dt.timestamp() as f64)` and historical/queue code
    /// via `from_secs(i64)`. For the same whole-second start these MUST
    /// produce a byte-identical storage key, otherwise the same physical
    /// volume fractures into two scans.
    #[wasm_bindgen_test]
    fn test_archive_and_realtime_keys_match_for_whole_second_header_time() {
        for t in [0i64, 1_700_000_000, 1_700_000_137, 1_699_999_999] {
            let from_int = ScanKey::from_secs("KDMX", t);
            let from_header = ScanKey::from_secs_f64("KDMX", t as f64);
            assert_eq!(
                from_int, from_header,
                "key constructors diverged for whole-second start {t}"
            );
            assert_eq!(from_int.to_storage_key(), from_header.to_storage_key());
        }
    }

    #[wasm_bindgen_test]
    fn test_unix_millis_secs_f64_round_trip() {
        let ms = UnixMillis::from_secs_f64(1_700_000_000.789);
        assert_eq!(ms.0, 1_700_000_000_789);
        let back = ms.as_secs_f64();
        assert!((back - 1_700_000_000.789).abs() < 1e-6);
        // Whole seconds agree with the integer constructor — the property the
        // scan coverage-key matching (Scan::key_ms) relies on.
        assert_eq!(
            UnixMillis::from_secs_f64(1_700_000_000.0),
            UnixMillis::from_secs(1_700_000_000)
        );
    }

    /// Pin the property the worker boundary now relies on: the dispatch
    /// site builds a typed `ScanKey` from `(site, secs_f64)`, and any
    /// code that wants to compare against the worker's storage-key string
    /// must get an identical result via `to_storage_key()`. Without this,
    /// the dropped `from_storage_key().expect()` parse in
    /// `handle_chunk_ingested_outcome` could silently disagree with
    /// itself across the wire.
    #[wasm_bindgen_test]
    fn test_scan_key_storage_key_round_trip_via_typed_dispatch() {
        let secs_f64 = 1_700_000_000.789;
        let typed = ScanKey::from_secs_f64("KDMX", secs_f64);
        let storage = typed.to_storage_key();
        let parsed = ScanKey::from_storage_key(&storage).unwrap();
        assert_eq!(typed, parsed);
        assert_eq!(parsed.scan_start.as_secs_f64(), secs_f64);
    }

    #[wasm_bindgen_test]
    fn test_sweep_data_key_storage_format() {
        let scan = ScanKey::new("KDMX", UnixMillis(1700000000000));
        let key = SweepDataKey::new(scan, 1, "reflectivity");
        assert_eq!(key.to_storage_key(), "KDMX|1700000000000|1|reflectivity");
    }

    #[wasm_bindgen_test]
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

    #[wasm_bindgen_test]
    fn test_has_elevation() {
        let mk_sweep = |elevation_number: u8| CachedSweep {
            start: 0.0,
            end: 0.0,
            elevation: 0.5,
            elevation_number,
            start_azimuth: 0.0,
            cached_products: vec!["reflectivity".to_string()],
        };
        let entry = ScanIndexEntry {
            scan: ScanKey::from_secs("KDMX", 1_700_000_000),
            vcp: None,
            file_name: None,
            cached_sweeps: vec![mk_sweep(1), mk_sweep(3)],
            total_size_bytes: 0,
        };

        // Stored cuts are hits; the gap between them (elev 2) is not.
        assert!(entry.has_elevation(1));
        assert!(entry.has_elevation(3));
        assert!(!entry.has_elevation(2));

        // An entry with no cached sweeps is never a hit — the filter-scoped
        // fetch must proceed.
        let empty = ScanIndexEntry {
            scan: ScanKey::from_secs("KDMX", 1_700_000_000),
            vcp: None,
            file_name: None,
            cached_sweeps: vec![],
            total_size_bytes: 0,
        };
        assert!(!empty.has_elevation(1));
    }

    /// `completeness()` can never produce the (planned=Some, has_vcp=false)
    /// combination that `from_counts` accepts as a public input: both are
    /// derived from `self.vcp`, so `planned_sweep_count()` is `Some` iff
    /// `has_vcp()`. Pins the contract divergence noted in the audit.
    #[wasm_bindgen_test]
    fn completeness_never_produces_planned_without_vcp() {
        let mk = |vcp: Option<ExtractedVcp>, cached: usize| ScanIndexEntry {
            scan: ScanKey::from_secs("KDMX", 1_700_000_000),
            vcp,
            file_name: None,
            cached_sweeps: (0..cached)
                .map(|i| CachedSweep {
                    start: 0.0,
                    end: 0.0,
                    elevation: 0.5,
                    elevation_number: i as u8 + 1,
                    start_azimuth: 0.0,
                    cached_products: vec![],
                })
                .collect(),
            total_size_bytes: 0,
        };
        // No VCP → planned is None, regardless of cached count.
        let no_vcp = mk(None, 5);
        assert!(!no_vcp.has_vcp());
        assert_eq!(no_vcp.planned_sweep_count(), None);
        assert_eq!(no_vcp.completeness(), ScanCompleteness::PartialNoVcp);

        // With a VCP → planned is Some and has_vcp is true together.
        let with_vcp = mk(
            Some(ExtractedVcp {
                number: 212,
                elevations: vec![elev(Some(20.0)), elev(Some(20.0))],
            }),
            1,
        );
        assert!(with_vcp.has_vcp());
        assert_eq!(with_vcp.planned_sweep_count(), Some(2));
        assert_eq!(with_vcp.completeness(), ScanCompleteness::PartialWithVcp);
    }

    fn elev(rate: Option<f32>) -> ExtractedVcpElevation {
        ExtractedVcpElevation {
            angle: 0.5,
            waveform: "CS".to_string(),
            prf_number: 1,
            is_sails: false,
            is_mrle: false,
            is_base_tilt: false,
            azimuth_rate: rate,
        }
    }

    #[wasm_bindgen_test]
    fn test_scan_key_from_storage_key_invalid() {
        assert_eq!(
            ScanKey::from_storage_key(""),
            Err(KeyParseError::WrongFieldCount {
                expected: 2,
                got: 1
            })
        );
        assert_eq!(
            ScanKey::from_storage_key("KDMX"),
            Err(KeyParseError::WrongFieldCount {
                expected: 2,
                got: 1
            })
        );
        assert_eq!(
            ScanKey::from_storage_key("KDMX|not_a_number"),
            Err(KeyParseError::BadTimestamp("not_a_number".into()))
        );
        assert_eq!(
            ScanKey::from_storage_key("A|B|C"),
            Err(KeyParseError::WrongFieldCount {
                expected: 2,
                got: 3
            })
        );
    }

    #[wasm_bindgen_test]
    fn test_sweep_data_key_from_storage_key() {
        // Round trip.
        let scan = ScanKey::new("KLOT", UnixMillis(1700000000000));
        let key = SweepDataKey::new(scan, 3, "velocity");
        let parsed = SweepDataKey::from_storage_key(&key.to_storage_key()).unwrap();
        assert_eq!(parsed, key);

        // Malformed inputs.
        assert_eq!(
            SweepDataKey::from_storage_key("KDMX|1700000000000|1"),
            Err(KeyParseError::WrongFieldCount {
                expected: 4,
                got: 3
            })
        );
        assert_eq!(
            SweepDataKey::from_storage_key("KDMX|nope|1|reflectivity"),
            Err(KeyParseError::BadTimestamp("nope".into()))
        );
        assert_eq!(
            SweepDataKey::from_storage_key("KDMX|1700000000000|999|reflectivity"),
            Err(KeyParseError::BadElevation("999".into()))
        );
        // The product field is last and tolerates embedded separators.
        let odd = SweepDataKey::from_storage_key("KDMX|1700000000000|1|weird|product").unwrap();
        assert_eq!(odd.product, "weird|product");
    }

    #[wasm_bindgen_test]
    fn site_idb_prefix_bounds_cover_site_and_exclude_neighbors() {
        let (lo, hi) = SiteId::new("KDMX").idb_prefix_bounds();
        assert_eq!(lo, "KDMX|");
        assert_eq!(hi, format!("KDMX|{}", PREFIX_RANGE_UPPER));
        // Every realistic `SITE|<13-digit-ms>` key falls strictly between
        // the two bounds.
        for ms in ["1000000000000", "1700000000000", "9999999999999"] {
            let k = format!("KDMX|{}", ms);
            assert!(lo.as_str() < k.as_str() && k.as_str() < hi.as_str());
        }
        // Lexicographic neighbors stay outside.
        for other in ["KDMY|1700000000000", "KDMW|1700000000000"] {
            assert!(!(lo.as_str() < other && other < hi.as_str()));
        }
    }

    #[wasm_bindgen_test]
    fn scan_idb_prefix_bounds_cover_blobs_and_exclude_neighbors() {
        let scan = ScanKey::new("KDMX", UnixMillis(1700000000000));
        let (lo, hi) = scan.idb_prefix_bounds();
        assert_eq!(lo, "KDMX|1700000000000|");
        for sweep_key in [
            "KDMX|1700000000000|1|reflectivity",
            "KDMX|1700000000000|3|velocity",
            "KDMX|1700000000000|17|differential_phase",
        ] {
            assert!(lo.as_str() < sweep_key && sweep_key < hi.as_str());
        }
        // Adjacent ms / other sites -> different prefix, must not match.
        for foreign in [
            "KDMX|1700000000001|1|reflectivity",
            "KDMX|1699999999999|1|reflectivity",
            "KDMY|1700000000000|1|reflectivity",
        ] {
            assert!(
                !(lo.as_str() < foreign && foreign < hi.as_str()),
                "{} should be outside the scan range",
                foreign
            );
        }
    }

    #[wasm_bindgen_test]
    fn test_scan_key_roundtrip() {
        let key = ScanKey::new("KFWS", UnixMillis(1609459200000));
        let serialized = key.to_storage_key();
        let parsed = ScanKey::from_storage_key(&serialized).unwrap();
        assert_eq!(parsed.site.0, "KFWS");
        assert_eq!(parsed.scan_start.0, 1609459200000);
    }

    #[wasm_bindgen_test]
    fn test_sweep_data_key_roundtrip() {
        let scan = ScanKey::new("KLOT", UnixMillis(1700000000000));
        let key = SweepDataKey::new(scan, 3, "velocity");
        assert_eq!(key.to_storage_key(), "KLOT|1700000000000|3|velocity");
        assert_eq!(key.elevation_number, 3);
        assert_eq!(key.product, "velocity");
    }

    #[wasm_bindgen_test]
    fn test_unix_millis_conversion() {
        let ms = UnixMillis::from_secs(1700000000);
        assert_eq!(ms.0, 1700000000000);
        assert_eq!(ms.as_secs(), 1700000000);
    }

    #[wasm_bindgen_test]
    fn test_site_id_from_conversions() {
        let s1: SiteId = "KDMX".into();
        let s2: SiteId = String::from("KDMX").into();
        assert_eq!(s1, s2);
        assert_eq!(format!("{}", s1), "KDMX");
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::data::vcp_timing::ExtractedVcpElevation;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── KeyParseError Display ────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn key_parse_error_display_all_variants() {
        let wrong = KeyParseError::WrongFieldCount {
            expected: 2,
            got: 3,
        };
        assert_eq!(format!("{wrong}"), "expected 2 '|'-separated fields, got 3");

        let bad_ts = KeyParseError::BadTimestamp("xyz".to_string());
        // The {s:?} debug formatting wraps the field in quotes.
        assert_eq!(format!("{bad_ts}"), "bad timestamp field \"xyz\"");

        let bad_elev = KeyParseError::BadElevation("999".to_string());
        assert_eq!(format!("{bad_elev}"), "bad elevation field \"999\"");
    }

    // ── Display impls for the key types ──────────────────────────────────────

    #[wasm_bindgen_test]
    fn scan_key_display_format() {
        let key = ScanKey::new("KDMX", UnixMillis(1700000000000));
        // ScanKey Display is "{site}@{scan_start}"; SiteId and UnixMillis
        // Display are the bare string / integer.
        assert_eq!(format!("{key}"), "KDMX@1700000000000");
    }

    #[wasm_bindgen_test]
    fn sweep_data_key_display_format() {
        let scan = ScanKey::new("KLOT", UnixMillis(1700000000000));
        let key = SweepDataKey::new(scan, 3, "velocity");
        // "{scan}@{elev}#{product}" where scan itself renders as "site@ms".
        assert_eq!(format!("{key}"), "KLOT@1700000000000@3#velocity");
    }

    #[wasm_bindgen_test]
    fn unix_millis_display_is_raw_integer() {
        assert_eq!(format!("{}", UnixMillis(0)), "0");
        assert_eq!(format!("{}", UnixMillis(-5)), "-5");
        assert_eq!(format!("{}", UnixMillis(1700000000000)), "1700000000000");
    }

    // ── UnixMillis conversions: truncation / negatives ───────────────────────

    #[wasm_bindgen_test]
    fn unix_millis_as_secs_truncates_toward_zero() {
        // 1999 ms -> 1 s (integer division drops the fractional second).
        assert_eq!(UnixMillis(1999).as_secs(), 1);
        assert_eq!(UnixMillis(1000).as_secs(), 1);
        assert_eq!(UnixMillis(999).as_secs(), 0);
        // Negative integer division in Rust truncates toward zero.
        assert_eq!(UnixMillis(-1999).as_secs(), -1);
        assert_eq!(UnixMillis(-1).as_secs(), 0);
    }

    #[wasm_bindgen_test]
    fn unix_millis_from_secs_and_as_secs_f64_negative() {
        let ms = UnixMillis::from_secs(-1234);
        assert_eq!(ms.0, -1_234_000);
        assert_eq!(ms.as_secs(), -1234);
        assert!((ms.as_secs_f64() - (-1234.0)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn unix_millis_from_secs_f64_rounds_to_nearest_ms() {
        // 0.0014 s = 1.4 ms -> rounds to 1.
        assert_eq!(UnixMillis::from_secs_f64(0.0014).0, 1);
        // 0.0015 s = 1.5 ms -> rounds half away from zero to 2.
        assert_eq!(UnixMillis::from_secs_f64(0.0015).0, 2);
        // 0.0016 s = 1.6 ms -> 2.
        assert_eq!(UnixMillis::from_secs_f64(0.0016).0, 2);
        // Negative fractional rounds magnitude away from zero.
        assert_eq!(UnixMillis::from_secs_f64(-0.0015).0, -2);
    }

    // ── ScanCompleteness::from_counts: branches the existing test misses ──────

    #[wasm_bindgen_test]
    fn completeness_partial_no_planned_with_vcp_is_partial_with_vcp() {
        // planned = None but has_vcp = true -> PartialWithVcp (the
        // `None if has_vcp` arm, not exercised by the existing test which only
        // pairs has_vcp with a Some(planned)).
        assert_eq!(
            ScanCompleteness::from_counts(true, 5, None),
            ScanCompleteness::PartialWithVcp
        );
        // planned = None and has_vcp = false -> PartialNoVcp.
        assert_eq!(
            ScanCompleteness::from_counts(false, 5, None),
            ScanCompleteness::PartialNoVcp
        );
    }

    #[wasm_bindgen_test]
    fn completeness_zero_cached_is_always_missing() {
        // cached == 0 short-circuits to Missing regardless of vcp/planned.
        assert_eq!(
            ScanCompleteness::from_counts(true, 0, Some(10)),
            ScanCompleteness::Missing
        );
        assert_eq!(
            ScanCompleteness::from_counts(true, 0, None),
            ScanCompleteness::Missing
        );
        assert_eq!(
            ScanCompleteness::from_counts(false, 0, Some(10)),
            ScanCompleteness::Missing
        );
    }

    #[wasm_bindgen_test]
    fn completeness_partial_with_planned_no_vcp() {
        // planned = Some, cached < planned, has_vcp = false -> PartialNoVcp.
        assert_eq!(
            ScanCompleteness::from_counts(false, 3, Some(10)),
            ScanCompleteness::PartialNoVcp
        );
        // exact-boundary: cached == planned -> Complete (>= check).
        assert_eq!(
            ScanCompleteness::from_counts(true, 10, Some(10)),
            ScanCompleteness::Complete
        );
    }

    // ── ElevationUpload::to_cached_sweep ─────────────────────────────────────

    #[wasm_bindgen_test]
    fn elevation_upload_to_cached_sweep_maps_fields_and_products() {
        let upload = ElevationUpload {
            elevation_number: 4,
            timing: SweepTiming {
                start_secs: 100.5,
                end_secs: 110.75,
                elevation_angle: 1.45,
                start_azimuth: 271.0,
            },
            blobs: vec![
                ProductBlob {
                    product: "reflectivity",
                    bytes: vec![1, 2, 3],
                },
                ProductBlob {
                    product: "velocity",
                    bytes: vec![4, 5],
                },
            ],
        };
        let cs = upload.to_cached_sweep();
        assert!((cs.start - 100.5).abs() < 1e-9);
        assert!((cs.end - 110.75).abs() < 1e-9);
        assert!((cs.elevation - 1.45).abs() < 1e-6);
        assert_eq!(cs.elevation_number, 4);
        assert!((cs.start_azimuth - 271.0).abs() < 1e-6);
        // Product names are collected in order.
        assert_eq!(cs.cached_products, vec!["reflectivity", "velocity"]);
    }

    #[wasm_bindgen_test]
    fn elevation_upload_to_cached_sweep_empty_blobs_yields_no_products() {
        let upload = ElevationUpload {
            elevation_number: 1,
            timing: SweepTiming {
                start_secs: 0.0,
                end_secs: 0.0,
                elevation_angle: 0.5,
                start_azimuth: 0.0,
            },
            blobs: vec![],
        };
        let cs = upload.to_cached_sweep();
        assert!(cs.cached_products.is_empty());
        assert_eq!(cs.elevation_number, 1);
    }

    // ── ScanIndexEntry accessors ─────────────────────────────────────────────

    fn mk_cached(elevation_number: u8, start: f64, end: f64) -> CachedSweep {
        CachedSweep {
            start,
            end,
            elevation: 0.5,
            elevation_number,
            start_azimuth: 0.0,
            cached_products: vec![],
        }
    }

    fn mk_elev(rate: Option<f32>) -> ExtractedVcpElevation {
        ExtractedVcpElevation {
            angle: 0.5,
            waveform: "CS".to_string(),
            prf_number: 1,
            is_sails: false,
            is_mrle: false,
            is_base_tilt: false,
            azimuth_rate: rate,
        }
    }

    #[wasm_bindgen_test]
    fn scan_index_entry_counts_and_has_vcp() {
        let entry = ScanIndexEntry {
            scan: ScanKey::from_secs("KDMX", 1_700_000_000),
            vcp: Some(ExtractedVcp {
                number: 212,
                elevations: vec![mk_elev(None), mk_elev(None), mk_elev(None)],
            }),
            file_name: None,
            cached_sweeps: vec![mk_cached(1, 0.0, 1.0), mk_cached(2, 1.0, 2.0)],
            total_size_bytes: 0,
        };
        assert!(entry.has_vcp());
        assert_eq!(entry.planned_sweep_count(), Some(3));
        assert_eq!(entry.cached_sweep_count(), 2);

        let no_vcp = ScanIndexEntry {
            scan: ScanKey::from_secs("KDMX", 1_700_000_000),
            vcp: None,
            file_name: None,
            cached_sweeps: vec![],
            total_size_bytes: 0,
        };
        assert!(!no_vcp.has_vcp());
        assert_eq!(no_vcp.planned_sweep_count(), None);
        assert_eq!(no_vcp.cached_sweep_count(), 0);
    }

    #[wasm_bindgen_test]
    fn scan_index_entry_end_timestamp_max_and_none() {
        // No sweeps -> None.
        let empty = ScanIndexEntry {
            scan: ScanKey::from_secs("KDMX", 1_700_000_000),
            vcp: None,
            file_name: None,
            cached_sweeps: vec![],
            total_size_bytes: 0,
        };
        assert_eq!(empty.end_timestamp_secs(), None);

        // Max across sweeps; the `end as i64` cast truncates the fraction.
        let entry = ScanIndexEntry {
            scan: ScanKey::from_secs("KDMX", 1_700_000_000),
            vcp: None,
            file_name: None,
            cached_sweeps: vec![
                mk_cached(1, 100.0, 110.9),
                mk_cached(2, 120.0, 130.6),
                mk_cached(3, 105.0, 115.2),
            ],
            total_size_bytes: 0,
        };
        // Largest end is 130.6 -> truncated to 130.
        assert_eq!(entry.end_timestamp_secs(), Some(130));
    }

    // ── SweepDataKey::from_storage_key elevation overflow / negative ──────────

    #[wasm_bindgen_test]
    fn sweep_data_key_elevation_out_of_u8_range_is_bad_elevation() {
        // 256 overflows u8 -> BadElevation (existing test only checks "999").
        assert_eq!(
            SweepDataKey::from_storage_key("KDMX|1700000000000|256|reflectivity"),
            Err(KeyParseError::BadElevation("256".into()))
        );
        // Negative is not a valid u8 either.
        assert_eq!(
            SweepDataKey::from_storage_key("KDMX|1700000000000|-1|reflectivity"),
            Err(KeyParseError::BadElevation("-1".into()))
        );
        // 255 is the inclusive upper bound of u8 and parses fine.
        let ok = SweepDataKey::from_storage_key("KDMX|1700000000000|255|reflectivity").unwrap();
        assert_eq!(ok.elevation_number, 255);
    }
}
