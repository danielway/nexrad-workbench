//! Type definitions for worker message payloads and public result types.

use crate::core::WorkerErrorKind;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Wire protocol: message-type tag strings (Rust ↔ JS)
//
// `worker.js` reads the `type` field of every request and writes the same
// field on every response. These two enums are the single Rust-side source
// of truth for those strings; the JS side must use the exact same literals,
// listed in the comment header at the top of `worker.js`. The round-trip
// invariant is pinned by `tests::request_type_strings_roundtrip` and
// `tests::response_type_strings_roundtrip` below.
// ---------------------------------------------------------------------------

/// Request message types (main thread → worker).
///
/// Strings appear as the `type` field on every outgoing message and must
/// stay in sync with the dispatch table in `worker.js`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum RequestType {
    Init,
    Ingest,
    IngestChunk,
    Render,
    RenderLive,
    RenderVolume,
}

impl RequestType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Ingest => "ingest",
            Self::IngestChunk => "ingest_chunk",
            Self::Render => "render",
            Self::RenderLive => "render_live",
            Self::RenderVolume => "render_volume",
        }
    }
}

/// Response message types (worker → main thread).
///
/// `id` accompanies every response except [`ResponseType::Ready`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum ResponseType {
    Ready,
    Ingested,
    ChunkIngested,
    Decoded,
    LiveDecoded,
    VolumeDecoded,
    Error,
}

impl ResponseType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Ingested => "ingested",
            Self::ChunkIngested => "chunk_ingested",
            Self::Decoded => "decoded",
            Self::LiveDecoded => "live_decoded",
            Self::VolumeDecoded => "volume_decoded",
            Self::Error => "error",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ready" => Self::Ready,
            "ingested" => Self::Ingested,
            "chunk_ingested" => Self::ChunkIngested,
            "decoded" => Self::Decoded,
            "live_decoded" => Self::LiveDecoded,
            "volume_decoded" => Self::VolumeDecoded,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Internal message types (serde-wasm-bindgen deserialization)
// ---------------------------------------------------------------------------

/// Header fields parsed from every worker response.
///
/// Always read first so the dispatch can tag the message and correlate it
/// with a pending request. `id` is `None` for the initial `ready` message
/// (which has no request) and `Some` for every other response type.
#[derive(Deserialize)]
pub(super) struct ResponseHeader {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(default)]
    pub id: Option<u64>,
}

/// Ingest result payload from the worker.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IngestResultMsg {
    pub scan_key: String,
    pub records_stored: u32,
    #[serde(default)]
    pub elevation_numbers: Vec<u8>,
    #[serde(default)]
    pub total_ms: f64,
    #[serde(default)]
    pub split_ms: f64,
    #[serde(default)]
    pub decompress_ms: f64,
    #[serde(default)]
    pub decode_ms: f64,
    #[serde(default)]
    pub extract_ms: f64,
    #[serde(default)]
    pub store_ms: f64,
    #[serde(default)]
    pub index_ms: f64,
    #[serde(default)]
    pub sweeps: Vec<crate::data::CachedSweep>,
    #[serde(default)]
    pub vcp: Option<crate::data::keys::ExtractedVcp>,
}

/// Chunk ingest result payload from the worker. Note: the worker also
/// echoes a `scanKey` field in the JSON, but we read the typed key from
/// the dispatch context instead — `serde` ignores the extra field.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChunkIngestResultMsg {
    #[serde(default)]
    pub sweeps_stored: u32,
    #[serde(default)]
    pub is_end: bool,
    #[serde(default)]
    pub total_ms: f64,
    #[serde(default)]
    pub elevations_completed: Vec<u8>,
    #[serde(default)]
    pub sweeps: Vec<crate::data::CachedSweep>,
    #[serde(default)]
    pub vcp: Option<crate::data::keys::ExtractedVcp>,
    #[serde(default)]
    pub current_elevation: Option<u8>,
    #[serde(default)]
    pub current_elevation_radials: Option<u32>,
    #[serde(default)]
    pub last_radial_azimuth: Option<f32>,
    #[serde(default)]
    pub last_radial_time_secs: Option<f64>,
    #[serde(default)]
    pub volume_header_time_secs: Option<f64>,
    #[serde(default)]
    pub chunk_min_time_secs: Option<f64>,
    #[serde(default)]
    pub chunk_max_time_secs: Option<f64>,
    #[serde(default)]
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    #[serde(default)]
    pub chunk_elev_az_ranges: Vec<(u8, f32, f32)>,
}

/// Scalar fields of the decoded sweep response from the worker.
/// ArrayBuffer fields (azimuths, gateValues, radialTimes) are extracted separately.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DecodedResultMsg {
    #[serde(default)]
    pub azimuth_count: u32,
    #[serde(default)]
    pub gate_count: u32,
    #[serde(default)]
    pub first_gate_range_km: f64,
    #[serde(default)]
    pub gate_interval_km: f64,
    #[serde(default)]
    pub max_range_km: f64,
    #[serde(default = "default_product")]
    pub product: String,
    #[serde(default)]
    pub radial_count: u32,
    #[serde(default)]
    pub fetch_ms: f64,
    #[serde(default)]
    pub deser_ms: f64,
    #[serde(default)]
    pub marshal_ms: f64,
    #[serde(default)]
    pub total_ms: f64,
    #[serde(default = "default_scale")]
    pub scale: f32,
    #[serde(default)]
    pub offset: f32,
    #[serde(default)]
    pub mean_elevation: f32,
    #[serde(default)]
    pub sweep_start_secs: f64,
    #[serde(default)]
    pub sweep_end_secs: f64,
    #[serde(default = "default_azimuth_spacing_deg")]
    pub azimuth_spacing_deg: f32,
}

fn default_azimuth_spacing_deg() -> f32 {
    1.0
}

fn default_product() -> String {
    "reflectivity".to_string()
}

fn default_scale() -> f32 {
    1.0
}

/// Per-sweep metadata in a volume decoded response.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VolumeSweepMetaMsg {
    #[serde(default)]
    pub elevation_deg: f32,
    #[serde(default)]
    pub azimuth_count: u32,
    #[serde(default)]
    pub gate_count: u32,
    #[serde(default)]
    pub first_gate_km: f32,
    #[serde(default)]
    pub gate_interval_km: f32,
    #[serde(default)]
    pub max_range_km: f32,
    #[serde(default)]
    pub data_offset: u32,
    #[serde(default)]
    pub scale: f32,
    #[serde(default)]
    pub offset: f32,
}

/// Scalar fields of the volume decoded response.
/// The `buffer` ArrayBuffer and `sweepMeta` array are extracted separately.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VolumeDecodedResultMsg {
    #[serde(default)]
    pub total_ms: f64,
    #[serde(default = "default_product")]
    pub product: String,
    #[serde(default = "default_word_size")]
    pub word_size: u8,
    #[serde(default)]
    pub sweep_meta: Vec<VolumeSweepMetaMsg>,
}

fn default_word_size() -> u8 {
    2
}

/// Error message from the worker.
///
/// `kind` is the structured category set by `worker.js`'s `classifyError`
/// (or by Rust code that throws an object with a `kind` field). Defaults
/// to [`WorkerErrorKind::Unknown`] when the worker shipped only a message.
#[derive(Deserialize)]
pub(super) struct ErrorMsg {
    pub id: u64,
    #[serde(default = "default_error_message")]
    pub message: String,
    #[serde(default = "default_error_kind")]
    pub kind: WorkerErrorKind,
}

fn default_error_message() -> String {
    "Unknown worker error".to_string()
}

fn default_error_kind() -> WorkerErrorKind {
    WorkerErrorKind::Unknown
}

// ---------------------------------------------------------------------------
// Outgoing request message types (main → worker)
// ---------------------------------------------------------------------------

/// Request message sent to the worker for ingest operations.
/// The `data` ArrayBuffer is set separately for zero-copy transfer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IngestRequestMsg<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub id: f64,
    pub site_id: &'a str,
    pub timestamp_secs: f64,
    pub file_name: &'a str,
    pub wanted_elevations: &'a [u8],
}

/// Request message sent to the worker for chunk ingest operations.
/// The `data` ArrayBuffer is set separately for zero-copy transfer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IngestChunkRequestMsg<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub id: f64,
    pub site_id: &'a str,
    pub timestamp_secs: f64,
    pub chunk_index: f64,
    pub is_start: bool,
    pub is_end: bool,
    pub file_name: &'a str,
    pub is_last_in_sweep: bool,
}

/// Request message sent to the worker for render operations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenderRequestMsg<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub id: f64,
    pub scan_key: &'a str,
    pub elevation_number: u8,
    pub product: &'a str,
}

/// Request message sent to the worker for volume render operations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenderVolumeRequestMsg<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub id: f64,
    pub scan_key: &'a str,
    pub product: &'a str,
    pub elevation_numbers: &'a [u8],
}

/// Request message sent to the worker for live render operations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenderLiveRequestMsg<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub id: f64,
    pub elevation_number: u8,
    pub product: &'a str,
}

// ---------------------------------------------------------------------------
// Public result/context types
// ---------------------------------------------------------------------------

/// Unique ID for tracking worker requests.
pub(super) type RequestId = u64;

/// Context for an ingest request.
#[allow(dead_code)]
pub(crate) struct IngestContext {
    /// Typed identifier for the volume being ingested. Built once at
    /// dispatch (in `WorkerPool::ingest`) so the worker round-trip
    /// doesn't re-parse the storage-key string — the JS side and the
    /// Rust side ultimately produce the same key from the same inputs.
    pub scan_key: crate::data::ScanKey,
    /// Volume scan start (Unix seconds, sub-second precision). Same value
    /// `scan_key` was built from; kept for diagnostics.
    pub timestamp_secs: f64,
    pub file_name: String,
    pub fetch_latency_ms: f64,
}

/// Successful ingest result from the worker.
pub(crate) struct IngestResult {
    pub context: IngestContext,
    /// Typed identifier for the ingested scan, parsed from the storage-key
    /// string the worker actually wrote under (derived from the decoded
    /// volume-header time). Falls back to `context.scan_key` only if that
    /// string is unparseable.
    pub scan_key: crate::data::ScanKey,
    /// Number of records stored in IDB.
    pub records_stored: u32,
    /// Unique elevation numbers found across all records.
    pub elevation_numbers: Vec<u8>,
    /// Per-sweep metadata extracted from radials during ingest.
    pub sweeps: Vec<crate::data::CachedSweep>,
    /// Full extracted VCP pattern (from Message Type 5).
    /// Available for direct VCP inspection; primary propagation is via IDB metadata.
    #[allow(dead_code)]
    pub vcp: Option<crate::data::keys::ExtractedVcp>,
    /// Total time in worker (ms).
    pub total_ms: f64,
    /// Sub-phase timing: record splitting.
    pub split_ms: f64,
    /// Sub-phase timing: decompression.
    pub decompress_ms: f64,
    /// Sub-phase timing: decoding records.
    pub decode_ms: f64,
    /// Sub-phase timing: sweep extraction.
    pub extract_ms: f64,
    /// Sub-phase timing: IDB store.
    pub store_ms: f64,
    /// Sub-phase timing: index update.
    pub index_ms: f64,
}

/// Context for a per-chunk ingest request (real-time streaming).
#[allow(dead_code)]
pub(crate) struct ChunkIngestContext {
    pub site_id: String,
    /// Typed identifier for the in-progress volume. Built once at dispatch
    /// from `(site_id, timestamp_secs)`; every chunk of the volume shares
    /// the same value. Consumers should read this rather than re-parsing
    /// the storage-key string the worker emits on the response.
    pub scan_key: crate::data::ScanKey,
    /// Volume scan start (Unix seconds, sub-second precision). Same value
    /// `scan_key` was built from; kept for diagnostics and lag math.
    pub timestamp_secs: f64,
    pub chunk_index: u32,
    pub is_end: bool,
}

/// Successful per-chunk ingest result from the worker.
pub(crate) struct ChunkIngestResult {
    pub context: ChunkIngestContext,
    /// Typed identifier for the in-progress volume. Same value as
    /// `context.scan_key`; surfaced separately for ergonomic access.
    pub scan_key: crate::data::ScanKey,
    /// Elevation numbers that became complete with this chunk.
    pub elevations_completed: Vec<u8>,
    /// Number of sweep blobs written to IDB.
    pub sweeps_stored: u32,
    /// Whether this was the final chunk in the volume.
    pub is_end: bool,
    /// Per-sweep metadata for all completed elevations so far.
    pub sweeps: Vec<crate::data::CachedSweep>,
    /// VCP pattern if extracted.
    pub vcp: Option<crate::data::keys::ExtractedVcp>,
    /// Total processing time in worker (ms).
    pub total_ms: f64,
    /// Elevation number currently being accumulated (partial sweep in progress).
    pub current_elevation: Option<u8>,
    /// Number of radials received so far for the current in-progress elevation.
    pub current_elevation_radials: Option<u32>,
    /// Last radial's azimuth angle in degrees (for sweep line extrapolation).
    pub last_radial_azimuth: Option<f32>,
    /// Timestamp of the last radial in Unix seconds (for sweep line extrapolation).
    pub last_radial_time_secs: Option<f64>,
    /// Volume header date/time in Unix seconds (authoritative scan start time).
    pub volume_header_time_secs: Option<f64>,
    /// Earliest radial collection time (Unix seconds) observed in this chunk.
    #[allow(dead_code)] // Consumed by debug UI in a later commit.
    pub chunk_min_time_secs: Option<f64>,
    /// Latest radial collection time (Unix seconds) observed in this chunk.
    /// Paired with the chunk's S3 upload time yields the per-chunk
    /// availability lag (AVAILABILITY − ACTUAL collection).
    pub chunk_max_time_secs: Option<f64>,
    /// Per-elevation time spans within this chunk:
    /// (elevation_number, start_secs, end_secs, radial_count).
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    /// Per-elevation azimuth ranges within this chunk:
    /// (elevation_number, first_azimuth, last_azimuth).
    pub chunk_elev_az_ranges: Vec<(u8, f32, f32)>,
}

/// Context for a render/decode request.
#[allow(dead_code)]
pub(crate) struct RenderContext {
    /// Typed identifier for the scan being rendered. The wire-protocol
    /// uses `to_storage_key()` once at dispatch.
    pub scan_key: crate::data::ScanKey,
    /// Elevation number being rendered.
    pub elevation_number: u8,
}

/// Decoded radar sweep data from the worker (raw data for GPU rendering).
pub(crate) struct DecodeResult {
    #[allow(dead_code)]
    pub context: RenderContext,
    /// Sorted azimuth angles in degrees.
    pub azimuths: Vec<f32>,
    /// Flat row-major raw gate values (azimuth_count * gate_count).
    /// Raw u8/u16 values cast to f32. Sentinels: 0=below threshold, 1=range folded.
    pub gate_values: Vec<f32>,
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub product: String,
    pub radial_count: u32,
    pub fetch_ms: f64,
    /// Sub-phase timing: deserialization.
    pub deser_ms: f64,
    /// Sub-phase timing: marshalling data for transfer.
    pub marshal_ms: f64,
    /// Total render time in worker (ms).
    pub total_ms: f64,
    /// Scale factor for decoding raw values: physical = (raw - offset) / scale.
    pub scale: f32,
    /// Offset for decoding raw values.
    pub offset: f32,
    /// Mean elevation angle across all radials in the sweep.
    pub mean_elevation: f32,
    /// Sweep start timestamp (Unix seconds).
    pub sweep_start_secs: f64,
    /// Sweep end timestamp (Unix seconds).
    pub sweep_end_secs: f64,
    /// Per-radial collection timestamps in Unix seconds (parallel to azimuths).
    pub radial_times: Vec<f64>,
    /// Median angular spacing between adjacent sorted radials, in degrees.
    /// Used by the shader's search threshold instead of deriving from azimuth_count.
    pub azimuth_spacing_deg: f32,
}

/// Per-sweep metadata for the volume ray marcher.
pub struct VolumeSweepMeta {
    pub elevation_deg: f32,
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_km: f32,
    pub gate_interval_km: f32,
    pub max_range_km: f32,
    pub data_offset: u32,
    pub scale: f32,
    pub offset: f32,
}

/// All-elevation packed volume data for ray-march rendering.
pub(crate) struct VolumeData {
    /// Packed raw gate values (all sweeps concatenated).
    /// Byte width per value is determined by `word_size`.
    pub buffer: Vec<u8>,
    /// Bytes per gate value: 1 (R8UI) when all sweeps are u8, 2 (R16UI) otherwise.
    pub word_size: u8,
    /// Per-sweep metadata sorted by elevation.
    pub sweeps: Vec<VolumeSweepMeta>,
    pub product: String,
    pub total_ms: f64,
}

/// Outcome of any worker operation.
pub(crate) enum WorkerOutcome {
    /// Archive ingest completed.
    Ingested(IngestResult),
    /// Per-chunk ingest completed (real-time streaming).
    ChunkIngested(ChunkIngestResult),
    /// Decode completed (raw data for GPU rendering).
    Decoded(DecodeResult),
    /// Live partial sweep decoded (from in-memory accumulator, not IDB).
    LiveDecoded(DecodeResult),
    /// Volume decode completed (all elevations packed for ray marching).
    VolumeDecoded(VolumeData),
    /// Error from any operation.
    WorkerError {
        id: u64,
        /// Structured category — callers should dispatch on this rather
        /// than parsing `message`. See [`WorkerErrorKind`].
        kind: WorkerErrorKind,
        message: String,
        /// Volume start timestamp (Unix seconds, sub-second precision) of
        /// the scan whose request failed, if the id could be correlated
        /// with a pending ingest or render. Lets callers clean up
        /// per-scan UI state (e.g. timeline ghosts) without guessing from
        /// global state.
        failed_scan_timestamp_secs: Option<f64>,
    },
}

/// Context for a volume render request.
#[allow(dead_code)]
pub(crate) struct VolumeRenderContext {
    pub scan_key: crate::data::ScanKey,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn response_type_strings_roundtrip() {
        for variant in [
            ResponseType::Ready,
            ResponseType::Ingested,
            ResponseType::ChunkIngested,
            ResponseType::Decoded,
            ResponseType::LiveDecoded,
            ResponseType::VolumeDecoded,
            ResponseType::Error,
        ] {
            assert_eq!(
                ResponseType::parse(variant.as_str()),
                Some(variant),
                "round-trip failed for {:?}",
                variant
            );
        }
    }

    #[wasm_bindgen_test]
    fn response_type_unknown_string() {
        assert_eq!(ResponseType::parse(""), None);
        assert_eq!(ResponseType::parse("not_a_real_type"), None);
        // Case-sensitive, the JS wire format is snake_case lowercase.
        assert_eq!(ResponseType::parse("Ready"), None);
    }

    #[wasm_bindgen_test]
    fn request_type_strings_are_snake_case() {
        // Pin the exact wire format that worker.js depends on. Any change
        // here must be reflected in worker.js's dispatch.
        assert_eq!(RequestType::Init.as_str(), "init");
        assert_eq!(RequestType::Ingest.as_str(), "ingest");
        assert_eq!(RequestType::IngestChunk.as_str(), "ingest_chunk");
        assert_eq!(RequestType::Render.as_str(), "render");
        assert_eq!(RequestType::RenderLive.as_str(), "render_live");
        assert_eq!(RequestType::RenderVolume.as_str(), "render_volume");
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // The existing `mod tests` pins `RequestType::as_str` exact strings and
    // round-trips `ResponseType` through `parse(as_str())`. A consistent typo
    // in BOTH `ResponseType::as_str` and `ResponseType::parse` would survive
    // that round-trip. Pin the exact `ResponseType::as_str` wire literals here
    // so the write direction is independently anchored against worker.js.
    #[wasm_bindgen_test]
    fn response_type_as_str_exact_wire_literals() {
        assert_eq!(ResponseType::Ready.as_str(), "ready");
        assert_eq!(ResponseType::Ingested.as_str(), "ingested");
        assert_eq!(ResponseType::ChunkIngested.as_str(), "chunk_ingested");
        assert_eq!(ResponseType::Decoded.as_str(), "decoded");
        assert_eq!(ResponseType::LiveDecoded.as_str(), "live_decoded");
        assert_eq!(ResponseType::VolumeDecoded.as_str(), "volume_decoded");
        assert_eq!(ResponseType::Error.as_str(), "error");
    }

    // The existing roundtrip test drives `parse` from `as_str`. Drive the
    // opposite direction with literals hand-written to match worker.js, so a
    // changed string in `parse` alone (without touching `as_str`) is caught.
    #[wasm_bindgen_test]
    fn response_type_parse_from_literal_wire_strings() {
        assert_eq!(ResponseType::parse("ready"), Some(ResponseType::Ready));
        assert_eq!(
            ResponseType::parse("ingested"),
            Some(ResponseType::Ingested)
        );
        assert_eq!(
            ResponseType::parse("chunk_ingested"),
            Some(ResponseType::ChunkIngested)
        );
        assert_eq!(ResponseType::parse("decoded"), Some(ResponseType::Decoded));
        assert_eq!(
            ResponseType::parse("live_decoded"),
            Some(ResponseType::LiveDecoded)
        );
        assert_eq!(
            ResponseType::parse("volume_decoded"),
            Some(ResponseType::VolumeDecoded)
        );
        assert_eq!(ResponseType::parse("error"), Some(ResponseType::Error));
    }

    // `parse` is whitespace- and substring-sensitive: nothing trims or does a
    // contains-match, so adornments must miss. Guards against an accidental
    // loosening of the exact-match dispatch.
    #[wasm_bindgen_test]
    fn response_type_parse_rejects_adorned_strings() {
        assert_eq!(ResponseType::parse(" ready"), None);
        assert_eq!(ResponseType::parse("ready "), None);
        assert_eq!(ResponseType::parse("ready\n"), None);
        assert_eq!(ResponseType::parse("READY"), None);
        // "decode" is a prefix of "decoded" but not an exact tag.
        assert_eq!(ResponseType::parse("decode"), None);
        // "render" is a REQUEST tag, never a valid response tag.
        assert_eq!(ResponseType::parse("render"), None);
        assert_eq!(ResponseType::parse("init"), None);
    }

    // The existing tests only exercise the DESERIALIZE direction of
    // WorkerErrorKind. Pin the SERIALIZE direction: each unit variant must
    // emit its `#[serde(rename_all = "snake_case")]` tag as a bare JS string.
    // Read it back as `String` via the same serde_wasm_bindgen idiom the
    // existing tests use, so no JS-object construction is needed.
    #[wasm_bindgen_test]
    fn worker_error_kind_serializes_to_snake_case_strings() {
        let cases = [
            (WorkerErrorKind::QuotaExceeded, "quota_exceeded"),
            (WorkerErrorKind::IdbFailure, "idb_failure"),
            (WorkerErrorKind::NotFound, "not_found"),
            (WorkerErrorKind::InvalidData, "invalid_data"),
            (WorkerErrorKind::InitFailed, "init_failed"),
            (WorkerErrorKind::Unknown, "unknown"),
        ];
        for (kind, expected) in cases {
            let v = serde_wasm_bindgen::to_value(&kind).unwrap();
            let s: String = serde_wasm_bindgen::from_value(v).unwrap();
            assert_eq!(s, expected, "serialize tag wrong for {:?}", kind);
        }
    }

    // A full enum->JS->enum round-trip for every variant. Distinct from the
    // existing string->enum test (which never starts from a Rust enum value)
    // and from the serialize test above (which inspects the intermediate
    // string). The `#[serde(other)] Unknown` variant round-trips cleanly
    // because it also carries the explicit "unknown" tag in the snake_case
    // rename table.
    #[wasm_bindgen_test]
    fn worker_error_kind_enum_value_roundtrip() {
        for kind in [
            WorkerErrorKind::QuotaExceeded,
            WorkerErrorKind::IdbFailure,
            WorkerErrorKind::NotFound,
            WorkerErrorKind::InvalidData,
            WorkerErrorKind::InitFailed,
            WorkerErrorKind::Unknown,
        ] {
            let v = serde_wasm_bindgen::to_value(&kind).unwrap();
            let back: WorkerErrorKind = serde_wasm_bindgen::from_value(v).unwrap();
            assert_eq!(back, kind, "enum round-trip failed for {:?}", kind);
        }
    }

    // The private `#[serde(default = ...)]` helper functions are the source of
    // truth for what a worker response gets when a field is absent. They are
    // pure and deterministic but otherwise only exercised implicitly. Pin
    // their exact values: these feed shader uniforms / product labels, so a
    // silent change would corrupt decode behavior for sparse messages.
    #[wasm_bindgen_test]
    fn serde_default_helpers_return_expected_values() {
        // Default product label used by both DecodedResultMsg and
        // VolumeDecodedResultMsg.
        assert_eq!(default_product(), "reflectivity");

        // Scale default of 1.0 makes `physical = (raw - offset) / scale` a
        // no-op divisor when the worker omits the field.
        assert!((default_scale() - 1.0).abs() < f32::EPSILON);

        // Azimuth spacing default is the canonical 1-degree super-resolution
        // step.
        assert!((default_azimuth_spacing_deg() - 1.0).abs() < f32::EPSILON);

        // Word size default of 2 bytes => R16UI texture path.
        assert_eq!(default_word_size(), 2u8);

        // Error defaults: human string + Unknown category.
        assert_eq!(default_error_message(), "Unknown worker error");
        assert_eq!(default_error_kind(), WorkerErrorKind::Unknown);
    }

    // Cross-check: no RequestType wire string collides with any ResponseType
    // wire string EXCEPT where intended (they share no literals). Worker.js
    // dispatches requests and responses on the same `type` key namespace; a
    // collision would route a response into the request handler. Computed from
    // the two as_str tables directly.
    #[wasm_bindgen_test]
    fn request_and_response_wire_strings_are_disjoint() {
        let requests = [
            RequestType::Init.as_str(),
            RequestType::Ingest.as_str(),
            RequestType::IngestChunk.as_str(),
            RequestType::Render.as_str(),
            RequestType::RenderLive.as_str(),
            RequestType::RenderVolume.as_str(),
        ];
        let responses = [
            ResponseType::Ready.as_str(),
            ResponseType::Ingested.as_str(),
            ResponseType::ChunkIngested.as_str(),
            ResponseType::Decoded.as_str(),
            ResponseType::LiveDecoded.as_str(),
            ResponseType::VolumeDecoded.as_str(),
            ResponseType::Error.as_str(),
        ];
        for req in requests {
            for resp in responses {
                assert!(req != resp, "request and response share wire tag {:?}", req);
            }
        }
        // And every response tag parses back to a response (sanity that the
        // disjoint-from-requests set is the same set `parse` accepts).
        for resp in responses {
            assert!(
                ResponseType::parse(resp).is_some(),
                "response tag {:?} not parseable",
                resp
            );
        }
    }
}
