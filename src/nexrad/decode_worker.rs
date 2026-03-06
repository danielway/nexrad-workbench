//! Web Worker-based data operations.
//!
//! Offloads expensive NEXRAD operations (ingestion, rendering) from the main UI
//! thread into a dedicated Web Worker. Communication uses `postMessage` with
//! Transferable ArrayBuffers for zero-copy data transfer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, Worker, WorkerOptions, WorkerType};

/// Unique ID for tracking worker requests.
type RequestId = u64;

/// Context for an ingest request.
pub struct IngestContext {
    pub timestamp_secs: i64,
    #[allow(dead_code)]
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
    #[allow(dead_code)] // Available for direct VCP inspection; primary propagation is via IDB metadata
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
pub struct ChunkIngestContext {
    #[allow(dead_code)]
    pub site_id: String,
    pub timestamp_secs: i64,
    #[allow(dead_code)]
    pub chunk_index: u32,
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub sweeps: Vec<crate::data::SweepMeta>,
    /// VCP pattern if extracted.
    #[allow(dead_code)]
    pub vcp: Option<crate::data::keys::ExtractedVcp>,
    /// Total processing time in worker (ms).
    pub total_ms: f64,
    /// Elevation number currently being accumulated (partial sweep in progress).
    pub current_elevation: Option<u8>,
    /// Number of radials received so far for the current in-progress elevation.
    pub current_elevation_radials: Option<u32>,
    /// Min data timestamp in this chunk (Unix seconds, from radial collection timestamps).
    pub chunk_min_time_secs: Option<f64>,
    /// Last radial's azimuth angle in degrees (for sweep line extrapolation).
    pub last_radial_azimuth: Option<f32>,
    /// Timestamp of the last radial in Unix seconds (for sweep line extrapolation).
    pub last_radial_time_secs: Option<f64>,
    /// Volume header date/time in Unix seconds (authoritative scan start time).
    pub volume_header_time_secs: Option<f64>,
    /// Per-elevation time spans within this chunk:
    /// (elevation_number, start_secs, end_secs, radial_count).
    pub chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
}

/// Context for a render/decode request.
pub struct RenderContext {
    /// Scan storage key.
    #[allow(dead_code)]
    pub scan_key: String,
    /// Elevation number being rendered.
    #[allow(dead_code)]
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
}

/// Outcome of any worker operation.
pub enum WorkerOutcome {
    /// Archive ingest completed.
    Ingested(IngestResult),
    /// Per-chunk ingest completed (real-time streaming).
    ChunkIngested(ChunkIngestResult),
    /// Decode completed (raw data for GPU rendering).
    Decoded(DecodeResult),
    /// Error from any operation.
    WorkerError { id: u64, message: String },
}

/// Manages a dedicated Web Worker for NEXRAD data operations.
///
/// Created once at app startup and kept alive for the entire session.
/// Supports two command types:
/// - `ingest`: Split, probe, and store archive records in IDB
/// - `render`: Selectively decode + render a single elevation
///
/// Results are polled via `try_recv()` each frame.
pub struct DecodeWorker {
    worker: Worker,
    next_id: u64,
    ready: Rc<RefCell<bool>>,
    pending_ingest: Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    pending_chunk_ingest: Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    pending_render: Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    results: Rc<RefCell<Vec<WorkerOutcome>>>,
    /// Requests queued before the worker was ready.
    queue: Vec<QueuedRequest>,
}

/// A request queued before the worker was ready.
enum QueuedRequest {
    Ingest(RequestId, Vec<u8>, String, i64, String),
    IngestChunk(RequestId, Vec<u8>, String, i64, u32, bool, bool, String),
    Render(RequestId, String, u8, String),
}

impl DecodeWorker {
    /// Create a new decode worker.
    ///
    /// Discovers the WASM/JS URLs from the current page's `<link>` tags
    /// (generated by Trunk), creates a module worker, and sends the init message.
    /// The worker will post a "ready" message once WASM is initialized.
    pub fn new(ctx: eframe::egui::Context) -> Result<Self, String> {
        let js_url =
            discover_js_url().ok_or_else(|| "Could not find JS module URL in DOM".to_string())?;
        let wasm_url =
            discover_wasm_url().ok_or_else(|| "Could not find WASM URL in DOM".to_string())?;

        log::info!(
            "Creating decode worker with JS={}, WASM={}",
            js_url,
            wasm_url
        );

        // Create an ES module worker
        let mut opts = WorkerOptions::new();
        #[allow(deprecated)]
        opts.type_(WorkerType::Module);

        let worker = Worker::new_with_options("worker.js", &opts)
            .map_err(|e| format!("Failed to create Worker: {:?}", e))?;

        let ready = Rc::new(RefCell::new(false));
        let pending_ingest: Rc<RefCell<HashMap<RequestId, IngestContext>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pending_chunk_ingest: Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pending_render: Rc<RefCell<HashMap<RequestId, RenderContext>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let results: Rc<RefCell<Vec<WorkerOutcome>>> = Rc::new(RefCell::new(Vec::new()));

        // Set up the onmessage handler
        {
            let ready_c = ready.clone();
            let pending_ingest_c = pending_ingest.clone();
            let pending_chunk_ingest_c = pending_chunk_ingest.clone();
            let pending_render_c = pending_render.clone();
            let results_c = results.clone();
            let ctx_c = ctx.clone();

            let onmessage = Closure::<dyn Fn(MessageEvent)>::new(move |event: MessageEvent| {
                let data = event.data();
                let msg_type = js_sys::Reflect::get(&data, &"type".into())
                    .ok()
                    .and_then(|v| v.as_string());

                match msg_type.as_deref() {
                    Some("ready") => {
                        *ready_c.borrow_mut() = true;
                        log::info!("Decode worker ready");
                    }
                    Some("ingested") => {
                        handle_ingested_message(&data, &pending_ingest_c, &results_c);
                        ctx_c.request_repaint();
                    }
                    Some("chunk_ingested") => {
                        handle_chunk_ingested_message(
                            &data,
                            &pending_chunk_ingest_c,
                            &results_c,
                        );
                        ctx_c.request_repaint();
                    }
                    Some("decoded") => {
                        handle_decoded_message(&data, &pending_render_c, &results_c);
                        ctx_c.request_repaint();
                    }
                    Some("error") => {
                        handle_error_message(&data, &results_c);
                        ctx_c.request_repaint();
                    }
                    other => {
                        log::warn!("Unknown worker message type: {:?}", other);
                    }
                }
            });

            worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
            onmessage.forget(); // Lives for app lifetime
        }

        // Set up onerror handler
        {
            let onerror =
                Closure::<dyn Fn(web_sys::ErrorEvent)>::new(move |event: web_sys::ErrorEvent| {
                    log::error!(
                        "Decode worker error: {} ({}:{})",
                        event.message(),
                        event.filename(),
                        event.lineno()
                    );
                });

            worker.set_onerror(Some(onerror.as_ref().unchecked_ref()));
            onerror.forget();
        }

        // Send init message with the WASM/JS URLs
        let init_msg = js_sys::Object::new();
        js_sys::Reflect::set(&init_msg, &"type".into(), &"init".into()).ok();
        js_sys::Reflect::set(&init_msg, &"jsUrl".into(), &js_url.into()).ok();
        js_sys::Reflect::set(&init_msg, &"wasmUrl".into(), &wasm_url.into()).ok();

        worker
            .post_message(&init_msg)
            .map_err(|e| format!("Failed to send init message: {:?}", e))?;

        Ok(Self {
            worker,
            next_id: 1,
            ready,
            pending_ingest,
            pending_chunk_ingest,
            pending_render,
            results,
            queue: Vec::new(),
        })
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Submit an archive for ingestion: split, probe elevations, store in IDB.
    pub fn ingest(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp_secs: i64,
        file_name: String,
        fetch_latency_ms: f64,
    ) {
        let id = self.next_request_id();
        self.pending_ingest.borrow_mut().insert(
            id,
            IngestContext {
                timestamp_secs,
                file_name: file_name.clone(),
                fetch_latency_ms,
            },
        );

        if *self.ready.borrow() {
            send_ingest_request(
                &self.worker,
                id,
                &data,
                &site_id,
                timestamp_secs,
                &file_name,
            );
        } else {
            self.queue.push(QueuedRequest::Ingest(
                id,
                data,
                site_id,
                timestamp_secs,
                file_name,
            ));
        }
    }

    /// Submit a decode request: fetch records from IDB, decode target elevation, return raw data.
    pub fn render(
        &mut self,
        scan_key: String,
        elevation_number: u8,
        product: String,
    ) {
        let id = self.next_request_id();
        self.pending_render.borrow_mut().insert(
            id,
            RenderContext {
                scan_key: scan_key.clone(),
                elevation_number,
            },
        );

        if *self.ready.borrow() {
            send_render_request(
                &self.worker,
                id,
                &scan_key,
                elevation_number,
                &product,
            );
        } else {
            self.queue.push(QueuedRequest::Render(
                id,
                scan_key,
                elevation_number,
                product,
            ));
        }
    }

    /// Submit a single real-time chunk for incremental ingest.
    pub fn ingest_chunk(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp_secs: i64,
        chunk_index: u32,
        is_start: bool,
        is_end: bool,
        file_name: String,
    ) {
        let id = self.next_request_id();
        self.pending_chunk_ingest.borrow_mut().insert(
            id,
            ChunkIngestContext {
                site_id: site_id.clone(),
                timestamp_secs,
                chunk_index,
                is_end,
            },
        );

        if *self.ready.borrow() {
            send_ingest_chunk_request(
                &self.worker,
                id,
                &data,
                &site_id,
                timestamp_secs,
                chunk_index,
                is_start,
                is_end,
                &file_name,
            );
        } else {
            self.queue.push(QueuedRequest::IngestChunk(
                id, data, site_id, timestamp_secs, chunk_index, is_start, is_end, file_name,
            ));
        }
    }

    /// Flush any queued requests if the worker has become ready.
    pub fn flush_queue(&mut self) {
        if *self.ready.borrow() && !self.queue.is_empty() {
            let queued: Vec<_> = self.queue.drain(..).collect();
            log::info!("Flushing {} queued worker requests", queued.len());
            for request in queued {
                match request {
                    QueuedRequest::Ingest(id, data, site_id, ts, file_name) => {
                        send_ingest_request(&self.worker, id, &data, &site_id, ts, &file_name);
                    }
                    QueuedRequest::IngestChunk(
                        id, data, site_id, ts, chunk_index, is_start, is_end, file_name,
                    ) => {
                        send_ingest_chunk_request(
                            &self.worker,
                            id,
                            &data,
                            &site_id,
                            ts,
                            chunk_index,
                            is_start,
                            is_end,
                            &file_name,
                        );
                    }
                    QueuedRequest::Render(id, scan_key, elev, product) => {
                        send_render_request(&self.worker, id, &scan_key, elev, &product);
                    }
                }
            }
        }
    }

    /// Poll for completed worker results. Call this each frame in the update loop.
    pub fn try_recv(&mut self) -> Vec<WorkerOutcome> {
        self.flush_queue();
        self.results.borrow_mut().drain(..).collect()
    }

    /// Returns true if the worker has initialized and is ready to accept requests.
    #[allow(dead_code)]
    pub fn is_ready(&self) -> bool {
        *self.ready.borrow()
    }
}

/// Handle an "ingested" message from the worker.
fn handle_ingested_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let id = js_sys::Reflect::get(data, &"id".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;

    let context = match pending.borrow_mut().remove(&id) {
        Some(ctx) => ctx,
        None => {
            log::warn!("Received ingested message for unknown request {}", id);
            return;
        }
    };

    let result_obj = js_sys::Reflect::get(data, &"result".into()).unwrap_or(JsValue::NULL);

    let scan_key = js_sys::Reflect::get(&result_obj, &"scanKey".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();

    let records_stored = js_sys::Reflect::get(&result_obj, &"recordsStored".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u32;

    let total_ms = js_sys::Reflect::get(&result_obj, &"totalMs".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    // Extract sub-phase timings
    let get_f64 = |key: &str| -> f64 {
        js_sys::Reflect::get(&result_obj, &key.into())
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    };
    let split_ms = get_f64("splitMs");
    let decompress_ms = get_f64("decompressMs");
    let decode_ms = get_f64("decodeMs");
    let extract_ms = get_f64("extractMs");
    let store_ms = get_f64("storeMs");
    let index_ms = get_f64("indexMs");

    // Extract unique elevation numbers from the elevationMap
    let mut elevation_numbers: Vec<u8> = Vec::new();
    if let Ok(elev_map) = js_sys::Reflect::get(&result_obj, &"elevationMap".into()) {
        if !elev_map.is_undefined() && !elev_map.is_null() {
            let elev_obj: js_sys::Object = elev_map.unchecked_into();
            let keys = js_sys::Object::keys(&elev_obj);
            for i in 0..keys.length() {
                if let Some(key_str) = keys.get(i).as_string() {
                    if let Ok(arr) = js_sys::Reflect::get(&elev_obj, &JsValue::from_str(&key_str)) {
                        let arr: js_sys::Array = arr.unchecked_into();
                        for j in 0..arr.length() {
                            if let Some(n) = arr.get(j).as_f64() {
                                let n = n as u8;
                                if !elevation_numbers.contains(&n) {
                                    elevation_numbers.push(n);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    elevation_numbers.sort_unstable();

    // Parse sweep metadata from JSON
    let sweeps: Vec<crate::data::SweepMeta> =
        js_sys::Reflect::get(&result_obj, &"sweepsJson".into())
            .ok()
            .and_then(|v| v.as_string())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

    // Parse extracted VCP pattern from JSON
    let vcp: Option<crate::data::keys::ExtractedVcp> =
        js_sys::Reflect::get(&result_obj, &"vcpJson".into())
            .ok()
            .and_then(|v| v.as_string())
            .and_then(|s| serde_json::from_str(&s).ok());

    log::info!(
        "Worker ingest complete: {} ({} records, {} elevations, {} sweeps, vcp={}, {:.0}ms)",
        scan_key,
        records_stored,
        elevation_numbers.len(),
        sweeps.len(),
        vcp.as_ref().map(|v| v.number.to_string()).unwrap_or_else(|| "none".to_string()),
        total_ms,
    );

    results
        .borrow_mut()
        .push(WorkerOutcome::Ingested(IngestResult {
            context,
            scan_key,
            records_stored,
            elevation_numbers,
            sweeps,
            vcp,
            total_ms,
            split_ms,
            decompress_ms,
            decode_ms,
            extract_ms,
            store_ms,
            index_ms,
        }));
}

/// Handle a "chunk_ingested" message from the worker.
fn handle_chunk_ingested_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let id = js_sys::Reflect::get(data, &"id".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;

    let context = match pending.borrow_mut().remove(&id) {
        Some(ctx) => ctx,
        None => {
            log::warn!("Received chunk_ingested message for unknown request {}", id);
            return;
        }
    };

    let result_obj = js_sys::Reflect::get(data, &"result".into()).unwrap_or(JsValue::NULL);

    let scan_key = js_sys::Reflect::get(&result_obj, &"scanKey".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();

    let sweeps_stored = js_sys::Reflect::get(&result_obj, &"sweepsStored".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u32;

    let is_end = js_sys::Reflect::get(&result_obj, &"isEnd".into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let total_ms = js_sys::Reflect::get(&result_obj, &"totalMs".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    // Parse elevations completed
    let mut elevations_completed: Vec<u8> = Vec::new();
    if let Ok(arr) = js_sys::Reflect::get(&result_obj, &"elevationsCompleted".into()) {
        if !arr.is_undefined() && !arr.is_null() {
            let arr: js_sys::Array = arr.unchecked_into();
            for i in 0..arr.length() {
                if let Some(n) = arr.get(i).as_f64() {
                    elevations_completed.push(n as u8);
                }
            }
        }
    }

    // Parse sweep metadata
    let sweeps: Vec<crate::data::SweepMeta> =
        js_sys::Reflect::get(&result_obj, &"sweepsJson".into())
            .ok()
            .and_then(|v| v.as_string())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

    // Parse VCP
    let vcp: Option<crate::data::keys::ExtractedVcp> =
        js_sys::Reflect::get(&result_obj, &"vcpJson".into())
            .ok()
            .and_then(|v| v.as_string())
            .and_then(|s| serde_json::from_str(&s).ok());

    // Parse current in-progress elevation info
    let current_elevation = js_sys::Reflect::get(&result_obj, &"currentElevation".into())
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u8);
    let current_elevation_radials = js_sys::Reflect::get(&result_obj, &"currentElevationRadials".into())
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as u32);

    // Parse chunk data time range
    let chunk_min_time_secs = js_sys::Reflect::get(&result_obj, &"chunkMinTimeSecs".into())
        .ok()
        .and_then(|v| v.as_f64());

    // Parse last radial azimuth/time for sweep line extrapolation
    let last_radial_azimuth = js_sys::Reflect::get(&result_obj, &"lastRadialAzimuth".into())
        .ok()
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let last_radial_time_secs = js_sys::Reflect::get(&result_obj, &"lastRadialTimeSecs".into())
        .ok()
        .and_then(|v| v.as_f64());

    // Parse volume header time (authoritative scan start from Archive II header)
    let volume_header_time_secs = js_sys::Reflect::get(&result_obj, &"volumeHeaderTimeSecs".into())
        .ok()
        .and_then(|v| v.as_f64());

    // Parse per-elevation chunk time spans
    let chunk_elev_spans: Vec<(u8, f64, f64, u32)> =
        js_sys::Reflect::get(&result_obj, &"chunkElevSpansJson".into())
            .ok()
            .and_then(|v| v.as_string())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

    results
        .borrow_mut()
        .push(WorkerOutcome::ChunkIngested(ChunkIngestResult {
            context,
            scan_key,
            elevations_completed,
            sweeps_stored,
            is_end,
            sweeps,
            vcp,
            total_ms,
            current_elevation,
            current_elevation_radials,
            chunk_min_time_secs,
            last_radial_azimuth,
            last_radial_time_secs,
            volume_header_time_secs,
            chunk_elev_spans,
        }));
}

/// Handle a "decoded" message from the worker.
fn handle_decoded_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let id = js_sys::Reflect::get(data, &"id".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;

    let context = match pending.borrow_mut().remove(&id) {
        Some(ctx) => ctx,
        None => {
            log::warn!("Received decoded message for unknown request {}", id);
            return;
        }
    };

    let az_buffer = js_sys::Reflect::get(data, &"azimuths".into()).unwrap_or(JsValue::NULL);
    let azimuths = js_sys::Float32Array::new(&az_buffer).to_vec();

    let val_buffer = js_sys::Reflect::get(data, &"gateValues".into()).unwrap_or(JsValue::NULL);
    let gate_values = js_sys::Float32Array::new(&val_buffer).to_vec();

    let azimuth_count = js_sys::Reflect::get(data, &"azimuthCount".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as u32;
    let gate_count = js_sys::Reflect::get(data, &"gateCount".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as u32;
    let first_gate_range_km = js_sys::Reflect::get(data, &"firstGateRangeKm".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let gate_interval_km = js_sys::Reflect::get(data, &"gateIntervalKm".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let max_range_km = js_sys::Reflect::get(data, &"maxRangeKm".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let product = js_sys::Reflect::get(data, &"product".into())
        .ok().and_then(|v| v.as_string()).unwrap_or_else(|| "reflectivity".to_string());
    let radial_count = js_sys::Reflect::get(data, &"radialCount".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as u32;
    let fetch_ms = js_sys::Reflect::get(data, &"fetchMs".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let deser_ms = js_sys::Reflect::get(data, &"deserMs".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let total_ms = js_sys::Reflect::get(data, &"totalMs".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let marshal_ms = js_sys::Reflect::get(data, &"marshalMs".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let scale = js_sys::Reflect::get(data, &"scale".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
    let offset = js_sys::Reflect::get(data, &"offset".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let mean_elevation = js_sys::Reflect::get(data, &"meanElevation".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let sweep_start_secs = js_sys::Reflect::get(data, &"sweepStartSecs".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let sweep_end_secs = js_sys::Reflect::get(data, &"sweepEndSecs".into())
        .ok().and_then(|v| v.as_f64()).unwrap_or(0.0);

    log::info!(
        "Worker decode: {}x{}, {} radials, {}, {:.0}ms (fetch: {:.1}, marshal: {:.1})",
        azimuth_count, gate_count, radial_count, product, total_ms,
        fetch_ms, marshal_ms,
    );

    results
        .borrow_mut()
        .push(WorkerOutcome::Decoded(DecodeResult {
            context,
            azimuths,
            gate_values,
            azimuth_count,
            gate_count,
            first_gate_range_km,
            gate_interval_km,
            max_range_km,
            product,
            radial_count,
            fetch_ms,
            deser_ms,
            marshal_ms,
            total_ms,
            scale,
            offset,
            mean_elevation,
            sweep_start_secs,
            sweep_end_secs,
        }));
}

/// Handle an "error" message from the worker.
fn handle_error_message(data: &JsValue, results: &Rc<RefCell<Vec<WorkerOutcome>>>) {
    let id = js_sys::Reflect::get(data, &"id".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;

    let message = js_sys::Reflect::get(data, &"message".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "Unknown worker error".to_string());

    log::error!("Worker error (request {}): {}", id, message);

    results
        .borrow_mut()
        .push(WorkerOutcome::WorkerError { id, message });
}

// ============================================================================
// Send helpers
// ============================================================================

/// Send an ingest request to the worker.
fn send_ingest_request(
    worker: &Worker,
    id: u64,
    data: &[u8],
    site_id: &str,
    timestamp_secs: i64,
    file_name: &str,
) {
    let array = js_sys::Uint8Array::from(data);
    let buffer = array.buffer();

    let msg = js_sys::Object::new();
    js_sys::Reflect::set(&msg, &"type".into(), &"ingest".into()).ok();
    js_sys::Reflect::set(&msg, &"id".into(), &JsValue::from(id as f64)).ok();
    js_sys::Reflect::set(&msg, &"data".into(), &buffer).ok();
    js_sys::Reflect::set(&msg, &"siteId".into(), &JsValue::from_str(site_id)).ok();
    js_sys::Reflect::set(
        &msg,
        &"timestampSecs".into(),
        &JsValue::from(timestamp_secs as f64),
    )
    .ok();
    js_sys::Reflect::set(&msg, &"fileName".into(), &JsValue::from_str(file_name)).ok();

    let transfer = js_sys::Array::new();
    transfer.push(&buffer);

    if let Err(e) = worker.post_message_with_transfer(&msg, &transfer) {
        log::error!("Failed to send ingest request {}: {:?}", id, e);
    }
}

/// Send a chunk ingest request to the worker.
fn send_ingest_chunk_request(
    worker: &Worker,
    id: u64,
    data: &[u8],
    site_id: &str,
    timestamp_secs: i64,
    chunk_index: u32,
    is_start: bool,
    is_end: bool,
    file_name: &str,
) {
    let array = js_sys::Uint8Array::from(data);
    let buffer = array.buffer();

    let msg = js_sys::Object::new();
    js_sys::Reflect::set(&msg, &"type".into(), &"ingest_chunk".into()).ok();
    js_sys::Reflect::set(&msg, &"id".into(), &JsValue::from(id as f64)).ok();
    js_sys::Reflect::set(&msg, &"data".into(), &buffer).ok();
    js_sys::Reflect::set(&msg, &"siteId".into(), &JsValue::from_str(site_id)).ok();
    js_sys::Reflect::set(
        &msg,
        &"timestampSecs".into(),
        &JsValue::from(timestamp_secs as f64),
    )
    .ok();
    js_sys::Reflect::set(
        &msg,
        &"chunkIndex".into(),
        &JsValue::from(chunk_index as f64),
    )
    .ok();
    js_sys::Reflect::set(&msg, &"isStart".into(), &JsValue::from(is_start)).ok();
    js_sys::Reflect::set(&msg, &"isEnd".into(), &JsValue::from(is_end)).ok();
    js_sys::Reflect::set(&msg, &"fileName".into(), &JsValue::from_str(file_name)).ok();

    let transfer = js_sys::Array::new();
    transfer.push(&buffer);

    if let Err(e) = worker.post_message_with_transfer(&msg, &transfer) {
        log::error!("Failed to send ingest_chunk request {}: {:?}", id, e);
    }
}

/// Send a render request to the worker.
fn send_render_request(
    worker: &Worker,
    id: u64,
    scan_key: &str,
    elevation_number: u8,
    product: &str,
) {
    let msg = js_sys::Object::new();
    js_sys::Reflect::set(&msg, &"type".into(), &"render".into()).ok();
    js_sys::Reflect::set(&msg, &"id".into(), &JsValue::from(id as f64)).ok();
    js_sys::Reflect::set(&msg, &"scanKey".into(), &JsValue::from_str(scan_key)).ok();
    js_sys::Reflect::set(
        &msg,
        &"elevationNumber".into(),
        &JsValue::from(elevation_number),
    )
    .ok();
    js_sys::Reflect::set(&msg, &"product".into(), &JsValue::from_str(product)).ok();

    if let Err(e) = worker.post_message(&msg) {
        log::error!("Failed to send render request {}: {:?}", id, e);
    }
}

/// Discover the Trunk-generated JS module URL from DOM `<link rel="modulepreload">` tags.
fn discover_js_url() -> Option<String> {
    let document = web_sys::window()?.document()?;
    let links = document
        .query_selector_all("link[rel='modulepreload']")
        .ok()?;
    for i in 0..links.length() {
        if let Some(el) = links.get(i) {
            let el: &web_sys::Element = el.unchecked_ref();
            if let Some(href) = el.get_attribute("href") {
                // Find the main app module (not snippet/helper modules)
                if href.contains("nexrad-workbench") && href.ends_with(".js") {
                    return Some(href);
                }
            }
        }
    }
    None
}

/// Discover the Trunk-generated WASM URL from DOM `<link rel="preload">` tags.
fn discover_wasm_url() -> Option<String> {
    let document = web_sys::window()?.document()?;
    let links = document
        .query_selector_all("link[rel='preload'][type='application/wasm']")
        .ok()?;
    if let Some(el) = links.get(0) {
        let el: &web_sys::Element = el.unchecked_ref();
        return el.get_attribute("href");
    }
    None
}
