//! Type definitions for worker message payloads and public result types.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Internal message types (serde-wasm-bindgen deserialization)
// ---------------------------------------------------------------------------

/// Envelope for all worker response messages (type + id).
#[derive(Deserialize)]
pub(super) struct MessageEnvelope {
    pub id: u64,
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
    pub sweeps: Vec<crate::data::SweepMeta>,
    #[serde(default)]
    pub vcp: Option<crate::data::keys::ExtractedVcp>,
}

/// Chunk ingest result payload from the worker.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChunkIngestResultMsg {
    pub scan_key: String,
    #[serde(default)]
    pub sweeps_stored: u32,
    #[serde(default)]
    pub is_end: bool,
    #[serde(default)]
    pub total_ms: f64,
    #[serde(default)]
    pub elevations_completed: Vec<u8>,
    #[serde(default)]
    pub sweeps: Vec<crate::data::SweepMeta>,
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
#[derive(Deserialize)]
pub(super) struct ErrorMsg {
    pub id: u64,
    #[serde(default = "default_error_message")]
    pub message: String,
}

fn default_error_message() -> String {
    "Unknown worker error".to_string()
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
    pub skip_overlap_delete: bool,
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
pub struct IngestContext {
    pub timestamp_secs: i64,
    pub file_name: String,
    pub fetch_latency_ms: f64,
}

/// Successful ingest result from the worker.
pub struct IngestResult {
    pub context: IngestContext,
    /// Scan storage key (e.g., "KDMX|1700000000000")
    pub scan_key: String,
    /// Number of records stored in IDB.
    pub records_stored: u32,
    /// Unique elevation numbers found across all records.
    pub elevation_numbers: Vec<u8>,
    /// Per-sweep metadata extracted from radials during ingest.
    pub sweeps: Vec<crate::data::SweepMeta>,
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
pub struct ChunkIngestContext {
    pub site_id: String,
    pub timestamp_secs: i64,
    pub chunk_index: u32,
    pub is_end: bool,
}

/// Successful per-chunk ingest result from the worker.
pub struct ChunkIngestResult {
    pub context: ChunkIngestContext,
    /// Scan storage key (e.g., "KDMX|1700000000000")
    pub scan_key: String,
    /// Elevation numbers that became complete with this chunk.
    pub elevations_completed: Vec<u8>,
    /// Number of sweep blobs written to IDB.
    pub sweeps_stored: u32,
    /// Whether this was the final chunk in the volume.
    pub is_end: bool,
    /// Per-sweep metadata for all completed elevations so far.
    pub sweeps: Vec<crate::data::SweepMeta>,
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
    /// Per-elevation time spans within this chunk:
    /// (elevation_number, start_secs, end_secs, radial_count).
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    /// Per-elevation azimuth ranges within this chunk:
    /// (elevation_number, first_azimuth, last_azimuth).
    pub chunk_elev_az_ranges: Vec<(u8, f32, f32)>,
}

/// Context for a render/decode request.
#[allow(dead_code)]
pub struct RenderContext {
    /// Scan storage key.
    pub scan_key: String,
    /// Elevation number being rendered.
    pub elevation_number: u8,
}

/// Decoded radar sweep data from the worker (raw data for GPU rendering).
pub struct DecodeResult {
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
pub struct VolumeData {
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
pub enum WorkerOutcome {
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
    WorkerError { id: u64, message: String },
}

/// Context for a volume render request.
#[allow(dead_code)]
pub struct VolumeRenderContext {
    pub scan_key: String,
}
