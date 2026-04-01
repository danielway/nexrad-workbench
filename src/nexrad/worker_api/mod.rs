//! WASM exports for the Web Worker.
//!
//! These functions are called from worker.js to perform heavy data operations
//! (ingest, render) in a background thread, keeping the main UI responsive.

mod ingest;
mod render;
mod render_live;

use crate::data::indexeddb::IndexedDbRecordStore;
use crate::data::keys::*;
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
    pub is_start: bool,
    #[serde(default)]
    pub is_end: bool,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub skip_overlap_delete: bool,
}

pub(super) fn default_product() -> String {
    "reflectivity".to_string()
}

/// Extract the `data` ArrayBuffer field from a JS object as `Vec<u8>`.
pub(super) fn extract_data_bytes(obj: &JsValue) -> Result<Vec<u8>, JsValue> {
    let val = js_sys::Reflect::get(obj, &"data".into())
        .map_err(|e| JsValue::from_str(&format!("Missing data: {:?}", e)))?;
    Ok(js_sys::Uint8Array::new(&val).to_vec())
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
    pub sweeps: &'a [SweepMeta],
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
    pub sweeps: Vec<SweepMeta>,
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
// IndexedDbRecordStore alive for the lifetime of the worker so that
// subsequent ingest/render calls reuse the already-open IDB connection
// instead of paying the ~60ms open+list overhead every time.

thread_local! {
    pub(super) static WORKER_IDB: std::cell::RefCell<Option<IndexedDbRecordStore>> =
        const { std::cell::RefCell::new(None) };
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
pub(super) async fn idb_store() -> Result<IndexedDbRecordStore, wasm_bindgen::JsValue> {
    let existing = WORKER_IDB.with(|cell| cell.borrow().clone());
    if let Some(store) = existing {
        return Ok(store);
    }
    let store = IndexedDbRecordStore::new();
    store
        .open()
        .await
        .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Failed to open IDB: {}", e)))?;
    WORKER_IDB.with(|cell| {
        *cell.borrow_mut() = Some(store.clone());
    });
    Ok(store)
}
