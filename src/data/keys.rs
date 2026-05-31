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

/// ACTUAL category: Unix-seconds volume collection start time, parsed from
/// the volume header's first-radial collection time. Authoritative — only
/// known once the worker has decoded at least one radial of the volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfirmedStart(pub f64);

/// AVAILABILITY-derived category: Unix-seconds volume start estimate
/// computed as `upload_date_time − median_availability_lag` from the Start
/// chunk's S3 upload time. Lands within ~1s of the eventual confirmed value;
/// used as the IDB key (chunks have to be written before any radial parse)
/// and as the UI's display value until [`ConfirmedStart`] arrives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProvisionalStart(pub f64);

/// In-flight identity + timing for the current live volume.
///
/// Replaces the parallel `current_scan_key: String` and
/// `current_volume_start: f64` fields that previously lived on
/// `LiveModeState`. Holding both timestamp variants in one place makes the
/// provisional → confirmed transition explicit — UI surfaces read
/// [`Self::best_start_secs`] (which swaps to confirmed atomically when the
/// worker reports the header time) and IDB code reads [`Self::scan_key`]
/// (always derived from the provisional value, since that's what was
/// written at first-chunk arrival).
///
/// `scan_key` doubles as the volume's stable identity for the lifetime of
/// the volume: it's built once when the Start chunk is processed and never
/// updated, so consumers can match on it without worrying about the
/// timestamp underneath shifting. A future revision may switch identity to
/// a synthetic `(site, NEXRAD volume_index)` pair without changing this
/// type's surface.
#[derive(Debug, Clone)]
pub struct LiveVolumeAnchor {
    pub scan_key: ScanKey,
    pub provisional: ProvisionalStart,
    pub confirmed: Option<ConfirmedStart>,
}

impl LiveVolumeAnchor {
    /// Construct an anchor for a freshly-arrived volume. The `scan_key`
    /// must already encode the provisional start (it's the IDB key the
    /// streaming loop has been using all along — see
    /// `realtime.rs::provisional_scan_start_secs`).
    pub fn new(scan_key: ScanKey, provisional: ProvisionalStart) -> Self {
        Self {
            scan_key,
            provisional,
            confirmed: None,
        }
    }

    /// Best-known volume start (Unix seconds). Confirmed when available,
    /// otherwise the provisional estimate. Every UI surface should use this
    /// for visual placement and timestamp comparisons.
    pub fn best_start_secs(&self) -> f64 {
        self.confirmed.map(|c| c.0).unwrap_or(self.provisional.0)
    }

    /// `true` once the worker has reported a confirmed start time. Useful
    /// for diagnostics and for distinguishing "we're guessing" from "we
    /// know" in debug overlays.
    #[allow(dead_code)]
    pub fn is_confirmed(&self) -> bool {
        self.confirmed.is_some()
    }

    /// Record the confirmed (radial-parsed) start time. Idempotent.
    pub fn confirm(&mut self, confirmed: ConfirmedStart) {
        self.confirmed = Some(confirmed);
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
#[derive(Debug, Clone)]
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
    /// Azimuth rotation rate in degrees/second from the VCP message.
    /// Primary input for sweep duration estimation: duration ≈ 360° / rate.
    #[serde(default)]
    pub azimuth_rate: Option<f32>,
}

/// Full Volume Coverage Pattern extracted from a NEXRAD VCP message (Type 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedVcp {
    /// VCP number (e.g., 215, 35, 212).
    pub number: u16,
    /// Ordered elevation cuts in this VCP.
    pub elevations: Vec<ExtractedVcpElevation>,
}

impl ExtractedVcp {
    /// Compute per-elevation sweep durations as fractions of total volume duration.
    ///
    /// Uses Method A (weight = 1/azimuth_rate) when azimuth rates are available,
    /// falling back to Method B category-based weights from empirical study, and
    /// finally to even distribution if neither is available.
    ///
    /// Returns a `Vec<f64>` with one entry per elevation, each being the estimated
    /// sweep duration in seconds for the given `total_volume_duration`.
    pub fn sweep_durations(&self, total_volume_duration: f64) -> Vec<f64> {
        if self.elevations.is_empty() {
            return Vec::new();
        }

        let weights: Vec<f64> = self
            .elevations
            .iter()
            .map(|e| {
                if let Some(rate) = e.azimuth_rate {
                    if rate > 0.0 {
                        return 1.0 / rate as f64;
                    }
                }
                // Method B fallback: use category-based weights
                let is_clear_air = crate::data::vcp::is_clear_air_vcp(self.number);
                1.0 / crate::data::vcp::fallback_azimuth_rate(
                    is_clear_air,
                    &e.waveform,
                    e.prf_number,
                )
            })
            .collect();

        let total_weight: f64 = weights.iter().sum();
        if total_weight <= 0.0 {
            // Shouldn't happen, but fall back to even distribution
            let even = total_volume_duration / self.elevations.len() as f64;
            return vec![even; self.elevations.len()];
        }

        weights
            .iter()
            .map(|w| (w / total_weight) * total_volume_duration)
            .collect()
    }

    /// Estimate total volume scan duration (seconds) from per-elevation azimuth rates.
    ///
    /// Computes `sum(360° / rate_i)` for each elevation. When azimuth rates are not
    /// available, uses Method B fallback rates. Returns `None` if there are no elevations.
    pub fn estimated_volume_duration(&self) -> Option<f64> {
        if self.elevations.is_empty() {
            return None;
        }

        let total: f64 = self
            .elevations
            .iter()
            .map(|e| {
                let rate = if let Some(r) = e.azimuth_rate {
                    if r > 0.0 {
                        r as f64
                    } else {
                        let is_clear_air = crate::data::vcp::is_clear_air_vcp(self.number);
                        crate::data::vcp::fallback_azimuth_rate(
                            is_clear_air,
                            &e.waveform,
                            e.prf_number,
                        )
                    }
                } else {
                    let is_clear_air = crate::data::vcp::is_clear_air_vcp(self.number);
                    crate::data::vcp::fallback_azimuth_rate(is_clear_air, &e.waveform, e.prf_number)
                };
                360.0 / rate
            })
            .sum();

        Some(total)
    }

    /// Compute cumulative start offsets (in seconds from volume start) for each elevation.
    ///
    /// Returns a `Vec<f64>` where entry `i` is the estimated start time offset of elevation `i`.
    #[allow(dead_code)]
    pub fn sweep_start_offsets(&self, total_volume_duration: f64) -> Vec<f64> {
        let durations = self.sweep_durations(total_volume_duration);
        let mut offsets = Vec::with_capacity(durations.len());
        let mut cumulative = 0.0;
        for dur in &durations {
            offsets.push(cumulative);
            cumulative += dur;
        }
        offsets
    }
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
        self.cached_sweeps.len() as u32
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

    #[wasm_bindgen_test]
    fn test_scan_key_from_storage_key_invalid() {
        assert!(ScanKey::from_storage_key("").is_none());
        assert!(ScanKey::from_storage_key("KDMX").is_none());
        assert!(ScanKey::from_storage_key("KDMX|not_a_number").is_none());
        assert!(ScanKey::from_storage_key("A|B|C").is_none());
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
