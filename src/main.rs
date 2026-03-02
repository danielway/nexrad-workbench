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
    wasm_bindgen_futures::future_to_promise(async move {
        use crate::data::indexeddb::IndexedDbRecordStore;
        use crate::data::keys::*;
        use crate::nexrad::{extract_elevation_numbers, probe_record_elevations};

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

        // Split into LDM records
        let t_split = web_time::Instant::now();
        let file = nexrad_data::volume::File::new(data);
        let records = file.records().map_err(|e| {
            wasm_bindgen::JsValue::from_str(&format!("Failed to split archive: {}", e))
        })?;
        let split_ms = t_split.elapsed().as_secs_f64() * 1000.0;

        if records.is_empty() {
            return Err(wasm_bindgen::JsValue::from_str("No records found"));
        }

        // Open IDB from worker context
        let store = IndexedDbRecordStore::new();
        store
            .open()
            .await
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Failed to open IDB: {}", e)))?;

        let scan_start = UnixMillis::from_secs(timestamp_secs);
        let scan_key = ScanKey::new(site_id.as_str(), scan_start);

        // Decompress, probe elevation metadata, and store decompressed records.
        // Records are stored decompressed so the render path skips bzip2 entirely.
        let t_store = web_time::Instant::now();
        let mut stored = 0u32;
        let mut decompress_ms = 0.0f64;
        let elevation_map = js_sys::Object::new();

        for (record_id, record) in records.iter().enumerate() {
            let record_id = record_id as u32;
            let record_key = RecordKey::new(scan_key.clone(), record_id);

            // Decompress once: keep the bytes for storage and extract elevations
            let (store_bytes, elevation_numbers) = if record.compressed() {
                let t_decompress = web_time::Instant::now();
                let decompressed = record.decompress().map_err(|e| {
                    wasm_bindgen::JsValue::from_str(&format!(
                        "Failed to decompress record {}: {}",
                        record_id, e
                    ))
                })?;
                decompress_ms += t_decompress.elapsed().as_secs_f64() * 1000.0;

                let bytes = decompressed.data().to_vec();
                let elevs = match decompressed.radials() {
                    Ok(radials) => Some(extract_elevation_numbers(&radials)),
                    Err(_) => None,
                };
                (bytes, elevs)
            } else {
                // Legacy CTM — already uncompressed
                let bytes = record.data().to_vec();
                let elevs = probe_record_elevations(record.data()).ok();
                (bytes, elevs)
            };

            let blob = RecordBlob::new(record_key.clone(), store_bytes);

            // First record typically contains VCP metadata
            let has_vcp = record_id == 0;

            let meta = RecordIndexEntry::new(record_key, blob.data.len() as u32)
                .with_vcp(has_vcp)
                .with_elevations(elevation_numbers.clone());

            let outcome = store.put_record(&blob, meta).await.map_err(|e| {
                wasm_bindgen::JsValue::from_str(&format!("Failed to store record: {}", e))
            })?;

            if outcome.inserted {
                stored += 1;
            }

            // Add to elevation map for the response
            if let Some(ref elevs) = elevation_numbers {
                let arr = js_sys::Array::new();
                for &e in elevs {
                    arr.push(&wasm_bindgen::JsValue::from(e));
                }
                js_sys::Reflect::set(
                    &elevation_map,
                    &wasm_bindgen::JsValue::from(record_id),
                    &arr,
                )
                .ok();
            }
        }
        let store_ms = t_store.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "worker_ingest: {} ({} records) in {:.0}ms (split: {:.0}ms, decompress: {:.0}ms, store: {:.0}ms)",
            file_name,
            stored,
            total_ms,
            split_ms,
            decompress_ms,
            store_ms,
        );

        // Build response object
        let result = js_sys::Object::new();
        js_sys::Reflect::set(
            &result,
            &"recordsStored".into(),
            &wasm_bindgen::JsValue::from(stored),
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

        Ok(result.into())
    })
}

/// Render a specific elevation from a scan stored in IndexedDB.
///
/// Called from the Web Worker. Returns a Promise that resolves to a JS object with:
///   { imageData: ArrayBuffer, width, height, renderTimeMs, radialCount }
///
/// Parameters are passed as a JS object:
///   { scanKey: string, elevationNumber: number, product: string, interpolation: string }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn worker_render(params: wasm_bindgen::JsValue) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        use crate::data::indexeddb::IndexedDbRecordStore;
        use crate::data::keys::*;
        use crate::nexrad::record_decode::decode_record_to_radials_timed;
        use nexrad_render::{
            default_color_scale, render_sweep, Interpolation, Product, RenderOptions,
        };

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

        let interpolation_str = js_sys::Reflect::get(&params, &"interpolation".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "nearest".to_string());

        let scan_key = ScanKey::from_storage_key(&scan_key_str)
            .ok_or_else(|| wasm_bindgen::JsValue::from_str("Invalid scanKey format"))?;

        let product = match product_str.as_str() {
            "reflectivity" => Product::Reflectivity,
            "velocity" => Product::Velocity,
            "spectrum_width" => Product::SpectrumWidth,
            "differential_reflectivity" => Product::DifferentialReflectivity,
            "differential_phase" => Product::DifferentialPhase,
            "correlation_coefficient" => Product::CorrelationCoefficient,
            _ => Product::Reflectivity,
        };

        let interpolation = match interpolation_str.as_str() {
            "bilinear" => Interpolation::Bilinear,
            _ => Interpolation::Nearest,
        };

        // Open IDB and fetch record entries
        let t_idb_open = web_time::Instant::now();
        let store = IndexedDbRecordStore::new();
        store
            .open()
            .await
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Failed to open IDB: {}", e)))?;
        let idb_open_ms = t_idb_open.elapsed().as_secs_f64() * 1000.0;

        let t_list = web_time::Instant::now();
        let entries = store
            .list_record_entries_for_scan(&scan_key)
            .await
            .map_err(|e| {
                wasm_bindgen::JsValue::from_str(&format!("Failed to list records: {}", e))
            })?;

        // Filter to records matching the target elevation
        let matching_keys: Vec<RecordKey> = entries
            .iter()
            .filter(|entry| {
                entry
                    .elevation_numbers
                    .as_ref()
                    .map(|nums| nums.contains(&elevation_number))
                    .unwrap_or(true) // Include records without metadata (fallback)
            })
            .map(|entry| entry.key.clone())
            .collect();
        let list_ms = t_list.elapsed().as_secs_f64() * 1000.0;

        log::info!(
            "worker_render fetch: {} total entries, {} match elev {} (idb_open: {:.0}ms, list: {:.0}ms)",
            entries.len(),
            matching_keys.len(),
            elevation_number,
            idb_open_ms,
            list_ms,
        );

        if matching_keys.is_empty() {
            return Err(wasm_bindgen::JsValue::from_str(
                "No records found for target elevation",
            ));
        }

        // Fetch matching record blobs, decompress, and decode — with sub-timings
        let mut all_radials: Vec<::nexrad::model::data::Radial> = Vec::new();
        let mut blob_bytes = 0u64;
        let mut idb_fetch_ms = 0.0f64;
        let mut decompress_ms = 0.0f64;
        let mut decode_ms = 0.0f64;
        for key in &matching_keys {
            let t_fetch = web_time::Instant::now();
            let blob_opt = store.get_record(key).await;
            idb_fetch_ms += t_fetch.elapsed().as_secs_f64() * 1000.0;

            if let Ok(Some(blob)) = blob_opt {
                blob_bytes += blob.data.len() as u64;
                match decode_record_to_radials_timed(&blob.data) {
                    Ok((radials, timings)) => {
                        decompress_ms += timings.decompress_ms;
                        decode_ms += timings.decode_ms;
                        all_radials.extend(radials);
                    }
                    Err(e) => log::warn!("Failed to decode record {}: {}", key, e),
                }
            }
        }
        let blob_fetch_ms = idb_fetch_ms + decompress_ms + decode_ms;
        let fetch_ms = idb_open_ms + list_ms + blob_fetch_ms;

        // Filter radials to target elevation (in case records contain multiple elevations)
        let target_radials: Vec<_> = all_radials
            .into_iter()
            .filter(|r| r.elevation_number() == elevation_number)
            .collect();

        let radial_count = target_radials.len();

        log::info!(
            "worker_render decode: {} records → {} radials ({} bytes) in {:.0}ms (idb_fetch: {:.0}ms, decompress: {:.0}ms, decode: {:.0}ms)",
            matching_keys.len(),
            radial_count,
            blob_bytes,
            blob_fetch_ms,
            idb_fetch_ms,
            decompress_ms,
            decode_ms,
        );

        if target_radials.is_empty() {
            return Err(wasm_bindgen::JsValue::from_str(
                "No radials found for target elevation",
            ));
        }

        // Build SweepField and render
        let t_build = web_time::Instant::now();
        let field = ::nexrad::model::data::SweepField::from_radials_owned(target_radials, product)
            .ok_or_else(|| {
                wasm_bindgen::JsValue::from_str("Failed to build SweepField from radials")
            })?;
        let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;

        let color_scale = default_color_scale(product);

        // Cap render resolution: native_for uses gate_count*2 which can be 3600+
        // pixels per side (13M+ pixels). Cap to 1024x1024 for ~1M pixels instead.
        const MAX_RENDER_SIZE: usize = 1024;
        let native_size = field.gate_count() * 2;
        let render_size = native_size.min(MAX_RENDER_SIZE);
        let options = RenderOptions::new(render_size, render_size)
            .transparent()
            .with_interpolation(interpolation);

        let t_render = web_time::Instant::now();
        let render_result = render_sweep(&field, &color_scale, &options)
            .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("Render failed: {}", e)))?;

        let image = render_result.into_image();
        let (width, height) = image.dimensions();
        let pixels = image.into_raw();
        let render_ms = t_render.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        log::info!(
            "worker_render: {} radials, {}x{} (native {}) in {:.0}ms (fetch: {:.0}ms, build: {:.0}ms, render: {:.0}ms)",
            radial_count,
            width,
            height,
            native_size,
            total_ms,
            fetch_ms,
            build_ms,
            render_ms,
        );

        // Transfer pixel buffer back to main thread
        let pixel_array = js_sys::Uint8Array::from(pixels.as_slice());
        let buffer = pixel_array.buffer();

        let result = js_sys::Object::new();
        js_sys::Reflect::set(&result, &"imageData".into(), &buffer).ok();
        js_sys::Reflect::set(
            &result,
            &"width".into(),
            &wasm_bindgen::JsValue::from(width),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"height".into(),
            &wasm_bindgen::JsValue::from(height),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"renderTimeMs".into(),
            &wasm_bindgen::JsValue::from(total_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"radialCount".into(),
            &wasm_bindgen::JsValue::from(radial_count as u32),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"fetchMs".into(),
            &wasm_bindgen::JsValue::from(fetch_ms),
        )
        .ok();
        // Sub-timings for diagnostics (worker log is not visible without logger init)
        js_sys::Reflect::set(
            &result,
            &"idbOpenMs".into(),
            &wasm_bindgen::JsValue::from(idb_open_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"listMs".into(),
            &wasm_bindgen::JsValue::from(list_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"blobFetchMs".into(),
            &wasm_bindgen::JsValue::from(blob_fetch_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"idbFetchMs".into(),
            &wasm_bindgen::JsValue::from(idb_fetch_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"decompressMs".into(),
            &wasm_bindgen::JsValue::from(decompress_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"decodeMs".into(),
            &wasm_bindgen::JsValue::from(decode_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"buildMs".into(),
            &wasm_bindgen::JsValue::from(build_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"renderMs".into(),
            &wasm_bindgen::JsValue::from(render_ms),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"matchingRecords".into(),
            &wasm_bindgen::JsValue::from(matching_keys.len() as u32),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"totalRecords".into(),
            &wasm_bindgen::JsValue::from(entries.len() as u32),
        )
        .ok();
        js_sys::Reflect::set(
            &result,
            &"blobBytes".into(),
            &wasm_bindgen::JsValue::from(blob_bytes as f64),
        )
        .ok();

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

    /// Texture cache for rendered radar imagery
    radar_texture_cache: nexrad::RadarTextureCache,

    /// Queue of files to download for selection download feature.
    /// Each entry is (date, file_name, timestamp).
    selection_download_queue: Vec<(chrono::NaiveDate, String, i64)>,

    /// Timestamp of the currently displayed scan (for detecting when to load a new scan)
    displayed_scan_timestamp: Option<i64>,

    /// Previous site ID to detect site changes (for clearing volume ring)
    previous_site_id: String,

    /// Channel for loading scans from cache on-demand (for scrubbing)
    scrub_load_channel: nexrad::ScrubLoadChannel,

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

    /// Previous render parameters for change detection (scan_key, elev_num, product, interp).
    /// When any of these change, a new worker.render() is sent.
    last_render_params: Option<(String, u8, String, String)>,

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

        Self {
            state,
            geo_layers,
            data_facade,
            download_channel,
            cache_load_channel,
            archive_index: nexrad::ArchiveIndex::new(),
            current_scan: None,
            radar_texture_cache: nexrad::RadarTextureCache::new(),
            selection_download_queue: Vec::new(),
            displayed_scan_timestamp: None,
            previous_site_id: initial_site_id,
            scrub_load_channel: nexrad::ScrubLoadChannel::new(),
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

    /// Send a render request to the worker for the current scan + settings.
    fn request_worker_render(&mut self) {
        let Some(ref scan_key) = self.current_render_scan_key else {
            return;
        };
        if self.decode_worker.is_none() {
            return;
        }

        let elevation_number = self.best_elevation_number();
        let product = self.state.viz_state.product.to_worker_string().to_string();
        let interpolation = self
            .state
            .viz_state
            .interpolation
            .to_worker_string()
            .to_string();

        let params = (
            scan_key.clone(),
            elevation_number,
            product.clone(),
            interpolation.clone(),
        );

        // Skip if same as last request
        if self.last_render_params.as_ref() == Some(&params) {
            return;
        }

        log::info!(
            "Requesting worker render: {} elev={} product={} interp={}",
            scan_key,
            elevation_number,
            product,
            interpolation
        );

        let scan_key = scan_key.clone();
        self.last_render_params = Some(params);
        self.decode_worker.as_mut().unwrap().render(
            scan_key,
            elevation_number,
            product,
            interpolation,
        );
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
            nexrad::RealtimeResult::RecordStored {
                scan_key,
                record_id,
                records_available,
            } => {
                log::debug!(
                    "Record stored: {} record {} ({} available)",
                    scan_key,
                    record_id,
                    records_available
                );
                // Records are now cached incrementally - timeline refresh will pick them up
                self.state.timeline_needs_refresh = true;
            }
            nexrad::RealtimeResult::PartialVolumeReady {
                scan_key,
                sweep_count,
                timestamp_ms,
            } => {
                log::info!(
                    "Partial volume ready: {} with {} sweeps at {}",
                    scan_key,
                    sweep_count,
                    timestamp_ms
                );

                // Store the pending decode request - will be processed in update loop
                self.state.pending_partial_decode = Some((timestamp_ms, scan_key.clone()));

                self.state.status_message = format!("Live: partial volume {} sweeps", sweep_count);
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

        // Detect site changes and clear volume ring
        if self.state.viz_state.site_id != self.previous_site_id {
            log::info!(
                "Site changed from {} to {}",
                self.previous_site_id,
                self.state.viz_state.site_id
            );
            self.radar_texture_cache.invalidate();
            self.displayed_scan_timestamp = None;
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
                            "Ingest complete: {} ({} records, {} elevations, {:.0}ms, fetch: {:.0}ms)",
                            result.scan_key,
                            result.records_stored,
                            result.elevation_numbers.len(),
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
                        self.state
                            .playback_state
                            .set_playback_position(result.context.timestamp_secs as f64);

                        // Refresh timeline to include the new scan
                        self.state.timeline_needs_refresh = true;

                        // Request eviction check
                        self.state.check_eviction_requested = true;

                        // Clear last render params to force a fresh render
                        self.last_render_params = None;

                        // Trigger render for the ingested scan
                        self.request_worker_render();
                    }
                    nexrad::WorkerOutcome::Rendered(result) => {
                        log::info!(
                            "Render complete: {}x{}, {} radials, {:.0}ms (fetch: {:.0}ms)",
                            result.width,
                            result.height,
                            result.radial_count,
                            result.render_time_ms,
                            result.fetch_ms,
                        );

                        self.state
                            .session_stats
                            .record_render_time(result.render_time_ms);

                        // Upload pixel data as GPU texture
                        if result.width > 0 && result.height > 0 && !result.image_data.is_empty() {
                            let image = egui::ColorImage::from_rgba_unmultiplied(
                                [result.width as usize, result.height as usize],
                                &result.image_data,
                            );

                            // Build a cache key for the rendered result
                            let cache_key = nexrad::RadarCacheKey::for_dynamic_sweep(
                                0, // content signature (not used for worker-rendered textures)
                                result.context.elevation_number as usize,
                                (0, 0),
                            );

                            self.radar_texture_cache.update(ctx, cache_key, image);
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

        // Check for completed scrub load operations (legacy path, kept for fallback)
        if let Some(result) = self.scrub_load_channel.try_recv() {
            match result {
                nexrad::ScrubLoadResult::Success { timestamp, .. }
                | nexrad::ScrubLoadResult::NotFound { timestamp }
                | nexrad::ScrubLoadResult::Error { timestamp, .. } => {
                    // In the new architecture, scrub uses worker.render() directly.
                    // This handler just marks the timestamp to prevent retry loops.
                    self.displayed_scan_timestamp = Some(timestamp);
                }
            }
        }

        // Handle realtime streaming results
        while let Some(result) = self.realtime_channel.try_recv() {
            self.handle_realtime_result(result, ctx);
        }

        // Handle pending partial volume decode — in the worker architecture,
        // partial volumes are already stored in IDB by the realtime ingest path.
        // We just need to send a render request.
        if let Some((timestamp_ms, _scan_key)) = self.state.pending_partial_decode.take() {
            let scan_ts_secs = timestamp_ms / 1000;
            let scan_key = data::ScanKey::from_secs(&self.state.viz_state.site_id, scan_ts_secs);
            self.current_render_scan_key = Some(scan_key.to_storage_key());
            self.displayed_scan_timestamp = Some(scan_ts_secs);
            self.last_render_params = None;
            self.request_worker_render();
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
        const MAX_SCAN_AGE_SECS: f64 = 15.0 * 60.0;
        {
            let playback_ts = self.state.playback_state.playback_position();
            if let Some(scan) = self
                .state
                .radar_timeline
                .find_recent_scan(playback_ts, MAX_SCAN_AGE_SECS)
            {
                let scan_ts = scan.start_time as i64;

                // Check if we need to load a different scan
                let needs_load = match self.displayed_scan_timestamp {
                    Some(displayed) => displayed != scan_ts,
                    None => true,
                };

                if needs_load && self.decode_worker.is_some() {
                    log::debug!(
                        "Scrubbing: render scan at {} (playback at {})",
                        scan_ts,
                        playback_ts as i64
                    );

                    // Build scan key in data storage format: "SITE|TIMESTAMP_MS"
                    let scan_key = data::ScanKey::from_secs(&self.state.viz_state.site_id, scan_ts);
                    self.current_render_scan_key = Some(scan_key.to_storage_key());
                    self.displayed_scan_timestamp = Some(scan_ts);
                    self.last_render_params = None; // Force fresh render
                    self.request_worker_render();
                }
            }
        }

        // Detect elevation/product/interpolation changes and trigger worker re-render.
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

        // Render canvas with texture-based radar rendering
        ui::render_canvas_with_geo(
            ctx,
            &mut self.state,
            Some(&self.geo_layers),
            &mut self.radar_texture_cache,
        );

        // Process keyboard shortcuts
        ui::handle_shortcuts(ctx, &mut self.state);

        // Render overlays (on top of everything)
        ui::render_site_modal(ctx, &mut self.state, &mut self.site_modal_state);
        ui::render_shortcuts_help(ctx, &mut self.state);
        ui::render_wipe_modal(ctx, &mut self.state);
    }
}
