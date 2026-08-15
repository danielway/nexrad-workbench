//! WASM exports for the Web Worker.
//!
//! These functions are called from worker.js to perform heavy data operations
//! (ingest, render) in a background thread, keeping the main UI responsive.

mod ingest;
mod render;
mod render_live;

use crate::core::volume_plan::{
    choose_bin_count, median_azimuth_spacing_deg, plan_azimuth_bins, plan_volume_sweeps,
    SweepCandidate, MAX_VOLUME_SWEEPS,
};
use crate::data::indexeddb::IndexedDbStore;
use crate::data::keys::*;
use crate::data::ExtractedVcp;
use crate::data::{parse_sweep_header, SweepHeader};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;

// ---------------------------------------------------------------------------
// Typed input param structs — deserialized from JS objects via serde-wasm-bindgen
// ---------------------------------------------------------------------------

/// Parameters for `worker_ingest`. The `data` ArrayBuffer is extracted separately.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IngestParams {
    pub site_id: String,
    pub timestamp_secs: f64,
    #[serde(default)]
    pub file_name: String,
    /// When non-empty, only these elevation numbers are extracted into sweep
    /// blobs and written to IDB; the rest of the decoded volume is dropped.
    /// Empty (the default) stores the whole volume. The VCP header is decoded
    /// and stored regardless of this filter. The full archive file is always
    /// downloaded — this scopes decode/storage, not the network transfer.
    #[serde(default)]
    pub wanted_elevations: Vec<u8>,
}

/// Parameters for `worker_render`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenderParams {
    pub scan_key: String,
    pub elevation_number: u8,
    #[serde(default = "default_product")]
    pub product: String,
}

/// Parameters for `worker_render_volume`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenderVolumeParams {
    pub scan_key: String,
    #[serde(default = "default_product")]
    pub product: String,
    pub elevation_numbers: Vec<u8>,
}

/// Parameters for `worker_ingest_chunk`. The `data` ArrayBuffer is extracted separately.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IngestChunkParams {
    pub site_id: String,
    pub timestamp_secs: f64,
    #[serde(default)]
    pub chunk_index: u32,
    #[serde(default)]
    pub source_sequence: u32,
    #[serde(default)]
    pub elevation_number: Option<u8>,
    #[serde(default)]
    pub chunk_index_in_sweep: Option<u8>,
    #[serde(default)]
    pub chunks_in_sweep: Option<u8>,
    #[serde(default)]
    pub is_start: bool,
    #[serde(default)]
    pub is_end: bool,
    #[serde(default)]
    pub file_name: String,
    /// True when the projection metadata indicates this is the last chunk in
    /// its sweep. Allows the worker to flush the sweep immediately rather than
    /// waiting for the next elevation's first chunk.
    #[serde(default)]
    pub is_last_in_sweep: bool,
}

pub(super) fn default_product() -> String {
    "reflectivity".to_string()
}

/// Throw a structured error from a worker_api function.
///
/// The returned `JsValue` is a JS object `{ kind, message }` so that
/// `worker.js`'s `classifyError` keeps the structured kind end-to-end
/// (instead of collapsing every Rust error to `unknown`). `kind` must be
/// one of the snake_case tags in `WorkerErrorKind` in
/// `src/nexrad/decode_worker/types.rs` — adding a new kind requires a
/// matching variant there.
pub(super) fn worker_error(kind: &str, message: impl AsRef<str>) -> JsValue {
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(&obj, &"kind".into(), &kind.into()).ok();
    js_sys::Reflect::set(&obj, &"message".into(), &message.as_ref().into()).ok();
    obj.into()
}

/// Extract the `data` ArrayBuffer field from a JS object as `Vec<u8>`.
pub(super) fn extract_data_bytes(obj: &JsValue) -> Result<Vec<u8>, JsValue> {
    let val = js_sys::Reflect::get(obj, &"data".into())
        .map_err(|e| JsValue::from_str(&format!("Missing data: {:?}", e)))?;
    Ok(js_sys::Uint8Array::new(&val).to_vec())
}

/// Attach a typed-array field to a JS response object.
///
/// Scalar response fields go through serde; typed-array fields (`Float32Array`,
/// `Uint8Array`, …) are attached separately for zero-copy transfer. Callers
/// should pass the already-constructed typed-array buffer.
pub(super) fn attach_buffer_field(obj: &JsValue, field: &str, buffer: &JsValue) {
    js_sys::Reflect::set(obj, &field.into(), buffer).ok();
}

// ---------------------------------------------------------------------------
// Typed response structs — serialized to JS objects via serde-wasm-bindgen
// ---------------------------------------------------------------------------

/// Response from `worker_ingest`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IngestResponse<'a> {
    pub records_stored: u32,
    pub scan_key: String,
    pub elevation_numbers: &'a [u8],
    pub total_ms: f64,
    pub split_ms: f64,
    pub decompress_ms: f64,
    pub decode_ms: f64,
    pub extract_ms: f64,
    pub store_ms: f64,
    pub index_ms: f64,
    pub sweeps: &'a [CachedSweep],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcp: Option<&'a ExtractedVcp>,
}

/// Scalar fields of the response from `worker_render`.
/// ArrayBuffer fields (azimuths, gateValues, radialTimes) are set separately.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RenderResponse {
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_range_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub product: String,
    pub radial_count: u32,
    pub scale: f64,
    pub offset: f64,
    pub mean_elevation: f64,
    pub sweep_start_secs: f64,
    pub sweep_end_secs: f64,
    pub fetch_ms: f64,
    pub deser_ms: f64,
    pub total_ms: f64,
    pub marshal_ms: f64,
    /// Median angular spacing between adjacent sorted radials, in degrees.
    /// Used by the shader's search threshold instead of deriving from azimuth_count.
    pub azimuth_spacing_deg: f32,
}

/// Response from `worker_ingest_chunk`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ChunkIngestResponse {
    pub chunk_index: u32,
    pub radials_decoded: u32,
    pub sweeps_stored: u32,
    pub scan_key: String,
    pub is_end: bool,
    pub total_ms: f64,
    pub sweeps: Vec<CachedSweep>,
    pub elevations_completed: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcp: Option<ExtractedVcp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_min_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_max_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chunk_elev_az_ranges: Vec<(u8, f32, f32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_header_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_radial_azimuth: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_radial_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_elevation: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_elevation_radials: Option<u32>,
}

/// Per-sweep metadata in the volume render response.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VolumeRenderSweepMeta {
    pub elevation_deg: f64,
    pub azimuth_count: u32,
    pub gate_count: u32,
    pub first_gate_km: f64,
    pub gate_interval_km: f64,
    pub max_range_km: f64,
    pub data_offset: u32,
    pub scale: f64,
    pub offset: f64,
}

/// Scalar fields of the volume render response.
/// The `buffer` ArrayBuffer is set separately for zero-copy transfer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VolumeRenderResponse {
    pub sweep_count: u32,
    pub word_size: u8,
    pub sweep_meta: Vec<VolumeRenderSweepMeta>,
    pub product: String,
    pub total_ms: f64,
}

// ---------------------------------------------------------------------------
// Worker-side cached IDB connection
// ---------------------------------------------------------------------------
// WASM is single-threaded so thread_local! is safe. We keep a single
// IndexedDbStore alive for the lifetime of the worker so that
// subsequent ingest/render calls reuse the already-open IDB connection
// instead of paying the ~60ms open+list overhead every time.

thread_local! {
    pub(super) static WORKER_IDB: IndexedDbStore = IndexedDbStore::new();
    static WORKER_LOGGER_INIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Initialize the log crate in the worker context (once).
pub(super) fn init_logger() {
    WORKER_LOGGER_INIT.with(|init| {
        if !init.get() {
            eframe::WebLogger::init(log::LevelFilter::Debug).ok();
            init.set(true);
        }
    });
}

/// Get (or lazily open) the shared worker IDB store.
///
/// The store itself is eagerly constructed; `open()` is a no-op after the
/// first successful call, and concurrent callers racing the first open
/// coalesce inside `IndexedDbStore::open`.
pub(super) async fn idb_store() -> Result<IndexedDbStore, wasm_bindgen::JsValue> {
    let store = WORKER_IDB.with(|s| s.clone());
    store
        .open()
        .await
        .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Failed to open IDB: {}", e)))?;
    Ok(store)
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn default_product_is_reflectivity() {
        assert_eq!(default_product(), "reflectivity");
    }

    #[wasm_bindgen_test]
    fn ingest_params_full_camel_case() {
        let json = r#"{
            "siteId": "KTLX",
            "timestampSecs": 1234.5,
            "fileName": "KTLX20230101_000000_V06",
            "wantedElevations": [1, 2, 3]
        }"#;
        let p: IngestParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(p.site_id, "KTLX");
        assert!((p.timestamp_secs - 1234.5).abs() < 1e-9);
        assert_eq!(p.file_name, "KTLX20230101_000000_V06");
        assert_eq!(p.wanted_elevations, vec![1u8, 2, 3]);
    }

    #[wasm_bindgen_test]
    fn ingest_params_defaults_when_optional_omitted() {
        // Only the required fields present; file_name and wanted_elevations default.
        let json = r#"{ "siteId": "KOUN", "timestampSecs": 0.0 }"#;
        let p: IngestParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(p.site_id, "KOUN");
        assert!(p.timestamp_secs.abs() < 1e-9);
        assert_eq!(p.file_name, "");
        assert!(p.wanted_elevations.is_empty());
    }

    #[wasm_bindgen_test]
    fn render_params_product_defaults_to_reflectivity() {
        let json = r#"{ "scanKey": "KTLX|1700000000000", "elevationNumber": 2 }"#;
        let p: RenderParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(p.scan_key, "KTLX|1700000000000");
        assert_eq!(p.elevation_number, 2u8);
        // default = default_product()
        assert_eq!(p.product, "reflectivity");
    }

    #[wasm_bindgen_test]
    fn render_params_explicit_product_overrides_default() {
        let json = r#"{ "scanKey": "K|1", "elevationNumber": 5, "product": "velocity" }"#;
        let p: RenderParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(p.elevation_number, 5u8);
        assert_eq!(p.product, "velocity");
    }

    #[wasm_bindgen_test]
    fn render_volume_params_parse() {
        let json = r#"{ "scanKey": "K|9", "elevationNumbers": [1, 4, 7] }"#;
        let p: RenderVolumeParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(p.scan_key, "K|9");
        assert_eq!(p.elevation_numbers, vec![1u8, 4, 7]);
        // product defaults
        assert_eq!(p.product, "reflectivity");
    }

    #[wasm_bindgen_test]
    fn render_volume_params_explicit_product() {
        let json = r#"{ "scanKey": "K|9", "elevationNumbers": [], "product": "spectrum_width" }"#;
        let p: RenderVolumeParams = serde_json::from_str(json).expect("deserialize");
        assert!(p.elevation_numbers.is_empty());
        assert_eq!(p.product, "spectrum_width");
    }

    #[wasm_bindgen_test]
    fn ingest_chunk_params_defaults() {
        // Only the required fields present; all optional booleans/index default.
        let json = r#"{ "siteId": "KTLX", "timestampSecs": 100.0 }"#;
        let p: IngestChunkParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(p.site_id, "KTLX");
        assert!((p.timestamp_secs - 100.0).abs() < 1e-9);
        assert_eq!(p.chunk_index, 0u32);
        assert!(!p.is_start);
        assert!(!p.is_end);
        assert_eq!(p.file_name, "");
        assert!(!p.is_last_in_sweep);
    }

    #[wasm_bindgen_test]
    fn ingest_chunk_params_full() {
        let json = r#"{
            "siteId": "KOUN",
            "timestampSecs": 200.25,
            "chunkIndex": 7,
            "isStart": true,
            "isEnd": true,
            "fileName": "chunk7",
            "isLastInSweep": true
        }"#;
        let p: IngestChunkParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(p.site_id, "KOUN");
        assert!((p.timestamp_secs - 200.25).abs() < 1e-9);
        assert_eq!(p.chunk_index, 7u32);
        assert!(p.is_start);
        assert!(p.is_end);
        assert_eq!(p.file_name, "chunk7");
        assert!(p.is_last_in_sweep);
    }

    #[wasm_bindgen_test]
    fn ingest_chunk_params_partial_flags() {
        // is_start true, is_end omitted (defaults false).
        let json = r#"{
            "siteId": "K",
            "timestampSecs": 1.0,
            "chunkIndex": 3,
            "isStart": true
        }"#;
        let p: IngestChunkParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(p.chunk_index, 3u32);
        assert!(p.is_start);
        assert!(!p.is_end);
        assert!(!p.is_last_in_sweep);
    }

    #[wasm_bindgen_test]
    fn ingest_params_missing_required_field_errors() {
        // siteId is required (no serde default) → deserialization must fail.
        let json = r#"{ "timestampSecs": 5.0 }"#;
        let res: Result<IngestParams, _> = serde_json::from_str(json);
        assert!(res.is_err());
    }
}
