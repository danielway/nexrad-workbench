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
        use crate::nexrad::record_decode::extract_sweep_data_from_sorted;
        use nexrad_render::Product;
        use std::collections::HashMap;

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
        let mut decompress_ms_total = 0.0f64;
        let mut decode_only_ms = 0.0f64;
        let mut all_radials: Vec<::nexrad::model::data::Radial> = Vec::new();
        let mut radial_metas: Vec<(i64, u8, f32)> = Vec::new();
        let mut has_vcp = false;
        let mut extracted_vcp: Option<ExtractedVcp> = None;
        let mut compressed_count = 0u32;

        for (record_id, record) in records.iter().enumerate() {
            let record_id = record_id as u32;

            let radials = if record.compressed() {
                compressed_count += 1;
                let t_decompress = web_time::Instant::now();
                let decompressed = record.decompress().map_err(|e| {
                    wasm_bindgen::JsValue::from_str(&format!(
                        "Failed to decompress record {}: {}",
                        record_id, e
                    ))
                })?;
                decompress_ms_total += t_decompress.elapsed().as_secs_f64() * 1000.0;
                let t_radials = web_time::Instant::now();

                let needs_vcp = extracted_vcp
                    .as_ref()
                    .map(|v| v.elevations.is_empty())
                    .unwrap_or(true);
                let r = if needs_vcp {
                    match decompressed.messages() {
                        Ok(msgs) => decode_with_vcp_extraction(msgs, &mut extracted_vcp),
                        Err(_) => Vec::new(),
                    }
                } else {
                    decompressed.radials().unwrap_or_default()
                };

                decode_only_ms += t_radials.elapsed().as_secs_f64() * 1000.0;
                r
            } else {
                use crate::nexrad::record_decode::decode_record_to_radials;
                let t_radials = web_time::Instant::now();
                let r = decode_record_to_radials(record.data()).unwrap_or_default();
                decode_only_ms += t_radials.elapsed().as_secs_f64() * 1000.0;
                r
            };

            if record_id == 0 {
                has_vcp = true;
            }

            if !radials.is_empty() {
                for r in &radials {
                    radial_metas.push((
                        r.collection_timestamp(),
                        r.elevation_number(),
                        r.elevation_angle_degrees(),
                    ));
                }
                all_radials.extend(radials);
            }
        }
        let phase1_ms = t_decode.elapsed().as_secs_f64() * 1000.0;

        let sweeps = build_sweep_meta(&radial_metas);
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

        let products = [
            (Product::Reflectivity, "reflectivity"),
            (Product::Velocity, "velocity"),
            (Product::SpectrumWidth, "spectrum_width"),
            (
                Product::DifferentialReflectivity,
                "differential_reflectivity",
            ),
            (Product::CorrelationCoefficient, "correlation_coefficient"),
            (Product::DifferentialPhase, "differential_phase"),
        ];

        // Group radials by elevation in one pass, sort each group by azimuth once
        let mut by_elevation: HashMap<u8, Vec<&::nexrad::model::data::Radial>> = HashMap::new();
        for radial in &all_radials {
            by_elevation
                .entry(radial.elevation_number())
                .or_default()
                .push(radial);
        }
        for group in by_elevation.values_mut() {
            group.sort_by(|a, b| {
                a.azimuth_angle_degrees()
                    .partial_cmp(&b.azimuth_angle_degrees())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let mut sweep_blobs: Vec<(String, Vec<u8>)> = Vec::new();
        for &elev_num in &elevation_numbers {
            if let Some(sorted_radials) = by_elevation.get(&elev_num) {
                for (product, product_name) in &products {
                    if let Some(sweep) = extract_sweep_data_from_sorted(sorted_radials, *product) {
                        let key = SweepDataKey::new(scan_key.clone(), elev_num, *product_name);
                        sweep_blobs.push((key.to_storage_key(), sweep.to_bytes()));
                    }
                }
            }
        }
        let extract_ms = t_extract.elapsed().as_secs_f64() * 1000.0;

        let sweep_count = sweep_blobs.len() as u32;
        let total_sweep_bytes: u64 = sweep_blobs.iter().map(|(_, b)| b.len() as u64).sum();

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
        // We widen everything to u16 (2 bytes per gate) so the shader only needs one format.
        let mut packed_data: Vec<u8> = Vec::new();
        let mut sweep_meta_vec: Vec<VolumeRenderSweepMeta> = Vec::new();
        let mut data_offset: u32 = 0; // offset in u16 elements, not bytes

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

            let az = header.azimuth_count as usize;
            let gc = header.gate_count as usize;
            let total_values = az * gc;

            // Copy raw gate values, widening u8 → u16 if needed
            if header.data_word_size == 1 {
                let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &blob_buffer,
                    header.gate_values_offset,
                    total_values as u32,
                );
                let mut tmp = vec![0u8; total_values];
                u8_view.copy_to(&mut tmp);
                for &val in &tmp {
                    packed_data.extend_from_slice(&(val as u16).to_le_bytes());
                }
            } else {
                // u16: copy raw bytes directly (already little-endian)
                let u8_view = js_sys::Uint8Array::new_with_byte_offset_and_length(
                    &blob_buffer,
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
            "render_volume: {} sweeps, {} values packed ({:.1}KB) in {:.1}ms",
            sweep_meta_vec.len(),
            data_offset,
            packed_data.len() as f64 / 1024.0,
            total_ms,
        );

        // Serialize scalar/struct fields, then attach the packed buffer separately
        let response = VolumeRenderResponse {
            sweep_count: sweep_meta_vec.len() as u32,
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
    radial_metas: Vec<(i64, u8, f32)>,
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
        use crate::nexrad::record_decode::extract_sweep_data_from_sorted;
        use nexrad_render::Product;
        use std::collections::HashMap;

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
        let mut chunk_radials: Vec<::nexrad::model::data::Radial> = Vec::new();
        let mut chunk_vcp: Option<ExtractedVcp> = None;
        let mut chunk_has_vcp = false;

        // Volume header scan start time (extracted from start chunk only)
        let mut volume_header_time_secs: Option<f64> = None;

        if is_start {
            // Start chunk = volume header + first compressed record
            let file = nexrad_data::volume::File::new(data);

            // Extract the volume scan start time from the Archive II header
            if let Some(header) = file.header() {
                if let Some(dt) = header.date_time() {
                    volume_header_time_secs = Some(dt.timestamp() as f64);
                }
            }

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

            let records = file.records().map_err(|e| {
                wasm_bindgen::JsValue::from_str(&format!("Failed to split start chunk: {}", e))
            })?;

            // Check if accumulator already has a full VCP (with elevation details).
            // A number-only VCP (empty elevations) should still allow extraction
            // so a Message Type 5 can upgrade it.
            let accum_has_full_vcp = CHUNK_ACCUM.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|a| a.vcp.as_ref())
                    .map(|v| !v.elevations.is_empty())
                    .unwrap_or(false)
            });

            for (i, record) in records.iter().enumerate() {
                if record.compressed() {
                    match record.decompress() {
                        Ok(decompressed) => {
                            if !accum_has_full_vcp
                                && chunk_vcp
                                    .as_ref()
                                    .map(|v| v.elevations.is_empty())
                                    .unwrap_or(true)
                            {
                                if let Ok(msgs) = decompressed.messages() {
                                    let r = decode_with_vcp_extraction(msgs, &mut chunk_vcp);
                                    chunk_radials.extend(r);
                                }
                            } else {
                                chunk_radials.extend(decompressed.radials().unwrap_or_default());
                            }
                        }
                        Err(e) => {
                            log::warn!("Failed to decompress record {} in start chunk: {}", i, e);
                        }
                    }
                } else {
                    use crate::nexrad::record_decode::decode_record_to_radials;
                    chunk_radials
                        .extend(decode_record_to_radials(record.data()).unwrap_or_default());
                }
                if chunk_vcp.is_some() {
                    chunk_has_vcp = true;
                }
            }
        } else {
            // Subsequent chunk = single compressed LDM record
            use nexrad_data::volume::Record;
            let record = Record::from_slice(&data);

            // Check if accumulator already has a full VCP (with elevation details).
            let accum_has_full_vcp = CHUNK_ACCUM.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .and_then(|a| a.vcp.as_ref())
                    .map(|v| !v.elevations.is_empty())
                    .unwrap_or(false)
            });

            if record.compressed() {
                match record.decompress() {
                    Ok(decompressed) => {
                        if !accum_has_full_vcp {
                            if let Ok(msgs) = decompressed.messages() {
                                let r = decode_with_vcp_extraction(msgs, &mut chunk_vcp);
                                chunk_radials.extend(r);
                            }
                        } else {
                            chunk_radials.extend(decompressed.radials().unwrap_or_default());
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to decompress chunk {}: {}", chunk_index, e);
                    }
                }
            } else {
                use crate::nexrad::record_decode::decode_record_to_radials;
                chunk_radials.extend(decode_record_to_radials(record.data()).unwrap_or_default());
            }
        }

        // Extract volume start time from radials (available directly from the
        // NEXRAD message header on the first radial of a volume scan). This
        // replaces the Archive II header-only approach, giving us an
        // authoritative timestamp from any chunk that contains the volume's
        // first radial.
        if volume_header_time_secs.is_none() {
            volume_header_time_secs =
                crate::nexrad::record_decode::extract_volume_start_time(&chunk_radials);
        }

        // --- Update accumulator with this chunk's radials ---
        // Detect which elevations in this chunk's radials differ from last_elevation_number
        let chunk_elev_numbers = extract_elevation_numbers(&chunk_radials);
        let mut newly_completed: Vec<u8> = Vec::new();

        // Compute the actual data time range of this chunk from radial timestamps (ms → secs).
        let chunk_min_ts_secs: Option<f64> = chunk_radials
            .iter()
            .map(|r| r.collection_timestamp() as f64 / 1000.0)
            .reduce(f64::min);
        let chunk_max_ts_secs: Option<f64> = chunk_radials
            .iter()
            .map(|r| r.collection_timestamp() as f64 / 1000.0)
            .reduce(f64::max);

        // Compute per-elevation time spans within this chunk.
        // Each entry: (elevation_number, min_time_secs, max_time_secs, radial_count)
        let chunk_elev_spans: Vec<(u8, f64, f64, u32)> = {
            let mut map: std::collections::BTreeMap<u8, (f64, f64, u32)> =
                std::collections::BTreeMap::new();
            for r in &chunk_radials {
                let elev = r.elevation_number();
                let t = r.collection_timestamp() as f64 / 1000.0;
                map.entry(elev)
                    .and_modify(|(min, max, count)| {
                        if t < *min {
                            *min = t;
                        }
                        if t > *max {
                            *max = t;
                        }
                        *count += 1;
                    })
                    .or_insert((t, t, 1));
            }
            map.into_iter()
                .map(|(elev, (min, max, count))| (elev, min, max, count))
                .collect()
        };

        // Last radial's azimuth and timestamp — used to extrapolate sweep line position
        // between chunks during real-time streaming.
        let last_radial_azimuth: Option<f32> =
            chunk_radials.last().map(|r| r.azimuth_angle_degrees());
        let last_radial_time_secs: Option<f64> = chunk_radials
            .last()
            .map(|r| r.collection_timestamp() as f64 / 1000.0);

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

            let products = [
                (Product::Reflectivity, "reflectivity"),
                (Product::Velocity, "velocity"),
                (Product::SpectrumWidth, "spectrum_width"),
                (
                    Product::DifferentialReflectivity,
                    "differential_reflectivity",
                ),
                (Product::CorrelationCoefficient, "correlation_coefficient"),
                (Product::DifferentialPhase, "differential_phase"),
            ];

            // Build sweep blobs for completed elevations
            let (sweep_blobs, sweep_metas, _scan_key_clone) = CHUNK_ACCUM.with(|cell| {
                let borrow = cell.borrow();
                let accum = borrow.as_ref().unwrap();

                // Group radials by elevation
                let mut by_elevation: HashMap<u8, Vec<&::nexrad::model::data::Radial>> =
                    HashMap::new();
                for radial in &accum.all_radials {
                    by_elevation
                        .entry(radial.elevation_number())
                        .or_default()
                        .push(radial);
                }
                for group in by_elevation.values_mut() {
                    group.sort_by(|a, b| {
                        a.azimuth_angle_degrees()
                            .partial_cmp(&b.azimuth_angle_degrees())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                }

                let mut blobs: Vec<(String, Vec<u8>)> = Vec::new();
                let mut metas: Vec<SweepMeta> = Vec::new();

                for &elev_num in &newly_completed {
                    if let Some(sorted_radials) = by_elevation.get(&elev_num) {
                        for (product, product_name) in &products {
                            if let Some(sweep) =
                                extract_sweep_data_from_sorted(sorted_radials, *product)
                            {
                                let key = SweepDataKey::new(
                                    accum.scan_key.clone(),
                                    elev_num,
                                    *product_name,
                                );
                                blobs.push((key.to_storage_key(), sweep.to_bytes()));
                            }
                        }

                        // Build sweep meta for this elevation
                        let elev_metas: Vec<&(i64, u8, f32)> = accum
                            .radial_metas
                            .iter()
                            .filter(|(_, en, _)| *en == elev_num)
                            .collect();
                        if !elev_metas.is_empty() {
                            let min_ts = elev_metas.iter().map(|(t, _, _)| *t).min().unwrap();
                            let max_ts = elev_metas.iter().map(|(t, _, _)| *t).max().unwrap();
                            let angle_sum: f64 = elev_metas.iter().map(|(_, _, a)| *a as f64).sum();
                            let count = elev_metas.len();
                            metas.push(SweepMeta {
                                start: min_ts as f64 / 1000.0,
                                end: max_ts as f64 / 1000.0,
                                elevation: (angle_sum / count as f64) as f32,
                                elevation_number: elev_num,
                            });
                        }
                    }
                }

                (blobs, metas, accum.scan_key.clone())
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
            let all_metas = build_sweep_meta(&accum.radial_metas);
            // Only include completed elevations
            all_metas
                .into_iter()
                .filter(|m| accum.completed_elevations.contains(&m.elevation_number))
                .collect::<Vec<SweepMeta>>()
        });

        let vcp = CHUNK_ACCUM.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|a| a.vcp.clone())
        });

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
        log::info!(
            "ingest_chunk: chunk={} is_start={} is_end={} radials={} vcp={:?} has_vcp={} completed_elevs={:?} sweeps_stored={} {:.1}ms",
            chunk_index, is_start, is_end,
            accum_info.0, accum_info.2, accum_info.1,
            newly_completed, sweeps_stored, total_ms,
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

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Decode messages from a decompressed record, extracting VCP pattern in the same pass.
///
/// The `messages` iterator comes from `DecompressedRecord::messages()`. We accept
/// the already-resolved iterator to avoid naming the `DecompressedRecord` type
/// which is not publicly exported.
fn decode_with_vcp_extraction<'a>(
    messages: impl IntoIterator<Item = nexrad_decode::messages::Message<'a>>,
    extracted_vcp: &mut Option<ExtractedVcp>,
) -> Vec<::nexrad::model::data::Radial> {
    use nexrad_decode::messages::MessageContents;

    let mut radials = Vec::new();
    for msg in messages {
        // Allow VolumeCoveragePattern (Message Type 5) to always overwrite the
        // extracted VCP — even when a DigitalRadarData fallback already set the
        // number — so we capture the full elevation detail.  The DRD fallback
        // only fires when nothing has been extracted yet.
        let has_full_vcp = extracted_vcp
            .as_ref()
            .map(|v| !v.elevations.is_empty())
            .unwrap_or(false);

        match msg.contents() {
            MessageContents::VolumeCoveragePattern(ref vcp_msg) if !has_full_vcp => {
                let header = vcp_msg.header();
                let elevations: Vec<ExtractedVcpElevation> = vcp_msg
                    .elevations()
                    .iter()
                    .map(|e| ExtractedVcpElevation {
                        angle: e.elevation_angle() as f32,
                        waveform: format!("{:?}", e.waveform_type()),
                        prf_number: e.surveillance_prf_number(),
                        is_sails: e.is_sails_cut(),
                        is_mrle: e.is_mrle_cut(),
                        is_base_tilt: e.is_base_tilt_cut(),
                        azimuth_rate: {
                            let rate = e.azimuth_rate();
                            if rate > 0.0 {
                                Some(rate as f32)
                            } else {
                                None
                            }
                        },
                    })
                    .collect();
                *extracted_vcp = Some(ExtractedVcp {
                    number: header.pattern_number(),
                    elevations,
                });
            }
            MessageContents::DigitalRadarData(ref m) if extracted_vcp.is_none() => {
                if let Some(vol_block) = m.volume_data_block() {
                    let raw = vol_block.volume_coverage_pattern_number();
                    if raw > 0 {
                        *extracted_vcp = Some(ExtractedVcp {
                            number: raw,
                            elevations: Vec::new(),
                        });
                    }
                }
            }
            _ => {}
        }
        match msg.into_contents() {
            MessageContents::DigitalRadarData(m) => {
                if let Ok(radial) = m.into_radial() {
                    radials.push(radial);
                }
            }
            MessageContents::DigitalRadarDataLegacy(m) => {
                if let Ok(radial) = m.into_radial() {
                    radials.push(radial);
                }
            }
            _ => {}
        }
    }
    radials
}

/// Build `SweepMeta` entries by grouping radial metadata by elevation number.
///
/// Each tuple is `(timestamp_ms, elevation_number, elevation_angle_degrees)`.
fn build_sweep_meta(radial_metas: &[(i64, u8, f32)]) -> Vec<SweepMeta> {
    use std::collections::BTreeMap;

    struct Accum {
        min_ts_ms: i64,
        max_ts_ms: i64,
        angle_sum: f64,
        count: u32,
    }

    let mut groups: BTreeMap<u8, Accum> = BTreeMap::new();

    for &(ts_ms, elev_num, elev_angle) in radial_metas {
        let entry = groups.entry(elev_num).or_insert(Accum {
            min_ts_ms: ts_ms,
            max_ts_ms: ts_ms,
            angle_sum: 0.0,
            count: 0,
        });
        entry.min_ts_ms = entry.min_ts_ms.min(ts_ms);
        entry.max_ts_ms = entry.max_ts_ms.max(ts_ms);
        entry.angle_sum += elev_angle as f64;
        entry.count += 1;
    }

    groups
        .into_iter()
        .map(|(elev_num, acc)| SweepMeta {
            start: acc.min_ts_ms as f64 / 1000.0,
            end: acc.max_ts_ms as f64 / 1000.0,
            elevation: (acc.angle_sum / acc.count as f64) as f32,
            elevation_number: elev_num,
        })
        .collect()
}
