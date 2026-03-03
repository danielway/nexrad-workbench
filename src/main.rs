#![warn(clippy::all)]

//! NEXRAD Workbench - A web-based radar data visualization tool.
//!
//! This application provides an interface for loading, viewing, and analyzing
//! NEXRAD weather radar data. It supports AWS archive browsing and realtime
//! streaming (when implemented).

mod data;
mod geo;
mod nexrad;
mod state;
mod ui;

use data::DataFacade;
use eframe::egui;
use state::AppState;

fn main() {}

// ---------------------------------------------------------------------------
// Worker-side cached IDB connection
// ---------------------------------------------------------------------------
// WASM is single-threaded so thread_local! is safe.  We keep a single
// IndexedDbRecordStore alive for the lifetime of the worker so that
// subsequent ingest/render calls reuse the already-open IDB connection
// instead of paying the ~60ms open+list overhead every time.

thread_local! {
    static WORKER_IDB: std::cell::RefCell<Option<data::indexeddb::IndexedDbRecordStore>> =
        const { std::cell::RefCell::new(None) };
    static WORKER_LOGGER_INIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Initialize the log crate in the worker context (once).
/// The main thread uses `eframe::WebLogger` during `eframe::WebRunner::start()`,
/// but the worker loads a separate WASM instance that never calls that.
fn worker_init_logger() {
    WORKER_LOGGER_INIT.with(|init| {
        if !init.get() {
            eframe::WebLogger::init(log::LevelFilter::Debug).ok();
            init.set(true);
        }
    });
}

/// Get (or lazily open) the shared worker IDB store.
async fn worker_idb_store() -> Result<data::indexeddb::IndexedDbRecordStore, wasm_bindgen::JsValue> {
    let existing = WORKER_IDB.with(|cell| cell.borrow().clone());
    if let Some(store) = existing {
        return Ok(store);
    }
    let store = data::indexeddb::IndexedDbRecordStore::new();
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
/// Called from the Web Worker. Returns a Promise that resolves to a JS object with:
///   { recordsStored, scanKey, elevationMap: { recordId: [elevNums] } }
///
/// Parameters are passed as a JS object:
///   { data: ArrayBuffer, siteId: string, timestampSecs: number, fileName: string }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_ingest(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    worker_init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        use crate::data::keys::*;
        use crate::nexrad::extract_elevation_numbers;
        use nexrad_render::Product;

        let t_total = web_time::Instant::now();

        // Extract parameters from JS object
        let data_val = js_sys::Reflect::get(&params, &"data".into())
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Missing data: {:?}", e)))?;
        let data_array = js_sys::Uint8Array::new(&data_val);
        let data = data_array.to_vec();

        let site_id = js_sys::Reflect::get(&params, &"siteId".into())
            .ok()
            .and_then(|v| v.as_string())
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Missing siteId"))?;

        let timestamp_secs = js_sys::Reflect::get(&params, &"timestampSecs".into())
            .ok()
            .and_then(|v| v.as_f64())
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Missing timestampSecs"))?
            as i64;

        let file_name = js_sys::Reflect::get(&params, &"fileName".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();

        let data_size = data.len();
        log::info!(
            "ingest: received {} ({:.1}MB)",
            file_name,
            data_size as f64 / (1024.0 * 1024.0),
        );

        // --- Split into LDM records ---
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

        // Reuse cached IDB connection (or open on first call)
        let store = worker_idb_store().await?;

        let scan_start = UnixMillis::from_secs(timestamp_secs);
        let scan_key = ScanKey::new(site_id.as_str(), scan_start);

        // --- Phase 1: Decompress + decode all records into radials ---
        let t_decode = web_time::Instant::now();
        let mut decompress_ms_total = 0.0f64;
        let mut decode_only_ms = 0.0f64;
        let mut all_radials: Vec<::nexrad::model::data::Radial> = Vec::new();
        let mut radial_metas: Vec<(i64, u8, f32)> = Vec::new();
        let elevation_map = js_sys::Object::new();
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

                let r = if extracted_vcp.is_none() {
                    // Decode messages manually to extract VCP pattern in the same pass
                    use nexrad_decode::messages::MessageContents;
                    match decompressed.messages() {
                        Ok(messages) => {
                            let mut radials = Vec::new();
                            for msg in messages {
                                // Borrow to read VCP data before consuming
                                if extracted_vcp.is_none() {
                                    match msg.contents() {
                                        MessageContents::VolumeCoveragePattern(ref vcp_msg) => {
                                            let header = vcp_msg.header();
                                            let elevations: Vec<ExtractedVcpElevation> = vcp_msg.elevations().iter().map(|e| {
                                                ExtractedVcpElevation {
                                                    angle: e.elevation_angle() as f32,
                                                    waveform: format!("{:?}", e.waveform_type()),
                                                    prf_number: e.surveillance_prf_number(),
                                                    is_sails: e.is_sails_cut(),
                                                    is_mrle: e.is_mrle_cut(),
                                                    is_base_tilt: e.is_base_tilt_cut(),
                                                }
                                            }).collect();
                                            extracted_vcp = Some(ExtractedVcp {
                                                number: header.pattern_number(),
                                                elevations,
                                            });
                                        }
                                        MessageContents::DigitalRadarData(ref m) => {
                                            // Fallback: extract VCP number from radial volume data block
                                            if let Some(vol_block) = m.volume_data_block() {
                                                let raw = vol_block.volume_coverage_pattern_number();
                                                if raw > 0 {
                                                    extracted_vcp = Some(ExtractedVcp {
                                                        number: raw,
                                                        elevations: Vec::new(),
                                                    });
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                // Consume to convert to radials
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
                let elevation_numbers = extract_elevation_numbers(&radials);
                let arr = js_sys::Array::new();
                for &e in &elevation_numbers {
                    arr.push(&wasm_bindgen::JsValue::from(e));
                }
                js_sys::Reflect::set(
                    &elevation_map,
                    &wasm_bindgen::JsValue::from(record_id),
                    &arr,
                )
                .ok();
                all_radials.extend(radials);
            }
        }
        let phase1_ms = t_decode.elapsed().as_secs_f64() * 1000.0;

        // Build sweep metadata by grouping radials by elevation_number
        let sweeps = build_sweep_meta_from_radials(&radial_metas);
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
        use std::collections::HashMap;
        use crate::nexrad::record_decode::extract_sweep_data_from_sorted;

        let products = [
            Product::Reflectivity,
            Product::Velocity,
            Product::SpectrumWidth,
            Product::DifferentialReflectivity,
            Product::CorrelationCoefficient,
            Product::DifferentialPhase,
        ];
        let product_names = [
            "reflectivity",
            "velocity",
            "spectrum_width",
            "differential_reflectivity",
            "correlation_coefficient",
            "differential_phase",
        ];

        // Pre-group radials by elevation in ONE pass (vs 138 full-array scans)
        let mut by_elevation: HashMap<u8, Vec<&::nexrad::model::data::Radial>> = HashMap::new();
        for radial in &all_radials {
            by_elevation
                .entry(radial.elevation_number())
                .or_default()
                .push(radial);
        }

        // Sort each group by azimuth ONCE (vs 6 times per elevation)
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
                for (product, product_name) in products.iter().zip(product_names.iter()) {
                    if let Some(sweep) =
                        extract_sweep_data_from_sorted(sorted_radials, *product)
                    {
                        let key = SweepDataKey::new(scan_key.clone(), elev_num, *product_name);
                        let bytes = sweep.to_bytes();
                        sweep_blobs.push((key.to_storage_key(), bytes));
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

        // --- Phase 3: Store sweep blobs in IDB ---
        let t_store = web_time::Instant::now();

        store.put_sweeps_batch(&sweep_blobs).await.map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to store sweeps batch: {}", e))
        })?;
        let store_ms = t_store.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "ingest: stored {} sweep blobs in IDB in {:.1}ms",
            sweep_count,
            store_ms,
        );

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
            file_name,
            total_ms,
            split_ms,
            decompress_ms_total,
            decode_only_ms,
            extract_ms,
            store_ms,
            index_ms,
            records.len(),
            all_radials.len(),
            elevation_numbers.len(),
            sweep_count,
            total_sweep_bytes as f64 / (1024.0 * 1024.0),
        );

        // Build response object
        let result = js_sys::Object::new();
        js_sys::Reflect::set(
            &result,
            &"recordsStored".into(),
            &wasm_bindgen::JsValue::from(sweep_count),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"scanKey".into(),
            &wasm_bindgen::JsValue::from_str(&scan_key.to_storage_key()),
        )
        .ok();
        js_sys::Reflect::set(&result, &"elevationMap".into(), &elevation_map).ok();
        js_sys::Reflect::set(
            &result,
            &"totalMs".into(),
            &wasm_bindgen::JsValue::from(total_ms),
        )
        .ok();

        // Include sweep metadata as JSON so the main thread has it immediately
        let sweeps_json = serde_json::to_string(&sweeps).unwrap_or_else(|_| "[]".to_string());
        js_sys::Reflect::set(
            &result,
            &"sweepsJson".into(),
            &wasm_bindgen::JsValue::from_str(&sweeps_json),
        )
        .ok();

        // Include extracted VCP pattern as JSON
        if let Some(ref vcp) = extracted_vcp {
            let vcp_json = serde_json::to_string(vcp).unwrap_or_else(|_| "null".to_string());
            js_sys::Reflect::set(
                &result,
                &"vcpJson".into(),
                &wasm_bindgen::JsValue::from_str(&vcp_json),
            )
            .ok();
        }

        Ok(result.into())
    })
}

/// Build `SweepMeta` entries by grouping radial metadata by elevation number.
///
/// Each tuple is `(timestamp_ms, elevation_number, elevation_angle_degrees)`.
fn build_sweep_meta_from_radials(
    radial_metas: &[(i64, u8, f32)],
) -> Vec<crate::data::keys::SweepMeta> {
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
        .map(|(elev_num, acc)| crate::data::keys::SweepMeta {
            start: acc.min_ts_ms as f64 / 1000.0,
            end: acc.max_ts_ms as f64 / 1000.0,
            elevation: (acc.angle_sum / acc.count as f64) as f32,
            elevation_number: elev_num,
        })
        .collect()
}

/// Render a specific elevation from pre-computed sweep data in IndexedDB.
///
/// Called from the Web Worker. Fetches a single pre-computed sweep blob
/// and returns the data directly — no decoding or extraction needed.
///
/// Parameters: { scanKey: string, elevationNumber: number, product: string }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    worker_init_logger();
    wasm_bindgen_futures::future_to_promise(async move {
        use crate::data::keys::*;

        let t_total = web_time::Instant::now();

        // Extract parameters
        let scan_key_str = js_sys::Reflect::get(&params, &"scanKey".into())
            .ok()
            .and_then(|v| v.as_string())
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Missing scanKey"))?;

        let elevation_number = js_sys::Reflect::get(&params, &"elevationNumber".into())
            .ok()
            .and_then(|v| v.as_f64())
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Missing elevationNumber"))?
            as u8;

        let product_str = js_sys::Reflect::get(&params, &"product".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "reflectivity".to_string());

        let scan_key = ScanKey::from_storage_key(&scan_key_str)
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Invalid scanKey format"))?;

        // Reuse cached IDB connection
        let store = worker_idb_store().await?;

        // Fetch raw IDB ArrayBuffer (no Rust-side copy)
        let t_fetch = web_time::Instant::now();
        let sweep_key = SweepDataKey::new(scan_key, elevation_number, &product_str);
        let blob_buffer = store
            .get_sweep_as_js(&sweep_key.to_storage_key())
            .await
            .map_err(|e| {
                wasm_bindgen::JsValue::from_str(&format!("Failed to fetch sweep: {}", e))
            })?
            .ok_or_else(|| {
                wasm_bindgen::JsValue::from_str(&format!(
                    "No pre-computed sweep for elev={} product={}",
                    elevation_number, product_str
                ))
            })?;
        let fetch_ms = t_fetch.elapsed().as_secs_f64() * 1000.0;
        let blob_len = blob_buffer.byte_length();

        // Parse header only (48 bytes) — no array allocations
        let t_deser = web_time::Instant::now();
        let header_bytes = {
            let view = js_sys::Uint8Array::new_with_byte_offset_and_length(&blob_buffer, 0, 48);
            let mut buf = [0u8; 48];
            view.copy_to(&mut buf);
            buf
        };
        let header = parse_sweep_header(&header_bytes).map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to parse sweep header: {}", e))
        })?;

        // Validate full blob size
        let az = header.azimuth_count as usize;
        let gc = header.gate_count as usize;
        let expected = header.gate_values_offset as usize + az * gc * 4;
        if (blob_len as usize) < expected {
            return Err(wasm_bindgen::JsValue::from_str(&format!(
                "Sweep blob too small: {} < {} expected",
                blob_len, expected
            )));
        }
        let deser_ms = t_deser.elapsed().as_secs_f64() * 1000.0;

        // Zero-copy marshal: create typed array views over raw IDB ArrayBuffer,
        // then slice() to create independent transferable buffers.
        let t_marshal = web_time::Instant::now();

        let az_view = js_sys::Float32Array::new_with_byte_offset_and_length(
            &blob_buffer, header.azimuths_offset, header.azimuth_count,
        );
        let ts_view = js_sys::Float64Array::new_with_byte_offset_and_length(
            &blob_buffer, header.timestamps_offset, header.azimuth_count,
        );
        let ea_view = js_sys::Float32Array::new_with_byte_offset_and_length(
            &blob_buffer, header.elev_angles_offset, header.azimuth_count,
        );
        let gv_view = js_sys::Float32Array::new_with_byte_offset_and_length(
            &blob_buffer, header.gate_values_offset, header.azimuth_count * header.gate_count,
        );

        // slice() creates independent ArrayBuffers for postMessage transfer
        let az_buf = az_view.slice(0, header.azimuth_count).buffer();
        let ts_buf = ts_view.slice(0, header.azimuth_count).buffer();
        let ea_buf = ea_view.slice(0, header.azimuth_count).buffer();
        let val_buf = gv_view.slice(0, header.azimuth_count * header.gate_count).buffer();

        let result = js_sys::Object::new();
        js_sys::Reflect::set(&result, &"azimuths".into(), &az_buf).ok();
        js_sys::Reflect::set(&result, &"gateValues".into(), &val_buf).ok();
        js_sys::Reflect::set(&result, &"timestamps".into(), &ts_buf).ok();
        js_sys::Reflect::set(&result, &"elevationAngles".into(), &ea_buf).ok();
        js_sys::Reflect::set(&result, &"azimuthCount".into(), &wasm_bindgen::JsValue::from(header.azimuth_count)).ok();
        js_sys::Reflect::set(&result, &"gateCount".into(), &wasm_bindgen::JsValue::from(header.gate_count)).ok();
        js_sys::Reflect::set(&result, &"firstGateRangeKm".into(), &wasm_bindgen::JsValue::from(header.first_gate_range_km)).ok();
        js_sys::Reflect::set(&result, &"gateIntervalKm".into(), &wasm_bindgen::JsValue::from(header.gate_interval_km)).ok();
        js_sys::Reflect::set(&result, &"maxRangeKm".into(), &wasm_bindgen::JsValue::from(header.max_range_km)).ok();
        js_sys::Reflect::set(&result, &"product".into(), &wasm_bindgen::JsValue::from_str(&product_str)).ok();
        js_sys::Reflect::set(&result, &"radialCount".into(), &wasm_bindgen::JsValue::from(header.radial_count)).ok();
        js_sys::Reflect::set(&result, &"scale".into(), &wasm_bindgen::JsValue::from(header.scale as f64)).ok();
        js_sys::Reflect::set(&result, &"offset".into(), &wasm_bindgen::JsValue::from(header.offset as f64)).ok();

        let marshal_ms = t_marshal.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "render: elev={} {} {}x{} ({:.1}KB) in {:.1}ms | fetch {:.1} | deser {:.1} | marshal {:.1}",
            elevation_number,
            product_str,
            header.azimuth_count,
            header.gate_count,
            blob_len as f64 / 1024.0,
            total_ms,
            fetch_ms,
            deser_ms,
            marshal_ms,
        );

        // Timing fields
        js_sys::Reflect::set(&result, &"fetchMs".into(), &wasm_bindgen::JsValue::from(fetch_ms)).ok();
        js_sys::Reflect::set(&result, &"totalMs".into(), &wasm_bindgen::JsValue::from(total_ms)).ok();
        js_sys::Reflect::set(&result, &"marshalMs".into(), &wasm_bindgen::JsValue::from(marshal_ms)).ok();

        Ok(result.into())
    })
}

/// Entry point for the WASM application.
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub async fn start() {
    // Web Workers have no window — skip app initialization
    if web_sys::window().is_none() {
        return;
    }

    use eframe::wasm_bindgen::JsCast as _;

    // Redirect `log` messages to `console.log`:
    eframe::WebLogger::init(log::LevelFilter::Debug).ok();

    let web_options = eframe::WebOptions::default();

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .expect("No window")
            .document()
            .expect("No document");

        let canvas = document
            .get_element_by_id("app_canvas")
            .expect("Failed to find app_canvas")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("app_canvas was not a HtmlCanvasElement");

        let start_result = eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| Ok(Box::new(WorkbenchApp::new(cc)))),
            )
            .await;

        // Remove the loading text once the app has loaded:
        if let Some(loading_text) = document.get_element_by_id("loading_text") {
            match start_result {
                Ok(_) => {
                    loading_text.remove();
                }
                Err(e) => {
                    loading_text.set_inner_html(
                        "<p>The app has crashed. See the developer console for details.</p>",
                    );
                    panic!("Failed to start eframe: {e:?}");
                }
            }
        }
    });
}

/// Main application state and logic.
pub struct WorkbenchApp {
    /// Application state containing all sub-states
    state: AppState,

    /// Geographic layer data for map overlays
    geo_layers: geo::GeoLayerSet,

    /// Record-based data facade
    data_facade: DataFacade,

    /// Channel for async NEXRAD download operations
    download_channel: nexrad::DownloadChannel,

    /// Channel for async cache metadata loading
    cache_load_channel: nexrad::CacheLoadChannel,

    /// Cache for archive file listings (by site/date)
    archive_index: nexrad::ArchiveIndex,

    /// Currently loaded NEXRAD scan
    current_scan: Option<nexrad::CachedScan>,

    /// GPU renderer for radar data (None if GL not available).
    gpu_renderer: Option<std::sync::Arc<std::sync::Mutex<nexrad::RadarGpuRenderer>>>,

    /// GL context for uploading data to GPU textures.
    gpu_renderer_gl: Option<std::sync::Arc<glow::Context>>,

    /// Queue of files to download for selection download feature.
    /// Each entry is (date, file_name, timestamp).
    selection_download_queue: Vec<(chrono::NaiveDate, String, i64)>,

    /// Timestamp of the currently displayed scan (for detecting when to load a new scan)
    displayed_scan_timestamp: Option<i64>,

    /// Elevation number of the currently displayed sweep within the scan.
    /// Used to detect intra-scan sweep changes when scrubbing through a multi-sweep scan.
    displayed_sweep_elevation_number: Option<u8>,

    /// Previous site ID to detect site changes (for clearing volume ring)
    previous_site_id: String,

    /// Channel for real-time streaming
    realtime_channel: nexrad::RealtimeChannel,

    /// Web Worker for offloading expensive NEXRAD operations.
    /// None if the worker failed to initialize.
    decode_worker: Option<nexrad::DecodeWorker>,

    /// Scan key of the currently displayed scan (data storage format "SITE|TIMESTAMP_MS").
    /// Used to send render requests to the worker.
    current_render_scan_key: Option<String>,

    /// Available elevation numbers for the current scan (from ingest).
    available_elevation_numbers: Vec<u8>,

    /// Previous render parameters for change detection (scan_key, elev_num, product, render_mode).
    /// When any of these change, a new worker decode is sent.
    last_render_params: Option<(String, u8, String, crate::state::RenderMode)>,

    /// Monotonic instant of last URL push (for throttling to ~1/sec).
    last_url_push: web_time::Instant,

    /// Last-saved user preferences snapshot (for change detection).
    last_saved_preferences: state::UserPreferences,

    /// Transient state for the site selection modal.
    site_modal_state: ui::SiteModalState,
}

// Embed shapefile data at compile time
static STATES_SHP: &[u8] =
    include_bytes!("../assets/vectors/cb_2023_us_state_20m/cb_2023_us_state_20m.shp");
static STATES_DBF: &[u8] =
    include_bytes!("../assets/vectors/cb_2023_us_state_20m/cb_2023_us_state_20m.dbf");
static COUNTIES_SHP: &[u8] =
    include_bytes!("../assets/vectors/cb_2023_us_county_20m/cb_2023_us_county_20m.shp");
static COUNTIES_DBF: &[u8] =
    include_bytes!("../assets/vectors/cb_2023_us_county_20m/cb_2023_us_county_20m.dbf");

impl WorkbenchApp {
    /// Creates a new WorkbenchApp instance.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut geo_layers = geo::GeoLayerSet::new();

        // Load embedded geographic data
        if let Err(e) = geo_layers.load_layer_from_shapefile(
            geo::GeoLayerType::States,
            STATES_SHP,
            Some(STATES_DBF),
        ) {
            log::error!("Failed to load states layer: {}", e);
        }

        if let Err(e) = geo_layers.load_layer_from_shapefile(
            geo::GeoLayerType::Counties,
            COUNTIES_SHP,
            Some(COUNTIES_DBF),
        ) {
            log::error!("Failed to load counties layer: {}", e);
        }

        log::info!(
            "Loaded geo layers: {} states, {} counties",
            geo_layers
                .states
                .as_ref()
                .map(|l| l.features.len())
                .unwrap_or(0),
            geo_layers
                .counties
                .as_ref()
                .map(|l| l.features.len())
                .unwrap_or(0),
        );

        let mut state = AppState::new();

        // Apply URL parameters (site, time, lat/lon)
        let url_params = state::url_state::parse_from_url();
        if let Some(ref site) = url_params.site {
            state.viz_state.site_id = site.to_uppercase();
            if let Some(site_info) = data::sites::get_site(site) {
                state.viz_state.center_lat = site_info.lat;
                state.viz_state.center_lon = site_info.lon;
            }
            state.timeline_needs_refresh = true;
        }
        if let Some(lat) = url_params.lat {
            state.viz_state.center_lat = lat;
        }
        if let Some(lon) = url_params.lon {
            state.viz_state.center_lon = lon;
        }
        // Apply view state (zoom levels) before centering so the zoom is correct
        if let Some(mz) = url_params.view.mz {
            state.viz_state.zoom = mz;
        }
        if let Some(tz) = url_params.view.tz {
            state.playback_state.timeline_zoom = tz;
        }
        if let Some(ref product_code) = url_params.product {
            if let Some(product) = state::RadarProduct::from_short_code(product_code) {
                state.viz_state.product = product;
            }
        }
        if let Some(time) = url_params.time {
            state.playback_state.set_playback_position(time);
            // Center the timeline view on the restored playback position.
            // We don't know the actual panel pixel width yet, so use the same
            // assumed width (1000px) that PlaybackState constructors use.
            let view_width_secs = 1000.0 / state.playback_state.timeline_zoom;
            state.playback_state.timeline_view_start = time - view_width_secs / 2.0;
        }

        // First-launch detection: open site selection modal if no site in URL
        if url_params.site.is_none() {
            state.site_modal_open = true;
        }

        let initial_site_id = state.viz_state.site_id.clone();
        let data_facade = DataFacade::new();
        let cache_load_channel = nexrad::CacheLoadChannel::new();
        let download_channel = nexrad::DownloadChannel::new();
        let realtime_channel = nexrad::RealtimeChannel::with_stats(download_channel.stats());

        // Open the record cache database
        {
            let facade = data_facade.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = facade.open().await {
                    log::error!("Failed to open record cache: {}", e);
                } else {
                    log::info!("Opened record cache database");
                }
            });
        }

        let initial_prefs = state::UserPreferences::from_app_state(&state);

        // Create decode worker (offloads nexrad::load() to a Web Worker)
        let decode_worker = match nexrad::DecodeWorker::new(cc.egui_ctx.clone()) {
            Ok(w) => {
                log::info!("Decode worker created successfully");
                Some(w)
            }
            Err(e) => {
                log::warn!("Failed to create decode worker, using main thread: {}", e);
                None
            }
        };

        // Create GPU renderer for radar visualization
        let gpu_renderer_gl = cc.gl.clone();
        let gpu_renderer = cc.gl.as_ref().map(|gl| {
            let renderer = nexrad::RadarGpuRenderer::new(gl);
            log::info!("GPU radar renderer created");
            std::sync::Arc::new(std::sync::Mutex::new(renderer))
        });

        Self {
            state,
            geo_layers,
            data_facade,
            download_channel,
            cache_load_channel,
            archive_index: nexrad::ArchiveIndex::new(),
            current_scan: None,
            gpu_renderer,
            gpu_renderer_gl,
            selection_download_queue: Vec::new(),
            displayed_scan_timestamp: None,
            displayed_sweep_elevation_number: None,
            previous_site_id: initial_site_id,
            realtime_channel,
            decode_worker,
            current_render_scan_key: None,
            available_elevation_numbers: Vec::new(),
            last_render_params: None,
            last_url_push: web_time::Instant::now(),
            last_saved_preferences: initial_prefs,
            site_modal_state: ui::SiteModalState::default(),
        }
    }

    /// Loads geographic layer data from GeoJSON string.
    #[allow(dead_code)]
    pub fn load_geo_layer(
        &mut self,
        layer_type: geo::GeoLayerType,
        geojson_str: &str,
    ) -> Result<(), String> {
        self.geo_layers.load_layer(layer_type, geojson_str)
    }

    /// Process selection download: download scans in the selected time range serially.
    fn process_selection_download(&mut self, ctx: &egui::Context) {
        let site_id = self.state.viz_state.site_id.clone();

        // If we have items in the queue, try to download the next one
        if !self.selection_download_queue.is_empty() {
            // Check if current download is still in progress
            let (_, _, timestamp) = &self.selection_download_queue[0];
            if self
                .download_channel
                .is_download_pending(&site_id, *timestamp)
            {
                // Still downloading, wait
                return;
            }

            // Previous download finished, remove it from queue
            let _ = self.selection_download_queue.remove(0);

            // Start the next download if there are more items
            if !self.selection_download_queue.is_empty() {
                let (next_date, next_name, next_ts) = &self.selection_download_queue[0];
                self.state.status_message = format!(
                    "Downloading {} ({} remaining)",
                    next_name,
                    self.selection_download_queue.len()
                );
                self.download_channel.download_file(
                    ctx.clone(),
                    site_id.clone(),
                    *next_date,
                    next_name.clone(),
                    *next_ts,
                    self.data_facade.clone(),
                );
            } else {
                // All done
                self.state.download_selection_in_progress = false;
                self.state.status_message = "Selection download complete".to_string();
                log::info!("Selection download complete");
            }
            return;
        }

        // No queue - check if we should build one
        if !self.state.download_selection_requested {
            return;
        }
        self.state.download_selection_requested = false;

        // Get the selection range
        let Some((sel_start, sel_end)) = self.state.playback_state.selection_range() else {
            log::warn!("Download selection requested but no valid selection");
            return;
        };

        let sel_start_i64 = sel_start as i64;
        let sel_end_i64 = sel_end as i64;

        // Determine the date range
        let start_date = match chrono::DateTime::from_timestamp(sel_start_i64, 0) {
            Some(dt) => dt.date_naive(),
            None => return,
        };
        let end_date = match chrono::DateTime::from_timestamp(sel_end_i64, 0) {
            Some(dt) => dt.date_naive(),
            None => return,
        };

        log::info!(
            "Building download queue for selection: {} to {} ({} to {})",
            sel_start_i64,
            sel_end_i64,
            start_date,
            end_date
        );

        // Collect all files in the range from cached listings
        let mut files_to_download = Vec::new();
        let mut current_date = start_date;

        while current_date <= end_date {
            // Check if we have the listing for this date
            if let Some(listing) = self.archive_index.get(&site_id, &current_date) {
                // Find all files in the selection range
                for file in &listing.files {
                    if file.timestamp >= sel_start_i64 && file.timestamp <= sel_end_i64 {
                        // Check if already cached
                        let is_cached = self
                            .state
                            .radar_timeline
                            .scans
                            .iter()
                            .any(|s| (s.start_time as i64 - file.timestamp).abs() < 60);

                        if !is_cached {
                            files_to_download.push((
                                current_date,
                                file.name.clone(),
                                file.timestamp,
                            ));
                        }
                    }
                }
            } else {
                // Need to fetch the listing first
                if !self
                    .download_channel
                    .is_listing_pending(&site_id, &current_date)
                {
                    log::info!("Fetching listing for {}/{}", site_id, current_date);
                    self.download_channel
                        .fetch_listing(ctx.clone(), site_id.clone(), current_date);
                }
                // Re-trigger download selection once listing arrives
                self.state.download_selection_requested = true;
                self.state.status_message =
                    format!("Fetching archive listing for {}...", current_date);
                return;
            }

            current_date += chrono::Duration::days(1);
        }

        if files_to_download.is_empty() {
            self.state.status_message = "No new scans to download in selection".to_string();
            log::info!("No new scans to download in selection");
            return;
        }

        // Sort by timestamp
        files_to_download.sort_by_key(|(_, _, ts)| *ts);

        log::info!(
            "Queued {} files for download in selection",
            files_to_download.len()
        );

        // Start downloading
        self.state.download_selection_in_progress = true;
        self.selection_download_queue = files_to_download;

        // Kick off first download
        let (date, file_name, timestamp) = &self.selection_download_queue[0];
        self.state.status_message = format!(
            "Downloading {} ({} total)",
            file_name,
            self.selection_download_queue.len()
        );
        self.download_channel.download_file(
            ctx.clone(),
            site_id,
            *date,
            file_name.clone(),
            *timestamp,
            self.data_facade.clone(),
        );
    }

    /// Start live mode streaming for the current site.
    fn start_live_mode(&mut self, ctx: &egui::Context) {
        let site_id = self.state.viz_state.site_id.clone();
        log::info!("Starting live mode for site: {}", site_id);

        // Get current time
        let now = js_sys::Date::now() / 1000.0;

        // Initialize live mode state
        self.state.live_mode_state.start(now);
        self.state.playback_state.set_playback_position(now);
        self.state.playback_state.time_model.enable_realtime_lock();
        self.state.playback_state.playing = true;
        self.state.status_message = "Connecting to live stream...".to_string();

        // Start the realtime channel with DataFacade for record storage
        self.realtime_channel
            .start(ctx.clone(), site_id, self.data_facade.clone());
    }

    /// Find the best elevation number for the current target_elevation.
    ///
    /// If sweep metadata with angles is available, picks the number whose angle
    /// is closest to target_elevation. Otherwise falls back to the lowest available number.
    fn best_elevation_number(&self) -> u8 {
        // First try to match by angle using timeline sweep metadata
        let target = self.state.viz_state.target_elevation;
        if let Some(scan) = self
            .state
            .radar_timeline
            .find_recent_scan(self.state.playback_state.playback_position(), 15.0 * 60.0)
        {
            if !scan.sweeps.is_empty() {
                // Find sweep whose angle is closest to target
                if let Some(best) = scan.sweeps.iter().min_by(|a, b| {
                    (a.elevation - target)
                        .abs()
                        .partial_cmp(&(b.elevation - target).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    return best.elevation_number;
                }
            }
        }

        // Fallback: use lowest available elevation number
        self.available_elevation_numbers
            .first()
            .copied()
            .unwrap_or(1)
    }

    /// Find the best elevation number for a scan given the playback position.
    ///
    /// In FixedTilt mode, finds the most recent sweep at the target elevation
    /// whose start_time <= playback_ts. A scan may contain multiple sweeps at the
    /// same elevation (e.g. VCP 215 has 0.5° at elevation_number 1 and 3).
    fn best_elevation_at_playback(&self, scan: &crate::state::radar_data::Scan, playback_ts: f64) -> u8 {
        let target = self.state.viz_state.target_elevation;

        // Filter sweeps matching target elevation (within 0.15° tolerance)
        // then filter to those that have started (start_time <= playback_ts)
        // pick the one with the latest start_time (most recent instance)
        let matching = scan
            .sweeps
            .iter()
            .filter(|s| (s.elevation - target).abs() < 0.15)
            .filter(|s| s.start_time <= playback_ts)
            .max_by(|a, b| {
                a.start_time
                    .partial_cmp(&b.start_time)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        if let Some(sweep) = matching {
            return sweep.elevation_number;
        }

        // No matching sweep started yet — fall back to best_elevation_number()
        self.best_elevation_number()
    }

    /// Find the most recent sweep (any elevation) at or before the playback position.
    ///
    /// Used by MostRecent render mode to always show the latest available data
    /// regardless of elevation.
    fn most_recent_sweep_elevation(
        &self,
        scan: &crate::state::radar_data::Scan,
        playback_ts: f64,
    ) -> u8 {
        scan.sweeps
            .iter()
            .filter(|s| s.start_time <= playback_ts)
            .max_by(|a, b| {
                a.start_time
                    .partial_cmp(&b.start_time)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|s| s.elevation_number)
            .unwrap_or_else(|| self.best_elevation_number())
    }

    /// Update the canvas overlay text with sweep timing and elevation info.
    fn update_overlay_from_sweep(&mut self, start: f64, end: f64, elevation_deg: f32) {
        self.state.viz_state.elevation = format!("{:.1}\u{00B0}", elevation_deg);

        // Format midpoint timestamp as HH:MM:SS UTC
        let mid_ms = ((start + end) / 2.0) * 1000.0;
        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(mid_ms));
        self.state.viz_state.timestamp = format!(
            "{:02}:{:02}:{:02} UTC",
            date.get_utc_hours(),
            date.get_utc_minutes(),
            date.get_utc_seconds()
        );

        // Staleness = now - sweep end
        let staleness = js_sys::Date::now() / 1000.0 - end;
        self.state.viz_state.data_staleness_secs = if staleness >= 0.0 {
            Some(staleness)
        } else {
            None
        };
    }

    /// Send a decode request to the worker for the current scan + settings.
    fn request_worker_render(&mut self) {
        let Some(ref scan_key) = self.current_render_scan_key else {
            return;
        };
        if self.decode_worker.is_none() {
            return;
        }

        let elevation_number = self
            .displayed_sweep_elevation_number
            .unwrap_or_else(|| self.best_elevation_number());
        let product = self.state.viz_state.product.to_worker_string().to_string();

        let params = (
            scan_key.clone(),
            elevation_number,
            product.clone(),
            self.state.viz_state.render_mode,
        );

        // Skip if same as last request
        if self.last_render_params.as_ref() == Some(&params) {
            return;
        }

        log::info!(
            "Requesting worker decode: {} elev={} product={}",
            scan_key,
            elevation_number,
            product,
        );

        let scan_key = scan_key.clone();
        self.last_render_params = Some(params);
        self.decode_worker
            .as_mut()
            .unwrap()
            .render(scan_key, elevation_number, product);
    }

    /// Stop live mode streaming.
    #[allow(dead_code)] // Called from UI when user stops live mode
    fn stop_live_mode(&mut self, reason: state::LiveExitReason) {
        log::info!("Stopping live mode: {:?}", reason);

        self.state.live_mode_state.stop(reason);
        self.realtime_channel.stop();

        self.state.status_message = self
            .state
            .live_mode_state
            .last_exit_reason
            .map(|r| r.message().to_string())
            .unwrap_or_default();
    }

    /// Handle a realtime streaming result.
    fn handle_realtime_result(&mut self, result: nexrad::RealtimeResult, _ctx: &egui::Context) {
        // Get current time
        let now = js_sys::Date::now() / 1000.0;

        match result {
            nexrad::RealtimeResult::Started { site_id } => {
                log::info!("Realtime streaming started for site: {}", site_id);
                self.state.live_mode_state.handle_streaming_started(now);
                self.state.status_message = format!("Live: connected to {}", site_id);
            }
            nexrad::RealtimeResult::ChunkReceived {
                chunks_in_volume,
                time_until_next,
                is_volume_end,
                fetch_latency_ms,
            } => {
                self.state
                    .session_stats
                    .record_fetch_latency(fetch_latency_ms);
                log::debug!(
                    "Chunk received: {} in volume, is_end={}",
                    chunks_in_volume,
                    is_volume_end
                );
                self.state.live_mode_state.handle_realtime_chunk(
                    chunks_in_volume,
                    time_until_next,
                    is_volume_end,
                    now,
                );
            }
            nexrad::RealtimeResult::VolumeComplete { data, timestamp } => {
                log::info!(
                    "Volume complete: {} bytes, timestamp={}",
                    data.len(),
                    timestamp
                );
                self.state.live_mode_state.handle_volume_complete(now);
                self.state.status_message = format!("Live: received volume ({} bytes)", data.len());

                // Send raw bytes to worker for ingest (split + store + probe).
                // The worker stores records in IDB and returns metadata.
                // The Ingested handler then triggers a render request.
                let site_id = self.state.viz_state.site_id.clone();
                let file_name = format!("live_{}_{}.nexrad", site_id, timestamp);
                if let Some(ref mut worker) = self.decode_worker {
                    worker.ingest(data, site_id, timestamp, file_name, 0.0);
                }
            }
            nexrad::RealtimeResult::Error(msg) => {
                log::error!("Realtime streaming error: {}", msg);
                self.state.live_mode_state.set_error(msg.clone());
                self.state.status_message = format!("Live error: {}", msg);
            }
        }
    }
}

impl eframe::App for WorkbenchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Record frame time for FPS meter
        let dt = ctx.input(|i| i.stable_dt);
        self.state.session_stats.record_frame_time(dt);

        // Resolve theme and apply egui visuals
        self.state.is_dark = self.state.theme_mode.is_dark();
        if self.state.is_dark {
            ctx.set_visuals(egui::Visuals::dark());
        } else {
            ctx.set_visuals(egui::Visuals::light());
        }

        // Run storm cell detection on demand when toggled on with existing data
        if self.state.storm_cells_visible && self.state.detected_storm_cells.is_empty() {
            if let Some(ref renderer) = self.gpu_renderer {
                if let Ok(r) = renderer.lock() {
                    if r.has_data() {
                        self.state.detected_storm_cells = r.detect_storm_cells(
                            self.state.viz_state.center_lat,
                            self.state.viz_state.center_lon,
                            35.0,
                        );
                    }
                }
            }
        }
        // Clear cached cells when toggle is off
        if !self.state.storm_cells_visible && !self.state.detected_storm_cells.is_empty() {
            self.state.detected_storm_cells.clear();
        }

        // Detect site changes and clear volume ring
        if self.state.viz_state.site_id != self.previous_site_id {
            log::info!(
                "Site changed from {} to {}",
                self.previous_site_id,
                self.state.viz_state.site_id
            );
            if let Some(ref renderer) = self.gpu_renderer {
                if let Ok(mut r) = renderer.lock() {
                    r.clear_data();
                }
            }
            self.displayed_scan_timestamp = None;
            self.displayed_sweep_elevation_number = None;
            self.state.displayed_scan_timestamp = None;
            self.state.displayed_sweep_elevation_number = None;
            self.previous_site_id = self.state.viz_state.site_id.clone();
        }

        // Handle cache clear request
        if self.state.clear_cache_requested && !self.cache_load_channel.is_loading() {
            self.state.clear_cache_requested = false;
            self.cache_load_channel
                .clear_cache(ctx.clone(), self.data_facade.clone());
        }

        // Handle wipe-all request: clear IndexedDB + localStorage, then reload
        if self.state.wipe_all_requested {
            self.state.wipe_all_requested = false;
            let facade = self.data_facade.clone();
            wasm_bindgen_futures::spawn_local(async move {
                // Clear all IndexedDB stores
                if let Err(e) = facade.clear_all().await {
                    log::error!("Failed to clear IndexedDB: {}", e);
                }
                // Clear localStorage and reload
                if let Some(window) = web_sys::window() {
                    if let Ok(Some(storage)) = window.local_storage() {
                        let _ = storage.clear();
                    }
                    let _ = window.location().reload();
                }
            });
        }

        // Check if timeline needs to be refreshed from cache
        if self.state.timeline_needs_refresh && !self.cache_load_channel.is_loading() {
            self.state.timeline_needs_refresh = false;
            self.cache_load_channel.load_site_timeline(
                ctx.clone(),
                self.data_facade.clone(),
                self.state.viz_state.site_id.clone(),
            );
        }

        // Check for completed cache load operations
        if let Some(result) = self.cache_load_channel.try_recv() {
            match result {
                nexrad::CacheLoadResult::Success {
                    site_id,
                    metadata,
                    total_cache_size,
                } => {
                    log::info!(
                        "Timeline loaded from cache: {} scan(s) for site {}",
                        metadata.len(),
                        site_id
                    );

                    // Update cache size in session stats
                    self.state.session_stats.cache_size_bytes = total_cache_size;

                    // Build timeline from metadata
                    self.state.radar_timeline = state::RadarTimeline::from_metadata(metadata);

                    // Get time ranges (may be non-contiguous)
                    let ranges = self.state.radar_timeline.time_ranges();
                    if !ranges.is_empty() {
                        // Set overall bounds from first to last
                        let start = ranges.first().unwrap().start;
                        let end = ranges.last().unwrap().end;
                        self.state.playback_state.data_start_timestamp = Some(start as i64);
                        self.state.playback_state.data_end_timestamp = Some(end as i64);

                        // Position playback at the end of the most recent range
                        let most_recent_end = ranges.last().unwrap().end;

                        // If current position is not within any range, move to most recent
                        // and center the timeline view on the data
                        let ts = self.state.playback_state.playback_position();
                        let in_any_range = ranges.iter().any(|r| r.contains(ts));
                        if !in_any_range {
                            self.state
                                .playback_state
                                .set_playback_position(most_recent_end);

                            // Center the timeline view on the data so it's visible
                            let view_width_secs = 1000.0 / self.state.playback_state.timeline_zoom;
                            self.state.playback_state.timeline_view_start =
                                most_recent_end - view_width_secs / 2.0;
                        }

                        log::info!("Timeline has {} contiguous range(s)", ranges.len());
                    }
                }
                nexrad::CacheLoadResult::Error(msg) => {
                    log::error!("Cache load failed: {}", msg);
                }
            }
        }

        // Handle eviction check request (after storage operations)
        if self.state.check_eviction_requested {
            self.state.check_eviction_requested = false;
            let facade = self.data_facade.clone();
            let quota = self.state.storage_settings.quota_bytes;
            let target = self.state.storage_settings.eviction_target_bytes;
            let ctx_clone = ctx.clone();

            wasm_bindgen_futures::spawn_local(async move {
                match facade.check_and_evict(quota, target).await {
                    Ok((evicted, count)) => {
                        if evicted {
                            log::info!("Eviction complete: removed {} scans", count);
                        }
                    }
                    Err(e) => {
                        log::error!("Eviction check failed: {}", e);
                    }
                }
                ctx_clone.request_repaint();
            });
        }

        // Check for completed Web Worker operations
        if let Some(ref mut worker) = self.decode_worker {
            for outcome in worker.try_recv() {
                match outcome {
                    nexrad::WorkerOutcome::Ingested(result) => {
                        log::info!(
                            "Ingest complete: {} ({} records, {} elevations, {} sweeps, {:.0}ms, fetch: {:.0}ms)",
                            result.scan_key,
                            result.records_stored,
                            result.elevation_numbers.len(),
                            result.sweeps.len(),
                            result.total_ms,
                            result.context.fetch_latency_ms,
                        );

                        self.state
                            .session_stats
                            .record_fetch_latency(result.context.fetch_latency_ms);
                        self.state.session_stats.record_store_time(result.total_ms);

                        // Track the scan for render requests
                        self.current_render_scan_key = Some(result.scan_key.clone());
                        self.available_elevation_numbers = result.elevation_numbers;
                        self.displayed_scan_timestamp = Some(result.context.timestamp_secs);
                        self.displayed_sweep_elevation_number = None;
                        self.state.displayed_scan_timestamp = Some(result.context.timestamp_secs);
                        self.state.displayed_sweep_elevation_number = None;
                        self.state
                            .playback_state
                            .set_playback_position(result.context.timestamp_secs as f64);

                        // Refresh timeline to include the new scan (sweeps
                        // were persisted to IDB during ingest and will be
                        // loaded by from_metadata on the next refresh).
                        self.state.timeline_needs_refresh = true;

                        // Request eviction check
                        self.state.check_eviction_requested = true;

                        // Clear last render params to force a fresh render
                        self.last_render_params = None;

                        // Trigger render for the ingested scan
                        self.request_worker_render();
                    }
                    nexrad::WorkerOutcome::Decoded(result) => {
                        log::info!(
                            "Decode complete: {}x{} (az x gates), {} radials, product={}, {:.0}ms",
                            result.azimuth_count,
                            result.gate_count,
                            result.radial_count,
                            result.product,
                            result.fetch_ms,
                        );

                        self.state
                            .session_stats
                            .record_render_time(result.fetch_ms);

                        // Upload decoded data to GPU renderer
                        if let (Some(ref renderer), Some(ref gl)) =
                            (&self.gpu_renderer, &self.gpu_renderer_gl)
                        {
                            if let Ok(mut r) = renderer.lock() {
                                r.update_data(
                                    gl,
                                    &result.azimuths,
                                    &result.gate_values,
                                    result.azimuth_count,
                                    result.gate_count,
                                    result.first_gate_range_km,
                                    result.gate_interval_km,
                                    result.max_range_km,
                                    result.offset,
                                    result.scale,
                                );
                                r.update_color_table(gl, &result.product);

                                // Run storm cell detection if enabled
                                if self.state.storm_cells_visible {
                                    self.state.detected_storm_cells = r.detect_storm_cells(
                                        self.state.viz_state.center_lat,
                                        self.state.viz_state.center_lon,
                                        35.0, // threshold dBZ
                                    );
                                }
                            }
                        }

                        // Refine canvas overlay with precise decoded data
                        if !result.timestamps.is_empty() {
                            let start = result
                                .timestamps
                                .iter()
                                .copied()
                                .fold(f64::INFINITY, f64::min);
                            let end = result
                                .timestamps
                                .iter()
                                .copied()
                                .fold(f64::NEG_INFINITY, f64::max);
                            let mean_elev = if !result.elevation_angles.is_empty() {
                                result.elevation_angles.iter().sum::<f32>()
                                    / result.elevation_angles.len() as f32
                            } else {
                                self.state.viz_state.target_elevation
                            };
                            self.update_overlay_from_sweep(start, end, mean_elev);
                        }
                    }
                    nexrad::WorkerOutcome::WorkerError { id, message } => {
                        log::error!("Worker error (request {}): {}", id, message);
                        self.state.status_message = format!("Worker error: {}", message);
                    }
                }
            }
        }

        // Check for completed NEXRAD download operations
        if let Some(result) = self.download_channel.try_recv() {
            self.state.download_in_progress = false;
            // Extract scan and timing info from result
            let (scan_opt, is_cache_hit) = match &result {
                nexrad::DownloadResult::Success {
                    scan,
                    fetch_latency_ms,
                    decode_latency_ms,
                } => {
                    self.state
                        .session_stats
                        .record_fetch_latency(*fetch_latency_ms);
                    self.state
                        .session_stats
                        .record_store_time(*decode_latency_ms);
                    (Some(scan), false)
                }
                nexrad::DownloadResult::CacheHit(scan) => (Some(scan), true),
                _ => (None, false),
            };

            if let Some(scan) = scan_opt {
                let fetch_latency = match &result {
                    nexrad::DownloadResult::Success {
                        fetch_latency_ms, ..
                    } => *fetch_latency_ms,
                    _ => 0.0,
                };

                if is_cache_hit {
                    self.state.status_message = format!("Loaded from cache: {}", scan.file_name);

                    // Cache hit: records already in IDB. Send render request directly.
                    let scan_key = data::ScanKey::from_secs(&scan.key.site_id, scan.key.timestamp);
                    self.current_render_scan_key = Some(scan_key.to_storage_key());
                    self.displayed_scan_timestamp = Some(scan.key.timestamp);
                    self.displayed_sweep_elevation_number = None;
                    self.state.displayed_scan_timestamp = Some(scan.key.timestamp);
                    self.state.displayed_sweep_elevation_number = None;
                    self.state
                        .playback_state
                        .set_playback_position(scan.key.timestamp as f64);
                    self.last_render_params = None; // Force fresh render
                    self.request_worker_render();
                } else {
                    self.state.status_message =
                        format!("Downloaded: {} ({} bytes)", scan.file_name, scan.data.len());

                    // Fresh download: send raw bytes to worker for ingest.
                    // Worker splits records, probes elevations, stores in IDB,
                    // then returns metadata. We render on the Ingested callback.
                    if let Some(ref mut worker) = self.decode_worker {
                        worker.ingest(
                            scan.data.clone(),
                            scan.key.site_id.clone(),
                            scan.key.timestamp,
                            scan.file_name.clone(),
                            fetch_latency,
                        );
                    }
                }

                self.current_scan = Some(scan.clone());

                // Refresh timeline to show the new/loaded scan
                self.state.timeline_needs_refresh = true;
            }

            match &result {
                nexrad::DownloadResult::Error(msg) => {
                    self.state.status_message = format!("Download failed: {}", msg);
                    log::error!("Download failed: {}", msg);
                }
                nexrad::DownloadResult::Progress(current, total) => {
                    self.state.status_message =
                        format!("Downloading: {} / {} bytes", current, total);
                }
                _ => {}
            }
        }

        // Check for completed archive listing operations
        if let Some(result) = self.download_channel.try_recv_listing() {
            match result {
                nexrad::ListingResult::Success {
                    site_id,
                    date,
                    listing,
                } => {
                    log::info!(
                        "Archive listing received: {} files for {}/{}",
                        listing.files.len(),
                        site_id,
                        date
                    );
                    self.archive_index.insert(&site_id, date, listing);
                }
                nexrad::ListingResult::Error(msg) => {
                    log::error!("Listing request failed: {}", msg);
                }
            }
        }

        // Process selection download queue
        if self.state.download_selection_requested || !self.selection_download_queue.is_empty() {
            self.process_selection_download(ctx);
        }

        // Handle realtime streaming results
        while let Some(result) = self.realtime_channel.try_recv() {
            self.handle_realtime_result(result, ctx);
        }

        // Stop realtime channel if live mode was stopped by UI
        if !self.state.live_mode_state.is_active() && self.realtime_channel.is_active() {
            log::info!("Stopping realtime channel (live mode ended)");
            self.realtime_channel.stop();
        }

        // Update live mode countdown from realtime channel
        if self.state.live_mode_state.is_active() {
            if let Some(duration) = self.realtime_channel.time_until_next() {
                let now = js_sys::Date::now() / 1000.0;

                self.state.live_mode_state.next_chunk_expected_at =
                    Some(now + duration.as_secs_f64());
            }
        }

        // Handle start live mode request from UI
        if self.state.start_live_requested {
            self.state.start_live_requested = false;
            self.start_live_mode(ctx);
        }

        // Auto-load scan when scrubbing: find the most recent scan within 15 minutes.
        // In the worker architecture, this sends a render request directly —
        // the worker reads records from IDB, decodes the target elevation, and renders.
        //
        // In FixedTilt mode, we also detect intra-scan sweep changes: a scan may
        // contain multiple sweeps at the target elevation (e.g. VCP 215 has 0.5°
        // at both elevation_number 1 and 3). As playback advances past a new
        // sweep's start_time, we re-render with that sweep's elevation_number.
        const MAX_SCAN_AGE_SECS: f64 = 15.0 * 60.0;
        {
            let playback_ts = self.state.playback_state.playback_position();

            // Extract scrub decision data from the immutable borrow of radar_timeline
            let scrub_action = self
                .state
                .radar_timeline
                .find_recent_scan(playback_ts, MAX_SCAN_AGE_SECS)
                .map(|scan| {
                    let scan_ts = scan.start_time as i64;
                    let target_elev_num = match self.state.viz_state.render_mode {
                        crate::state::RenderMode::FixedTilt => {
                            self.best_elevation_at_playback(scan, playback_ts)
                        }
                        crate::state::RenderMode::MostRecent => {
                            self.most_recent_sweep_elevation(scan, playback_ts)
                        }
                    };

                    let needs_new_scan = match self.displayed_scan_timestamp {
                        Some(displayed) => displayed != scan_ts,
                        None => true,
                    };
                    let needs_new_sweep = !needs_new_scan
                        && self.displayed_sweep_elevation_number != Some(target_elev_num);

                    // Capture overlay data from the matching sweep
                    let sweep_overlay = scan
                        .sweeps
                        .iter()
                        .find(|s| s.elevation_number == target_elev_num)
                        .map(|s| (s.start_time, s.end_time, s.elevation));

                    (scan_ts, target_elev_num, needs_new_scan, needs_new_sweep, sweep_overlay)
                });

            if let Some((scan_ts, target_elev_num, needs_new_scan, needs_new_sweep, sweep_overlay)) =
                scrub_action
            {
                if (needs_new_scan || needs_new_sweep) && self.decode_worker.is_some() {
                    if needs_new_scan {
                        log::debug!(
                            "Scrubbing: new scan at {} elev={} (playback at {})",
                            scan_ts,
                            target_elev_num,
                            playback_ts as i64
                        );
                    } else {
                        log::debug!(
                            "Scrubbing: new sweep elev_num={} within scan {} (playback at {})",
                            target_elev_num,
                            scan_ts,
                            playback_ts as i64
                        );
                    }

                    // Update canvas overlay from sweep metadata
                    if let Some((start, end, elev)) = sweep_overlay {
                        self.update_overlay_from_sweep(start, end, elev);
                    }

                    // Build scan key in data storage format: "SITE|TIMESTAMP_MS"
                    let scan_key = data::ScanKey::from_secs(&self.state.viz_state.site_id, scan_ts);
                    self.current_render_scan_key = Some(scan_key.to_storage_key());
                    self.displayed_scan_timestamp = Some(scan_ts);
                    self.displayed_sweep_elevation_number = Some(target_elev_num);
                    self.state.displayed_scan_timestamp = Some(scan_ts);
                    self.state.displayed_sweep_elevation_number = Some(target_elev_num);
                    self.last_render_params = None; // Force fresh render
                    self.request_worker_render();
                }
            }
        }

        // Detect elevation/product changes and trigger worker re-render.
        // If the user changes these settings and we have a current scan, we need
        // a new render from the worker.
        if self.current_render_scan_key.is_some() && self.decode_worker.is_some() {
            self.request_worker_render();
        }

        // Update session stats from live network statistics
        let network_stats = self.download_channel.stats();
        self.state
            .session_stats
            .update_from_network_stats(&network_stats);

        // Push current state to URL (throttled to once per second)
        {
            let now = web_time::Instant::now();
            if now.duration_since(self.last_url_push).as_secs_f64() >= 1.0 {
                self.last_url_push = now;
                let view = state::url_state::ViewState {
                    mz: Some(self.state.viz_state.zoom),
                    tz: Some(self.state.playback_state.timeline_zoom),
                };
                state::url_state::push_to_url(
                    &self.state.viz_state.site_id,
                    self.state.playback_state.playback_position(),
                    self.state.viz_state.product.short_code(),
                    self.state.viz_state.center_lat,
                    self.state.viz_state.center_lon,
                    &view,
                );

                // Save user preferences if changed (piggyback on URL throttle)
                let current_prefs = state::UserPreferences::from_app_state(&self.state);
                if current_prefs != self.last_saved_preferences {
                    current_prefs.save();
                    self.last_saved_preferences = current_prefs;
                }
            }
        }

        // Render UI panels in the correct order for egui layout
        // Side and top/bottom panels must be rendered before CentralPanel
        ui::render_top_bar(ctx, &mut self.state);
        ui::render_bottom_panel(ctx, &mut self.state);
        ui::render_left_panel(
            ctx,
            &mut self.state,
            &self.download_channel,
            &self.data_facade,
        );
        ui::render_right_panel(ctx, &mut self.state);

        // Render canvas with GPU-based radar rendering
        ui::render_canvas_with_geo(
            ctx,
            &mut self.state,
            Some(&self.geo_layers),
            self.gpu_renderer.as_ref(),
        );

        // Process keyboard shortcuts
        ui::handle_shortcuts(ctx, &mut self.state);

        // Render overlays (on top of everything)
        ui::render_site_modal(ctx, &mut self.state, &mut self.site_modal_state);
        ui::render_shortcuts_help(ctx, &mut self.state);
        ui::render_wipe_modal(ctx, &mut self.state);
    }
}
