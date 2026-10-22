//! Type definitions for worker message payloads and public result types.

use crate::core::{ChunkIngestResult, DecodeResult, IngestResult, VolumeData, WorkerErrorKind};
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
    Detect,
}

impl RequestType {
    /// Every variant, for the protocol-drift tests below.
    ///
    /// Kept complete by [`Self::exhaustiveness_guard`]: adding a variant without
    /// adding it here fails to compile.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Init,
        Self::Ingest,
        Self::IngestChunk,
        Self::Render,
        Self::RenderLive,
        Self::RenderVolume,
        Self::Detect,
    ];

    /// Compile-time completeness check for [`Self::ALL`]: this match must list
    /// every variant, and `ALL.len()` must equal the number of arms.
    #[cfg(test)]
    #[allow(dead_code)] // Exists for its exhaustive match, never called.
    const fn exhaustiveness_guard(self) -> usize {
        match self {
            Self::Init => 0,
            Self::Ingest => 1,
            Self::IngestChunk => 2,
            Self::Render => 3,
            Self::RenderLive => 4,
            Self::RenderVolume => 5,
            Self::Detect => 6,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Ingest => "ingest",
            Self::IngestChunk => "ingest_chunk",
            Self::Render => "render",
            Self::RenderLive => "render_live",
            Self::RenderVolume => "render_volume",
            Self::Detect => "detect",
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
    /// A `render_live` request whose elevation was stored before it ran.
    LiveSuperseded,
    VolumeDecoded,
    /// A `detect` request's storm cells.
    Detected,
    Error,
}

impl ResponseType {
    /// Every variant, for the protocol-drift tests below. Kept complete by
    /// [`Self::exhaustiveness_guard`].
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Ready,
        Self::Ingested,
        Self::ChunkIngested,
        Self::Decoded,
        Self::LiveDecoded,
        Self::LiveSuperseded,
        Self::VolumeDecoded,
        Self::Detected,
        Self::Error,
    ];

    /// Compile-time completeness check for [`Self::ALL`].
    #[cfg(test)]
    #[allow(dead_code)] // Exists for its exhaustive match, never called.
    const fn exhaustiveness_guard(self) -> usize {
        match self {
            Self::Ready => 0,
            Self::Ingested => 1,
            Self::ChunkIngested => 2,
            Self::Decoded => 3,
            Self::LiveDecoded => 4,
            Self::LiveSuperseded => 5,
            Self::VolumeDecoded => 6,
            Self::Detected => 7,
            Self::Error => 8,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Ingested => "ingested",
            Self::ChunkIngested => "chunk_ingested",
            Self::Decoded => "decoded",
            Self::LiveDecoded => "live_decoded",
            Self::LiveSuperseded => "live_superseded",
            Self::VolumeDecoded => "volume_decoded",
            Self::Detected => "detected",
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
            "live_superseded" => Self::LiveSuperseded,
            "volume_decoded" => Self::VolumeDecoded,
            "detected" => Self::Detected,
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
    pub vcp: Option<crate::data::ExtractedVcp>,
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
    pub vcp: Option<crate::data::ExtractedVcp>,
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

/// Storm cell detection result from the worker.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DetectedResultMsg {
    pub result: crate::core::StormCellsResult,
    #[serde(default)]
    pub fetch_ms: f64,
    #[serde(default)]
    pub detect_ms: f64,
    #[serde(default)]
    pub total_ms: f64,
    #[serde(default)]
    pub azimuth_count: u32,
    #[serde(default)]
    pub gate_count: u32,
    #[serde(default)]
    pub complete: bool,
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
    pub source_sequence: u32,
    pub elevation_number: Option<u8>,
    pub chunk_index_in_sweep: Option<u8>,
    pub chunks_in_sweep: Option<u8>,
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

/// Request message sent to the worker for storm cell detection.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DetectRequestMsg<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub id: f64,
    pub scan_key: &'a str,
    pub elevation_number: u8,
    /// `"stored"` or `"live"`.
    pub input: &'a str,
    pub params: &'a crate::storm_cells::DetectionParams,
    pub radar_lat: f64,
    pub radar_lon: f64,
}

/// Request message sent to the worker for live render operations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenderLiveRequestMsg<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub id: f64,
    pub site_id: &'a str,
    pub elevation_number: u8,
    pub product: &'a str,
}

// ---------------------------------------------------------------------------
// Public result/context types
// ---------------------------------------------------------------------------

/// Unique ID for tracking worker requests.
pub(super) type RequestId = u64;

// The pure outcome vocabulary — `IngestContext`/`IngestResult`,
// `ChunkIngestContext`/`ChunkIngestResult`, `RenderContext`/`DecodeResult`,
// `VolumeSweepMeta`/`VolumeData`, — lives in `crate::core` (domain::worker)
// so core reducers can consume worker outcomes without importing nexrad.
// Only the wire-protocol machinery stays here.

/// Outcome of any worker operation.
pub(crate) enum WorkerOutcome {
    /// Archive ingest completed.
    Ingested(IngestResult),
    /// Per-chunk ingest completed (real-time streaming).
    ChunkIngested(ChunkIngestResult),
    /// Decode completed (raw data for GPU rendering).
    Decoded(DecodeResult),
    /// Live partial sweep decoded (from in-memory accumulator, not IDB),
    /// for the streaming site that asked.
    LiveDecoded {
        site: crate::data::SiteId,
        result: DecodeResult,
    },
    /// Volume decode completed (all elevations packed for ray marching).
    VolumeDecoded(VolumeData),
    /// Storm cell detection completed.
    Detected(crate::core::storm_cells::DetectReply),
    /// Storm cell detection failed in the worker (or its reply did not
    /// parse). Carries the core's request id, not the wire id.
    DetectFailed { request_id: u64, message: String },
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

/// Context for a live (partial sweep) render request: the streaming site
/// whose accumulator it reads, beside the render context. The render
/// context keeps the sentinel scan key (an empty site) that marks a live
/// partial on the canvas, so the site travels here instead.
pub(crate) struct LiveRenderContext {
    pub site: crate::data::SiteId,
    pub render: crate::core::RenderContext,
}

/// Context for a volume render request.
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
            ResponseType::LiveSuperseded,
            ResponseType::VolumeDecoded,
            ResponseType::Detected,
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
        assert_eq!(RequestType::Detect.as_str(), "detect");
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
        assert_eq!(ResponseType::LiveSuperseded.as_str(), "live_superseded");
        assert_eq!(ResponseType::VolumeDecoded.as_str(), "volume_decoded");
        assert_eq!(ResponseType::Detected.as_str(), "detected");
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
            ResponseType::parse("live_superseded"),
            Some(ResponseType::LiveSuperseded)
        );
        assert_eq!(
            ResponseType::parse("volume_decoded"),
            Some(ResponseType::VolumeDecoded)
        );
        assert_eq!(
            ResponseType::parse("detected"),
            Some(ResponseType::Detected)
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
            RequestType::Detect.as_str(),
        ];
        let responses = [
            ResponseType::Ready.as_str(),
            ResponseType::Ingested.as_str(),
            ResponseType::ChunkIngested.as_str(),
            ResponseType::Decoded.as_str(),
            ResponseType::LiveDecoded.as_str(),
            ResponseType::LiveSuperseded.as_str(),
            ResponseType::VolumeDecoded.as_str(),
            ResponseType::Detected.as_str(),
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

#[cfg(test)]
mod detect_wire_tests {
    use super::*;
    use crate::core::{StormCell, StormCellsResult};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// The `detected` message as `worker.js` posts it: the worker's
    /// serialized response plus `type` and `id`.
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Wire<'a> {
        #[serde(rename = "type")]
        msg_type: &'a str,
        id: f64,
        result: &'a StormCellsResult,
        fetch_ms: f64,
        detect_ms: f64,
        total_ms: f64,
        azimuth_count: u32,
        gate_count: u32,
        complete: bool,
    }

    #[wasm_bindgen_test]
    fn detected_result_survives_the_js_boundary() {
        let mut result = StormCellsResult {
            cells: vec![StormCell {
                id: 0,
                group_id: Some(0),
                level_dbz: 50.0,
                peak_dbz: 62.5,
                mean_dbz: 51.0,
                area_km2: 182.0,
                gate_count: 900,
                lat: 41.3,
                lon: -94.5,
                bounds: (41.2, -94.6, 41.4, -94.4),
                bearing_from_radar_deg: 239.0,
                range_from_radar_km: 98.0,
                orientation_deg: 45.0,
                elongation: 1.8,
                outline: vec![(41.2, -94.6), (41.4, -94.6), (41.3, -94.4), (41.2, -94.6)],
                core_outline: vec![
                    (41.25, -94.55),
                    (41.35, -94.55),
                    (41.3, -94.45),
                    (41.25, -94.55),
                ],
            }],
            ..StormCellsResult::default()
        };
        result.source.sweep_id = Some("KDMX|1716322647000|1|reflectivity".into());
        result.source.elevation_deg = Some(0.48);
        result.source.params.threshold_dbz = 40.0;
        let wire = Wire {
            msg_type: ResponseType::Detected.as_str(),
            id: 7.0,
            result: &result,
            fetch_ms: 9.5,
            detect_ms: 84.0,
            total_ms: 95.0,
            azimuth_count: 720,
            gate_count: 1832,
            complete: false,
        };
        let js = serde_wasm_bindgen::to_value(&wire).unwrap();
        let header: ResponseHeader = serde_wasm_bindgen::from_value(js.clone()).unwrap();
        assert_eq!(
            ResponseType::parse(&header.msg_type),
            Some(ResponseType::Detected)
        );
        assert_eq!(header.id, Some(7));
        let back: DetectedResultMsg = serde_wasm_bindgen::from_value(js).unwrap();
        assert_eq!(back.result, result);
        assert_eq!(back.detect_ms, 84.0);
        assert_eq!((back.azimuth_count, back.gate_count), (720, 1832));
        assert!(!back.complete);
    }
}

/// Guards the Rust ⇄ `worker.js` protocol against silent drift.
///
/// The message vocabulary is defined twice — once as the enums above, once as
/// string literals in `worker.js` — and nothing but these tests connects them.
/// Before they existed, renaming a tag on the JS side compiled fine, passed the
/// whole suite, and broke only at runtime in the browser.
///
/// `worker.js` is embedded with [`include_str!`], so the check runs in the
/// ordinary headless suite with no node harness, and a stale path fails the
/// build rather than skipping silently.
#[cfg(test)]
mod protocol_drift_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const WORKER_JS_RAW: &str = include_str!("../../../../worker.js");

    /// `worker.js` with `//` line comments removed. The file's header documents
    /// the whole protocol in prose (`{ type: 'init', ... }`), which the scans
    /// below would otherwise read as real dispatch and emission sites.
    fn worker_js() -> String {
        WORKER_JS_RAW
            .lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every `msg.type === '...'` dispatch guard in `worker.js`.
    fn js_dispatched_requests() -> Vec<String> {
        tags_after(&worker_js(), "msg.type === '")
    }

    /// Every `type: '...'` message-construction site in `worker.js`.
    fn js_emitted_responses() -> Vec<String> {
        tags_after(&worker_js(), "type: '")
    }

    /// Collect every single-quoted string that immediately follows `marker`.
    fn tags_after(src: &str, marker: &str) -> Vec<String> {
        src.match_indices(marker)
            .filter_map(|(i, m)| {
                let rest = &src[i + m.len()..];
                rest.find('\'').map(|end| rest[..end].to_string())
            })
            .collect()
    }

    #[wasm_bindgen_test]
    fn worker_js_dispatches_every_request_type() {
        let dispatched = js_dispatched_requests();
        for req in RequestType::ALL {
            assert!(
                dispatched.iter().any(|d| d == req.as_str()),
                "worker.js has no `msg.type === '{}'` branch — the main thread \
                 sends a request the worker will silently ignore",
                req.as_str()
            );
        }
    }

    #[wasm_bindgen_test]
    fn worker_js_dispatches_nothing_unknown() {
        for tag in js_dispatched_requests() {
            assert!(
                RequestType::ALL.iter().any(|r| r.as_str() == tag),
                "worker.js dispatches on '{tag}', which no RequestType sends — \
                 a stale handler or a typo"
            );
        }
    }

    #[wasm_bindgen_test]
    fn worker_js_emits_every_response_type() {
        let emitted = js_emitted_responses();
        for resp in ResponseType::ALL {
            assert!(
                emitted.iter().any(|e| e == resp.as_str()),
                "worker.js never emits `type: '{}'` — the main thread parses a \
                 response the worker never sends",
                resp.as_str()
            );
        }
    }

    #[wasm_bindgen_test]
    fn worker_js_emits_nothing_unparseable() {
        for tag in js_emitted_responses() {
            assert!(
                ResponseType::parse(&tag).is_some(),
                "worker.js emits `type: '{tag}'`, which ResponseType::parse \
                 rejects — the main thread would drop the message"
            );
        }
    }

    #[wasm_bindgen_test]
    fn the_embedded_worker_js_is_the_real_one() {
        // Guards the include_str! path: if it ever resolved to something else,
        // the assertions above would vacuously pass on an empty haystack.
        assert!(
            WORKER_JS_RAW.contains("onmessage"),
            "embedded worker.js does not look like the worker entry point"
        );
        assert!(
            WORKER_JS_RAW.len() > 1000,
            "embedded worker.js is suspiciously small"
        );
    }
}
