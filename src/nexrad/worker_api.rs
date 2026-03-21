//! WASM exports for the Web Worker.
//!
//! These functions are called from worker.js to perform heavy data operations
//! (ingest, render) in a background thread, keeping the main UI responsive.

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
struct IngestParams {
    site_id: String,
    timestamp_secs: f64,
    #[serde(default)]
    file_name: String,
}

/// Parameters for `worker_render`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderParams {
    scan_key: String,
    elevation_number: u8,
    #[serde(default = "default_product")]
    product: String,
}

/// Parameters for `worker_render_volume`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderVolumeParams {
    scan_key: String,
    #[serde(default = "default_product")]
    product: String,
    elevation_numbers: Vec<u8>,
}

/// Parameters for `worker_ingest_chunk`. The `data` ArrayBuffer is extracted separately.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IngestChunkParams {
    site_id: String,
    timestamp_secs: f64,
    #[serde(default)]
    chunk_index: u32,
    #[serde(default)]
    is_start: bool,
    #[serde(default)]
    is_end: bool,
    #[serde(default)]
    file_name: String,
}

fn default_product() -> String {
    "reflectivity".to_string()
}

/// Extract the `data` ArrayBuffer field from a JS object as `Vec<u8>`.
fn extract_data_bytes(obj: &JsValue) -> Result<Vec<u8>, JsValue> {
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
struct IngestResponse<'a> {
    records_stored: u32,
    scan_key: String,
    elevation_numbers: &'a [u8],
    total_ms: f64,
    split_ms: f64,
    decompress_ms: f64,
    decode_ms: f64,
    extract_ms: f64,
    store_ms: f64,
    index_ms: f64,
    sweeps: &'a [SweepMeta],
    #[serde(skip_serializing_if = "Option::is_none")]
    vcp: Option<&'a ExtractedVcp>,
}

/// Scalar fields of the response from `worker_render`.
/// ArrayBuffer fields (azimuths, gateValues, radialTimes) are set separately.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderResponse {
    azimuth_count: u32,
    gate_count: u32,
    first_gate_range_km: f64,
    gate_interval_km: f64,
    max_range_km: f64,
    product: String,
    radial_count: u32,
    scale: f64,
    offset: f64,
    mean_elevation: f64,
    sweep_start_secs: f64,
    sweep_end_secs: f64,
    fetch_ms: f64,
    deser_ms: f64,
    total_ms: f64,
    marshal_ms: f64,
}

/// Response from `worker_ingest_chunk`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChunkIngestResponse {
    chunk_index: u32,
    radials_decoded: u32,
    sweeps_stored: u32,
    scan_key: String,
    is_end: bool,
    total_ms: f64,
    sweeps: Vec<SweepMeta>,
    elevations_completed: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vcp: Option<ExtractedVcp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_min_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_max_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    chunk_elev_az_ranges: Vec<(u8, f32, f32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume_header_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_radial_azimuth: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_radial_time_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_elevation: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_elevation_radials: Option<u32>,
}

/// Per-sweep metadata in the volume render response.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VolumeRenderSweepMeta {
    elevation_deg: f64,
    azimuth_count: u32,
    gate_count: u32,
    first_gate_km: f64,
    gate_interval_km: f64,
    max_range_km: f64,
    data_offset: u32,
    scale: f64,
    offset: f64,
}

/// Scalar fields of the volume render response.
/// The `buffer` ArrayBuffer is set separately for zero-copy transfer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VolumeRenderResponse {
    sweep_count: u32,
    word_size: u8,
    sweep_meta: Vec<VolumeRenderSweepMeta>,
    product: String,
    total_ms: f64,
}

// ---------------------------------------------------------------------------
// Worker-side cached IDB connection
// ---------------------------------------------------------------------------
// WASM is single-threaded so thread_local! is safe. We keep a single
// IndexedDbRecordStore alive for the lifetime of the worker so that
// subsequent ingest/render calls reuse the already-open IDB connection
// instead of paying the ~60ms open+list overhead every time.

thread_local! {
    static WORKER_IDB: std::cell::RefCell<Option<IndexedDbRecordStore>> =
        const { std::cell::RefCell::new(None) };
    static WORKER_LOGGER_INIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Initialize the log crate in the worker context (once).
fn init_logger() {
    WORKER_LOGGER_INIT.with(|init| {
        if !init.get() {
            eframe::WebLogger::init(log::LevelFilter::Debug).ok();
            init.set(true);
        }
    });
}

/// Get (or lazily open) the shared worker IDB store.
async fn idb_store() -> Result<IndexedDbRecordStore, wasm_bindgen::JsValue> {
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

/// Ingest a raw NEXRAD archive file: split into LDM records, probe for elevation
/// metadata, store in IndexedDB, and return metadata.
///
/// Called from the Web Worker via worker.js.
///
/// Parameters (JS object): `{ data: ArrayBuffer, siteId: string, timestampSecs: number, fileName: string }`
/// Returns (JS object): `{ recordsStored, scanKey, elevationMap, totalMs, sweepsJson, vcpJson? }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_ingest(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();

        // --- Extract parameters from JS ---
        let data = extract_data_bytes(&params)?;
        let p: IngestParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid ingest params: {}", e)))?;
        let site_id = p.site_id;
        let timestamp_secs = p.timestamp_secs as i64;
        let file_name = p.file_name;

        log::info!(
            "ingest: received {} ({:.1}MB)",
            file_name,
            data.len() as f64 / (1024.0 * 1024.0),
        );

        // --- Phase 0: Split into LDM records ---
        let t_split = web_time::Instant::now();
        let file = nexrad_data::volume::File::new(data);
        let records = file.records().map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to split archive: {}", e))
        })?;
        let split_ms = t_split.elapsed().as_secs_f64() * 1000.0;

        if records.is_empty() {
            return Err(wasm_bindgen::JsValue::from_str("No records found"));
        }

        log::info!(
            "ingest: split into {} records in {:.1}ms",
            records.len(),
            split_ms,
        );

        let store = idb_store().await?;
        let scan_key = ScanKey::new(site_id.as_str(), UnixMillis::from_secs(timestamp_secs));

        // --- Phase 1: Decompress + decode all records into radials ---
        let t_decode = web_time::Instant::now();
        let decoded = crate::nexrad::ingest_phases::decompress_and_decode_records(&records)?;
        let all_radials = decoded.all_radials;
        let radial_metas = decoded.radial_metas;
        let decompress_ms_total = decoded.decompress_ms;
        let decode_only_ms = decoded.decode_ms;
        let compressed_count = decoded.compressed_count;
        let extracted_vcp = decoded.extracted_vcp;
        let has_vcp = decoded.has_vcp;
        let phase1_ms = t_decode.elapsed().as_secs_f64() * 1000.0;

        let sweeps = crate::nexrad::ingest_phases::build_sweep_meta(&radial_metas);
        let elevation_numbers: Vec<u8> = sweeps.iter().map(|s| s.elevation_number).collect();
        let end_timestamp_secs = sweeps
            .iter()
            .map(|s| s.end as i64)
            .max()
            .unwrap_or(timestamp_secs);

        log::info!(
            "ingest: decompressed {} records, decoded {} radials across {} elevations in {:.1}ms (decompress: {:.1}ms, decode: {:.1}ms)",
            compressed_count,
            all_radials.len(),
            elevation_numbers.len(),
            phase1_ms,
            decompress_ms_total,
            decode_only_ms,
        );

        // --- Phase 2: Extract sweep data for all (elevation, product) pairs ---
        let t_extract = web_time::Instant::now();
        let by_elevation = crate::nexrad::ingest_phases::group_radials_by_elevation(&all_radials);
        let sweep_blobs = crate::nexrad::ingest_phases::extract_sweep_blobs(
            &by_elevation,
            &elevation_numbers,
            &scan_key,
        );
        let extract_ms = t_extract.elapsed().as_secs_f64() * 1000.0;

        let sweep_count = sweep_blobs.len() as u32;
        let total_sweep_bytes: u64 = sweep_blobs
            .iter()
            .map(|(_, b): &(String, Vec<u8>)| b.len() as u64)
            .sum();

        log::info!(
            "ingest: extracted {} sweeps ({:.1}MB) in {:.1}ms",
            sweep_count,
            total_sweep_bytes as f64 / (1024.0 * 1024.0),
            extract_ms,
        );

        // --- Phase 2.5: Delete any overlapping scans from IDB ---
        let archive_end_ms = end_timestamp_secs * 1000;
        let deleted = store
            .delete_overlapping_scans(
                &SiteId(site_id.clone()),
                scan_key.scan_start,
                archive_end_ms,
                &scan_key,
            )
            .await
            .map_err(|e| {
                wasm_bindgen::JsValue::from_str(&format!(
                    "Failed to delete overlapping scans: {}",
                    e
                ))
            })?;
        if deleted > 0 {
            log::info!("ingest: replaced {} overlapping scan(s)", deleted);
        }

        // --- Phase 3: Store sweep blobs in IDB ---
        let t_store = web_time::Instant::now();
        store.put_sweeps_batch(&sweep_blobs).await.map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to store sweeps batch: {}", e))
        })?;
        let store_ms = t_store.elapsed().as_secs_f64() * 1000.0;

        // --- Phase 4: Store scan index entry ---
        let t_index = web_time::Instant::now();
        let mut scan_entry = ScanIndexEntry::new(scan_key.clone());
        scan_entry.has_vcp = has_vcp;
        scan_entry.vcp = extracted_vcp.clone();
        scan_entry.present_records = records.len() as u32;
        scan_entry.file_name = Some(file_name.clone());
        scan_entry.total_size_bytes = total_sweep_bytes;
        scan_entry.end_timestamp_secs = Some(end_timestamp_secs);
        scan_entry.sweeps = Some(sweeps.clone());
        scan_entry.has_precomputed_sweeps = true;

        store.put_scan_index_entry(&scan_entry).await.map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to store scan index: {}", e))
        })?;
        let index_ms = t_index.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "ingest: complete {} in {:.0}ms | split {:.1} | decompress {:.1} | decode {:.1} | extract {:.1} | store {:.1} | index {:.1} | {} records, {} radials, {} elevations, {} sweeps, {:.1}MB",
            file_name, total_ms, split_ms, decompress_ms_total, decode_only_ms,
            extract_ms, store_ms, index_ms,
            records.len(), all_radials.len(), elevation_numbers.len(),
            sweep_count, total_sweep_bytes as f64 / (1024.0 * 1024.0),
        );

        // --- Build JS response ---
        let response = IngestResponse {
            records_stored: sweep_count,
            scan_key: scan_key.to_storage_key(),
            elevation_numbers: &elevation_numbers,
            total_ms,
            split_ms,
            decompress_ms: decompress_ms_total,
            decode_ms: decode_only_ms,
            extract_ms,
            store_ms,
            index_ms,
            sweeps: &sweeps,
            vcp: extracted_vcp.as_ref(),
        };
        serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))
    })
}

/// Render a specific elevation from pre-computed sweep data in IndexedDB.
///
/// Called from the Web Worker via worker.js. Fetches a single pre-computed
/// sweep blob and returns the data for GPU upload — no decoding needed.
///
/// Parameters (JS object): `{ scanKey: string, elevationNumber: number, product: string }`
/// Returns (JS object): `{ azimuths: Float32Array, gateValues: Float32Array, azimuthCount, gateCount, ... }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();

        let p: RenderParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid render params: {}", e)))?;
        let scan_key_str = p.scan_key;
        let elevation_number = p.elevation_number;
        let product_str = p.product;

        let scan_key = ScanKey::from_storage_key(&scan_key_str)
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Invalid scanKey format"))?;

        let store = idb_store().await?;

        // Fetch raw IDB ArrayBuffer (no Rust-side copy)
        let t_fetch = web_time::Instant::now();
        let sweep_key = SweepDataKey::new(scan_key, elevation_number, &product_str);
        let blob_buffer = store
            .get_sweep_as_js(&sweep_key.to_storage_key())
            .await
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Failed to fetch sweep: {}", e)))?
            .ok_or_else(|| {
                wasm_bindgen::JsValue::from_str(&format!(
                    "No pre-computed sweep for elev={} product={}",
                    elevation_number, product_str
                ))
            })?;
        let fetch_ms = t_fetch.elapsed().as_secs_f64() * 1000.0;
        let blob_len = blob_buffer.byte_length();

        // Parse header only (72 bytes) — no array allocations
        let t_deser = web_time::Instant::now();
        let header_bytes = {
            let view = js_sys::Uint8Array::new_with_byte_offset_and_length(&blob_buffer, 0, 72);
            let mut buf = [0u8; 72];
            view.copy_to(&mut buf);
            buf
        };
        let header = parse_sweep_header(&header_bytes).map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to parse sweep header: {}", e))
        })?;

        // Validate full blob size
        let az = header.azimuth_count as usize;
        let gc = header.gate_count as usize;
        let ws = header.data_word_size as usize;
        let expected = header.gate_values_offset as usize + az * gc * ws;
        if (blob_len as usize) < expected {
            return Err(wasm_bindgen::JsValue::from_str(&format!(
                "Sweep blob too small: {} < {} expected",
                blob_len, expected
            )));
        }
        let deser_ms = t_deser.elapsed().as_secs_f64() * 1000.0;

        // Marshal: create typed array views over raw IDB ArrayBuffer
        let t_marshal = web_time::Instant::now();

        let az_view = js_sys::Float32Array::new_with_byte_offset_and_length(
            &blob_buffer,
            header.azimuths_offset,
            header.azimuth_count,
        );
        let az_buf = az_view.slice(0, header.azimuth_count).buffer();

        // Extract radial_times if present (format version >= 1)
        let rt_buf = if header.radial_times_offset > 0 {
            let rt_view = js_sys::Float64Array::new_with_byte_offset_and_length(
                &blob_buffer,
                header.radial_times_offset as u32,
                header.azimuth_count,
            );
            Some(rt_view.slice(0, header.azimuth_count).buffer())
        } else {
            None
        };

        // Convert native-width gate values to f32 for GPU upload
        let gate_count_total = header.azimuth_count * header.gate_count;
        let val_buf = if header.data_word_size == 1 {
            let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                &blob_buffer,
                header.gate_values_offset,
                gate_count_total,
            );
            js_sys::Float32Array::new(&u8_view).buffer()
        } else {
            let u16_view = js_sys::Uint16Array::new_with_byte_offset_and_length(
                &blob_buffer,
                header.gate_values_offset,
                gate_count_total,
            );
            js_sys::Float32Array::new(&u16_view).buffer()
        };

        let marshal_ms = t_marshal.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "render: elev={} {} {}x{} ({:.1}KB) in {:.1}ms | fetch {:.1} | deser {:.1} | marshal {:.1}",
            elevation_number, product_str,
            header.azimuth_count, header.gate_count,
            blob_len as f64 / 1024.0,
            total_ms, fetch_ms, deser_ms, marshal_ms,
        );

        // Serialize scalar fields, then attach ArrayBuffer fields separately
        let response = RenderResponse {
            azimuth_count: header.azimuth_count,
            gate_count: header.gate_count,
            first_gate_range_km: header.first_gate_range_km,
            gate_interval_km: header.gate_interval_km,
            max_range_km: header.max_range_km,
            product: product_str,
            radial_count: header.radial_count,
            scale: header.scale as f64,
            offset: header.offset as f64,
            mean_elevation: header.mean_elevation as f64,
            sweep_start_secs: header.sweep_start_secs,
            sweep_end_secs: header.sweep_end_secs,
            fetch_ms,
            deser_ms,
            total_ms,
            marshal_ms,
        };
        let result = serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))?;
        // ArrayBuffer fields must be set directly (not serializable via serde)
        js_sys::Reflect::set(&result, &"azimuths".into(), &az_buf).ok();
        js_sys::Reflect::set(&result, &"gateValues".into(), &val_buf).ok();
        if let Some(rt) = rt_buf {
            js_sys::Reflect::set(&result, &"radialTimes".into(), &rt).ok();
        }
        Ok(result)
    })
}

// ---------------------------------------------------------------------------
// Live (partial sweep) render from in-memory ChunkAccumulator
// ---------------------------------------------------------------------------

/// Parameters for `worker_render_live`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderLiveParams {
    #[serde(default = "default_product")]
    product: String,
    #[serde(default)]
    elevation_number: Option<u8>,
}

/// Render the current partial sweep from the in-memory ChunkAccumulator.
///
/// This reads directly from memory (no IDB), so it's very fast (~1ms).
/// Returns the same RenderResponse shape as `worker_render`.
///
/// Parameters (JS object): `{ product: string, elevationNumber?: number }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render_live(params: wasm_bindgen::JsValue) -> Result<JsValue, JsValue> {
    use crate::nexrad::record_decode::extract_sweep_data_from_sorted;
    use nexrad_render::Product;

    let t_total = web_time::Instant::now();

    let p: RenderLiveParams = serde_wasm_bindgen::from_value(params)
        .map_err(|e| JsValue::from_str(&format!("Invalid render_live params: {}", e)))?;

    let product = match p.product.as_str() {
        "velocity" => Product::Velocity,
        "spectrum_width" => Product::SpectrumWidth,
        "differential_reflectivity" => Product::DifferentialReflectivity,
        "differential_phase" => Product::DifferentialPhase,
        "correlation_coefficient" => Product::CorrelationCoefficient,
        "clutter_filter_power" => Product::ClutterFilterPower,
        _ => Product::Reflectivity,
    };

    CHUNK_ACCUM.with(|cell| {
        let borrow = cell.borrow();
        let accum = borrow
            .as_ref()
            .ok_or_else(|| JsValue::from_str("No chunk accumulator active"))?;

        let target_elev = p
            .elevation_number
            .or(accum.last_elevation_number)
            .ok_or_else(|| JsValue::from_str("No elevation available in accumulator"))?;

        // Filter and sort radials for the target elevation
        let mut sorted: Vec<&::nexrad::model::data::Radial> = accum
            .all_radials
            .iter()
            .filter(|r| r.elevation_number() == target_elev)
            .collect();

        if sorted.is_empty() {
            return Err(JsValue::from_str(&format!(
                "No radials for elevation {} in accumulator",
                target_elev
            )));
        }

        sorted.sort_by(|a, b| {
            a.azimuth_angle_degrees()
                .partial_cmp(&b.azimuth_angle_degrees())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let sweep = extract_sweep_data_from_sorted(&sorted, product).ok_or_else(|| {
            JsValue::from_str(&format!(
                "No {} data for elevation {} in accumulator",
                p.product, target_elev
            ))
        })?;

        // Marshal PrecomputedSweep into the same JS response format as worker_render
        let t_marshal = web_time::Instant::now();

        // Convert gate values to f32 array
        let gate_values_f32: Vec<f32> = match &sweep.gate_values {
            crate::data::keys::GateValues::U8(v) => v.iter().map(|&x| x as f32).collect(),
            crate::data::keys::GateValues::U16(v) => v.iter().map(|&x| x as f32).collect(),
        };

        let az_array = js_sys::Float32Array::from(sweep.azimuths.as_slice());
        let az_buf = az_array.buffer();

        let val_array = js_sys::Float32Array::from(gate_values_f32.as_slice());
        let val_buf = val_array.buffer();

        let rt_array = js_sys::Float64Array::from(sweep.radial_times.as_slice());
        let rt_buf = rt_array.buffer();

        let marshal_ms = t_marshal.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        let accum_total = accum.all_radials.len();
        let elev_radials = sorted.len();
        let product_radials = sweep.azimuth_count;
        let expected_values = sweep.azimuth_count as usize * sweep.gate_count as usize;
        let actual_values = gate_values_f32.len();
        log::info!(
            "render_live: elev={} {} {}x{} accum_total={} elev_radials={} product_radials={} vals={}/{} az=[{:.1}..{:.1}] offset={} scale={} in {:.1}ms (marshal: {:.1}ms)",
            target_elev,
            p.product,
            sweep.azimuth_count,
            sweep.gate_count,
            accum_total,
            elev_radials,
            product_radials,
            actual_values,
            expected_values,
            sweep.azimuths.first().copied().unwrap_or(f32::NAN),
            sweep.azimuths.last().copied().unwrap_or(f32::NAN),
            sweep.offset,
            sweep.scale,
            total_ms,
            marshal_ms,
        );

        let response = RenderResponse {
            azimuth_count: sweep.azimuth_count,
            gate_count: sweep.gate_count,
            first_gate_range_km: sweep.first_gate_range_km,
            gate_interval_km: sweep.gate_interval_km,
            max_range_km: sweep.max_range_km,
            product: p.product,
            radial_count: sweep.radial_count,
            scale: sweep.scale as f64,
            offset: sweep.offset as f64,
            mean_elevation: sweep.mean_elevation as f64,
            sweep_start_secs: sweep.sweep_start_secs,
            sweep_end_secs: sweep.sweep_end_secs,
            fetch_ms: 0.0,
            deser_ms: 0.0,
            total_ms,
            marshal_ms,
        };
        let result = serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))?;
        js_sys::Reflect::set(&result, &"azimuths".into(), &az_buf).ok();
        js_sys::Reflect::set(&result, &"gateValues".into(), &val_buf).ok();
        js_sys::Reflect::set(&result, &"radialTimes".into(), &rt_buf).ok();
        Ok(result)
    })
}

// ---------------------------------------------------------------------------
// Volume render (all elevations packed for ray marching)
// ---------------------------------------------------------------------------

/// Render all elevations for a scan, packing raw gate data into a single buffer
/// for volumetric ray-march rendering on the GPU.
///
/// Parameters (JS object): `{ scanKey: string, product: string, elevationNumbers: number[] }`
/// Returns (JS object): `{ buffer: ArrayBuffer, sweepMeta: [...], product, totalMs }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render_volume(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        let t_total = web_time::Instant::now();

        let p: RenderVolumeParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid render_volume params: {}", e)))?;
        let scan_key_str = p.scan_key;
        let product_str = p.product;
        let elevation_numbers = p.elevation_numbers;

        let scan_key = ScanKey::from_storage_key(&scan_key_str)
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Invalid scanKey format"))?;

        let store = idb_store().await?;

        // Collect all sweep data into a packed buffer.
        // We keep native word size when all sweeps are u8 to halve transfer cost.
        // Only widen to u16 when at least one sweep has u16 data.
        let mut packed_data: Vec<u8> = Vec::new();
        let mut sweep_meta_vec: Vec<VolumeRenderSweepMeta> = Vec::new();
        let mut data_offset: u32 = 0; // offset in values (not bytes)
        let mut has_u16 = false;

        // First pass: read all sweep blobs and headers, determine word size
        struct SweepBlob {
            blob_buffer: js_sys::ArrayBuffer,
            header: SweepHeader,
            total_values: usize,
        }
        let mut sweep_blobs: Vec<SweepBlob> = Vec::new();

        for &elev_num in &elevation_numbers {
            let sweep_key = SweepDataKey::new(scan_key.clone(), elev_num, &product_str);
            let blob_buffer = match store.get_sweep_as_js(&sweep_key.to_storage_key()).await {
                Ok(Some(buf)) => buf,
                _ => continue, // skip missing elevations
            };

            let header_bytes = {
                let view = js_sys::Uint8Array::new_with_byte_offset_and_length(&blob_buffer, 0, 72);
                let mut buf = [0u8; 72];
                view.copy_to(&mut buf);
                buf
            };
            let header = match parse_sweep_header(&header_bytes) {
                Ok(h) => h,
                Err(_) => continue,
            };

            if header.data_word_size != 1 {
                has_u16 = true;
            }

            let total_values = header.azimuth_count as usize * header.gate_count as usize;
            sweep_blobs.push(SweepBlob {
                blob_buffer,
                header,
                total_values,
            });
        }

        // Second pass: pack data using native word size when all u8,
        // widening to u16 only when mixed.
        let word_size: u8 = if has_u16 { 2 } else { 1 };

        for sb in &sweep_blobs {
            let header = &sb.header;
            let total_values = sb.total_values;

            if word_size == 1 {
                // All sweeps are u8: copy raw bytes, no widening
                let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &sb.blob_buffer,
                    header.gate_values_offset,
                    total_values as u32,
                );
                let prev_len = packed_data.len();
                packed_data.resize(prev_len + total_values, 0);
                u8_view.copy_to(&mut packed_data[prev_len..]);
            } else if header.data_word_size == 1 {
                // Mixed volume: widen this u8 sweep to u16
                let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &sb.blob_buffer,
                    header.gate_values_offset,
                    total_values as u32,
                );
                let mut tmp = vec![0u8; total_values];
                u8_view.copy_to(&mut tmp);
                for &val in &tmp {
                    packed_data.extend_from_slice(&(val as u16).to_le_bytes());
                }
            } else {
                // Native u16: copy raw bytes directly
                let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &sb.blob_buffer,
                    header.gate_values_offset,
                    (total_values * 2) as u32,
                );
                let prev_len = packed_data.len();
                packed_data.resize(prev_len + total_values * 2, 0);
                u8_view.copy_to(&mut packed_data[prev_len..]);
            }

            sweep_meta_vec.push(VolumeRenderSweepMeta {
                elevation_deg: header.mean_elevation as f64,
                azimuth_count: header.azimuth_count,
                gate_count: header.gate_count,
                first_gate_km: header.first_gate_range_km,
                gate_interval_km: header.gate_interval_km,
                max_range_km: header.max_range_km,
                data_offset,
                scale: header.scale as f64,
                offset: header.offset as f64,
            });

            data_offset += total_values as u32;
        }

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "render_volume: {} sweeps, {} values packed ({:.1}KB, u{}) in {:.1}ms",
            sweep_meta_vec.len(),
            data_offset,
            packed_data.len() as f64 / 1024.0,
            word_size * 8,
            total_ms,
        );

        // Serialize scalar/struct fields, then attach the packed buffer separately
        let response = VolumeRenderResponse {
            sweep_count: sweep_meta_vec.len() as u32,
            word_size,
            sweep_meta: sweep_meta_vec,
            product: product_str,
            total_ms,
        };
        let result = serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))?;

        // ArrayBuffer must be set directly for zero-copy transfer
        let packed_u8 = js_sys::Uint8Array::from(&packed_data[..]);
        let packed_buffer = packed_u8.buffer();
        js_sys::Reflect::set(&result, &"buffer".into(), &packed_buffer).ok();

        Ok(result)
    })
}

// ---------------------------------------------------------------------------
// Per-chunk incremental ingest
// ---------------------------------------------------------------------------

/// Accumulator for per-chunk ingest. Holds decoded radials across chunks
/// until an elevation is complete, then flushes sweep blobs to IDB.
#[allow(dead_code)]
struct ChunkAccumulator {
    scan_key: ScanKey,
    site_id: String,
    all_radials: Vec<::nexrad::model::data::Radial>,
    radial_metas: Vec<(i64, u8, f32, f32)>,
    completed_elevations: std::collections::HashSet<u8>,
    last_elevation_number: Option<u8>,
    vcp: Option<ExtractedVcp>,
    has_vcp: bool,
    total_chunks: u32,
    total_size_bytes: u64,
    file_name: String,
    timestamp_secs: i64,
}

thread_local! {
    static CHUNK_ACCUM: std::cell::RefCell<Option<ChunkAccumulator>> =
        const { std::cell::RefCell::new(None) };
}

/// Ingest a single real-time chunk: decompress, decode, and store completed
/// elevations to IDB incrementally.
///
/// Called from the Web Worker via worker.js.
///
/// Parameters (JS object):
/// `{ data: ArrayBuffer, siteId: string, timestampSecs: number,
///    chunkIndex: number, isStart: bool, isEnd: bool, fileName: string }`
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_ingest_chunk(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        use crate::nexrad::extract_elevation_numbers;

        let t_total = web_time::Instant::now();

        // --- Extract parameters from JS ---
        let data = extract_data_bytes(&params)?;
        let p: IngestChunkParams = serde_wasm_bindgen::from_value(params)
            .map_err(|e| JsValue::from_str(&format!("Invalid ingest_chunk params: {}", e)))?;
        let site_id = p.site_id;
        let timestamp_secs = p.timestamp_secs as i64;
        let chunk_index = p.chunk_index;
        let is_start = p.is_start;
        let is_end = p.is_end;
        let file_name = p.file_name;

        let data_len = data.len();

        // --- Decode the chunk's record(s) into radials ---
        let (chunk_radials, chunk_vcp, chunk_has_vcp, mut volume_header_time_secs);

        if is_start {
            let result = crate::nexrad::ingest_phases::decode_start_chunk(data, false);
            chunk_radials = result.chunk_radials;
            chunk_vcp = result.chunk_vcp;
            chunk_has_vcp = result.chunk_has_vcp;
            volume_header_time_secs = result.volume_header_time_secs;

            // --- Delete any overlapping scans so we don't double-store ---
            let scan_key = ScanKey::new(site_id.as_str(), UnixMillis::from_secs(timestamp_secs));
            let overlap_start_secs = volume_header_time_secs
                .map(|t| t as i64)
                .unwrap_or(timestamp_secs);
            let overlap_start_ms = overlap_start_secs * 1000;
            let overlap_end_ms = (overlap_start_secs + 600) * 1000;
            let store = idb_store().await?;
            let deleted = store
                .delete_overlapping_scans(
                    &SiteId(site_id.clone()),
                    UnixMillis(overlap_start_ms),
                    overlap_end_ms,
                    &scan_key,
                )
                .await
                .map_err(|e| {
                    wasm_bindgen::JsValue::from_str(&format!(
                        "Failed to delete overlapping scans: {}",
                        e
                    ))
                })?;
            if deleted > 0 {
                log::info!(
                    "ingest_chunk: replaced {} overlapping scan(s) before real-time ingest",
                    deleted
                );
            }

            // --- Reset accumulator ---
            CHUNK_ACCUM.with(|cell| {
                *cell.borrow_mut() = Some(ChunkAccumulator {
                    scan_key,
                    site_id: site_id.clone(),
                    all_radials: Vec::new(),
                    radial_metas: Vec::new(),
                    completed_elevations: std::collections::HashSet::new(),
                    last_elevation_number: None,
                    vcp: None,
                    has_vcp: false,
                    total_chunks: 0,
                    total_size_bytes: 0,
                    file_name: file_name.clone(),
                    timestamp_secs,
                });
            });
        } else {
            let accum_has_full_vcp = CHUNK_ACCUM.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|a| a.vcp.as_ref())
                    .map(|v| !v.elevations.is_empty())
                    .unwrap_or(false)
            });

            let result = crate::nexrad::ingest_phases::decode_subsequent_chunk(
                &data,
                accum_has_full_vcp,
                chunk_index,
            );
            chunk_radials = result.chunk_radials;
            chunk_vcp = result.chunk_vcp;
            chunk_has_vcp = result.chunk_has_vcp;
            volume_header_time_secs = result.volume_header_time_secs;
        }

        if volume_header_time_secs.is_none() {
            volume_header_time_secs =
                crate::nexrad::record_decode::extract_volume_start_time(&chunk_radials);
        }

        // --- Update accumulator with this chunk's radials ---
        let chunk_elev_numbers = extract_elevation_numbers(&chunk_radials);
        let mut newly_completed: Vec<u8> = Vec::new();

        let time_spans = crate::nexrad::ingest_phases::compute_chunk_time_spans(&chunk_radials);
        let chunk_min_ts_secs = time_spans.chunk_min_ts_secs;
        let chunk_max_ts_secs = time_spans.chunk_max_ts_secs;
        let chunk_elev_spans = time_spans.chunk_elev_spans;
        let chunk_elev_az_ranges = time_spans.chunk_elev_az_ranges;
        let first_radial_azimuth = time_spans.first_radial_azimuth;
        let last_radial_azimuth = time_spans.last_radial_azimuth;
        let last_radial_time_secs = time_spans.last_radial_time_secs;

        // Detailed chunk diagnostics for real-time streaming debugging
        {
            let radial_count = chunk_radials.len();
            let elev_summary: Vec<String> = chunk_elev_spans
                .iter()
                .map(|(elev, _start, _end, count)| format!("e{}:{}r", elev, count))
                .collect();
            let total_accum_radials = CHUNK_ACCUM.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map(|a| a.all_radials.len())
                    .unwrap_or(0)
            });
            log::info!(
                "Chunk#{} elev=[{}] radials={} az_range=[{:.1}..{:.1}] accum_total={} is_start={} is_end={} size={}B",
                chunk_index,
                elev_summary.join(", "),
                radial_count,
                first_radial_azimuth.unwrap_or(0.0),
                last_radial_azimuth.unwrap_or(0.0),
                total_accum_radials,
                is_start,
                is_end,
                data_len,
            );
        }

        CHUNK_ACCUM.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let accum = borrow.as_mut().ok_or_else(|| {
                wasm_bindgen::JsValue::from_str("No accumulator — missing Start chunk?")
            })?;

            accum.total_chunks += 1;
            accum.total_size_bytes += data_len as u64;

            // Update VCP if newly extracted or if the chunk has a fuller VCP
            // (i.e. one with elevation details upgrading a number-only VCP).
            if chunk_has_vcp {
                accum.has_vcp = true;
            }
            if let Some(ref new_vcp) = chunk_vcp {
                let should_upgrade = match accum.vcp {
                    None => true,
                    Some(ref existing) => {
                        existing.elevations.is_empty() && !new_vcp.elevations.is_empty()
                    }
                };
                if should_upgrade {
                    accum.vcp = chunk_vcp.clone();
                }
            }

            // Check for elevation transition → previous elevation is complete
            if let Some(first_elev) = chunk_elev_numbers.first() {
                if let Some(last) = accum.last_elevation_number {
                    if *first_elev != last && !accum.completed_elevations.contains(&last) {
                        newly_completed.push(last);
                        accum.completed_elevations.insert(last);
                    }
                }
            }

            // Append radials and metadata
            for r in &chunk_radials {
                accum.radial_metas.push((
                    r.collection_timestamp(),
                    r.elevation_number(),
                    r.elevation_angle_degrees(),
                    r.azimuth_angle_degrees(),
                ));
            }
            accum.all_radials.extend(chunk_radials);

            // Update last elevation number
            if let Some(&last) = chunk_elev_numbers.last() {
                accum.last_elevation_number = Some(last);
            }

            Ok::<(), wasm_bindgen::JsValue>(())
        })?;

        // On end, finalize all remaining elevations
        if is_end {
            CHUNK_ACCUM.with(|cell| {
                let mut borrow = cell.borrow_mut();
                if let Some(accum) = borrow.as_mut() {
                    // All unique elevation numbers that haven't been completed yet
                    let all_elevs: std::collections::HashSet<u8> = accum
                        .all_radials
                        .iter()
                        .map(|r| r.elevation_number())
                        .collect();
                    for elev in all_elevs {
                        if !accum.completed_elevations.contains(&elev) {
                            newly_completed.push(elev);
                            accum.completed_elevations.insert(elev);
                        }
                    }
                }
            });
        }

        // --- Flush completed elevations to IDB ---
        let mut sweeps_stored: u32 = 0;
        let new_sweep_metas: Vec<SweepMeta>;
        let new_size_bytes: u64;

        if !newly_completed.is_empty() {
            let store = idb_store().await?;

            // Build sweep blobs for completed elevations
            let (sweep_blobs, sweep_metas) = CHUNK_ACCUM.with(|cell| {
                let borrow = cell.borrow();
                let accum = borrow.as_ref().unwrap();
                crate::nexrad::ingest_phases::build_flush_sweep_blobs(
                    &accum.all_radials,
                    &accum.radial_metas,
                    &newly_completed,
                    &accum.scan_key,
                )
            });

            new_size_bytes = sweep_blobs.iter().map(|(_, b)| b.len() as u64).sum();
            sweeps_stored = sweep_blobs.len() as u32;
            new_sweep_metas = sweep_metas;

            // Store sweep blobs
            if !sweep_blobs.is_empty() {
                store.put_sweeps_batch(&sweep_blobs).await.map_err(|e| {
                    wasm_bindgen::JsValue::from_str(&format!("Failed to store sweeps: {}", e))
                })?;
            }

            // Merge scan index entry
            let partial_entry = CHUNK_ACCUM.with(|cell| {
                let borrow = cell.borrow();
                let accum = borrow.as_ref().unwrap();

                let end_ts = new_sweep_metas
                    .iter()
                    .map(|s| s.end as i64)
                    .max()
                    .unwrap_or(accum.timestamp_secs);

                let mut entry = ScanIndexEntry::new(accum.scan_key.clone());
                entry.has_vcp = accum.has_vcp;
                entry.vcp = accum.vcp.clone();
                entry.file_name = Some(accum.file_name.clone());
                entry.end_timestamp_secs = Some(end_ts);
                if let Some(ref vcp) = accum.vcp {
                    entry.expected_records = Some(vcp.elevations.len() as u32);
                }
                entry
            });

            store
                .merge_scan_index_entry(
                    &partial_entry,
                    newly_completed.len() as u32,
                    new_size_bytes,
                    &new_sweep_metas,
                )
                .await
                .map_err(|e| {
                    wasm_bindgen::JsValue::from_str(&format!("Failed to merge scan index: {}", e))
                })?;
        }

        // --- Build the scan key for response ---
        let scan_key_str = CHUNK_ACCUM.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|a| a.scan_key.to_storage_key())
                .unwrap_or_default()
        });

        // Build sweep metadata for all completed elevations so far
        let all_sweeps = CHUNK_ACCUM.with(|cell| {
            let borrow = cell.borrow();
            let accum = borrow.as_ref().unwrap();
            let all_metas = crate::nexrad::ingest_phases::build_sweep_meta(&accum.radial_metas);
            // Only include completed elevations
            all_metas
                .into_iter()
                .filter(|m| accum.completed_elevations.contains(&m.elevation_number))
                .collect::<Vec<SweepMeta>>()
        });

        let vcp = CHUNK_ACCUM.with(|cell| cell.borrow().as_ref().and_then(|a| a.vcp.clone()));

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        let accum_info = CHUNK_ACCUM.with(|c| {
            c.borrow()
                .as_ref()
                .map(|a| {
                    (
                        a.all_radials.len(),
                        a.has_vcp,
                        a.vcp.as_ref().map(|v| v.number),
                    )
                })
                .unwrap_or((0, false, None))
        });
        // Build per-elevation summary of the full accumulator state
        let chunk_detail = {
            use std::collections::BTreeMap;
            CHUNK_ACCUM.with(|cell| {
                let borrow = cell.borrow();
                let Some(accum) = borrow.as_ref() else {
                    return String::from("no accum");
                };

                // Per-elevation stats from this chunk's contribution
                // We know chunk added radials at indices [prev_count..current_count]
                // But we don't have prev_count here. Instead, summarize the
                // per-elevation state of the full accumulator.
                let mut by_elev: BTreeMap<u8, Vec<f32>> = BTreeMap::new();
                for r in &accum.all_radials {
                    by_elev
                        .entry(r.elevation_number())
                        .or_default()
                        .push(r.azimuth_angle_degrees());
                }

                let mut parts: Vec<String> = Vec::new();
                for (elev, mut azimuths) in by_elev {
                    azimuths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let count = azimuths.len();
                    let min_az = azimuths.first().copied().unwrap_or(0.0);
                    let max_az = azimuths.last().copied().unwrap_or(0.0);
                    // Compute total angular span (handles wrap-around)
                    let span = if count <= 1 {
                        0.0
                    } else {
                        // Use sorted gaps to detect wrap
                        let mut max_gap = 0.0f32;
                        for i in 1..azimuths.len() {
                            let gap = azimuths[i] - azimuths[i - 1];
                            if gap > max_gap {
                                max_gap = gap;
                            }
                        }
                        let wrap_gap = (360.0 - max_az + min_az).max(0.0);
                        if wrap_gap > max_gap {
                            // No wrap: span is simply max - min
                            max_az - min_az
                        } else {
                            // Wraps through 0: span is 360 - largest_gap
                            360.0 - max_gap
                        }
                    };
                    let completed = if accum.completed_elevations.contains(&elev) {
                        " done"
                    } else {
                        ""
                    };
                    parts.push(format!(
                        "e{}:{}az {:.0}-{:.0}({:.0}°){}",
                        elev, count, min_az, max_az, span, completed
                    ));
                }

                // Product summary: check which products are present in the accumulator
                let mut products_present: Vec<&str> = Vec::new();
                let sample = accum.all_radials.first();
                if let Some(r) = sample {
                    use nexrad_render::Product;
                    for (p, name) in [
                        (Product::Reflectivity, "REF"),
                        (Product::Velocity, "VEL"),
                        (Product::SpectrumWidth, "SW"),
                        (Product::DifferentialReflectivity, "ZDR"),
                        (Product::CorrelationCoefficient, "CC"),
                        (Product::DifferentialPhase, "PHI"),
                    ] {
                        if p.moment_data(r).is_some() || p.cfp_moment_data(r).is_some() {
                            products_present.push(name);
                        }
                    }
                }

                format!(
                    "[{}] products=[{}]",
                    parts.join(" | "),
                    products_present.join(",")
                )
            })
        };

        log::info!(
            "ingest_chunk: chunk={} is_start={} is_end={} radials={} vcp={:?} has_vcp={} completed_elevs={:?} sweeps_stored={} {:.1}ms {}",
            chunk_index, is_start, is_end,
            accum_info.0, accum_info.2, accum_info.1,
            newly_completed, sweeps_stored, total_ms,
            chunk_detail,
        );

        // Current in-progress elevation info
        let current_elevation =
            CHUNK_ACCUM.with(|c| c.borrow().as_ref().and_then(|a| a.last_elevation_number));
        let current_elevation_radials = CHUNK_ACCUM.with(|c| {
            c.borrow().as_ref().and_then(|a| {
                a.last_elevation_number
                    .map(|elev| a.radial_metas.iter().filter(|m| m.1 == elev).count() as u32)
            })
        });

        // --- Clear accumulator on end ---
        if is_end {
            CHUNK_ACCUM.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }

        // --- Build JS response ---
        let response = ChunkIngestResponse {
            chunk_index,
            radials_decoded: chunk_elev_numbers.len() as u32,
            sweeps_stored,
            scan_key: scan_key_str,
            is_end,
            total_ms,
            sweeps: all_sweeps,
            elevations_completed: newly_completed,
            vcp,
            chunk_min_time_secs: chunk_min_ts_secs,
            chunk_max_time_secs: chunk_max_ts_secs,
            chunk_elev_spans,
            chunk_elev_az_ranges,
            volume_header_time_secs,
            last_radial_azimuth,
            last_radial_time_secs,
            current_elevation,
            current_elevation_radials,
        };
        serde_wasm_bindgen::to_value(&response)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize response: {}", e)))
    })
}
