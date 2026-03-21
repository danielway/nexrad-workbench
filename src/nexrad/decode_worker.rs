//! Web Worker-based data operations.
//!
//! Offloads expensive NEXRAD operations (ingestion, rendering) from the main UI
//! thread into a dedicated Web Worker. Communication uses `postMessage` with
//! Transferable ArrayBuffers for zero-copy data transfer.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, Worker, WorkerOptions, WorkerType};

// ---------------------------------------------------------------------------
// Typed structs for worker message payloads (serde-wasm-bindgen)
// ---------------------------------------------------------------------------

/// Envelope for all worker response messages (type + id).
#[derive(Deserialize)]
struct MessageEnvelope {
    id: u64,
}

/// Ingest result payload from the worker.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IngestResultMsg {
    scan_key: String,
    records_stored: u32,
    #[serde(default)]
    elevation_numbers: Vec<u8>,
    #[serde(default)]
    total_ms: f64,
    #[serde(default)]
    split_ms: f64,
    #[serde(default)]
    decompress_ms: f64,
    #[serde(default)]
    decode_ms: f64,
    #[serde(default)]
    extract_ms: f64,
    #[serde(default)]
    store_ms: f64,
    #[serde(default)]
    index_ms: f64,
    #[serde(default)]
    sweeps: Vec<crate::data::SweepMeta>,
    #[serde(default)]
    vcp: Option<crate::data::keys::ExtractedVcp>,
}

/// Chunk ingest result payload from the worker.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkIngestResultMsg {
    scan_key: String,
    #[serde(default)]
    sweeps_stored: u32,
    #[serde(default)]
    is_end: bool,
    #[serde(default)]
    total_ms: f64,
    #[serde(default)]
    elevations_completed: Vec<u8>,
    #[serde(default)]
    sweeps: Vec<crate::data::SweepMeta>,
    #[serde(default)]
    vcp: Option<crate::data::keys::ExtractedVcp>,
    #[serde(default)]
    current_elevation: Option<u8>,
    #[serde(default)]
    current_elevation_radials: Option<u32>,
    #[serde(default)]
    last_radial_azimuth: Option<f32>,
    #[serde(default)]
    last_radial_time_secs: Option<f64>,
    #[serde(default)]
    volume_header_time_secs: Option<f64>,
    #[serde(default)]
    chunk_elev_spans: Vec<(u8, f64, f64, u32)>,
}

/// Scalar fields of the decoded sweep response from the worker.
/// ArrayBuffer fields (azimuths, gateValues, radialTimes) are extracted separately.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DecodedResultMsg {
    #[serde(default)]
    azimuth_count: u32,
    #[serde(default)]
    gate_count: u32,
    #[serde(default)]
    first_gate_range_km: f64,
    #[serde(default)]
    gate_interval_km: f64,
    #[serde(default)]
    max_range_km: f64,
    #[serde(default = "default_product")]
    product: String,
    #[serde(default)]
    radial_count: u32,
    #[serde(default)]
    fetch_ms: f64,
    #[serde(default)]
    deser_ms: f64,
    #[serde(default)]
    marshal_ms: f64,
    #[serde(default)]
    total_ms: f64,
    #[serde(default = "default_scale")]
    scale: f32,
    #[serde(default)]
    offset: f32,
    #[serde(default)]
    mean_elevation: f32,
    #[serde(default)]
    sweep_start_secs: f64,
    #[serde(default)]
    sweep_end_secs: f64,
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
struct VolumeSweepMetaMsg {
    #[serde(default)]
    elevation_deg: f32,
    #[serde(default)]
    azimuth_count: u32,
    #[serde(default)]
    gate_count: u32,
    #[serde(default)]
    first_gate_km: f32,
    #[serde(default)]
    gate_interval_km: f32,
    #[serde(default)]
    max_range_km: f32,
    #[serde(default)]
    data_offset: u32,
    #[serde(default)]
    scale: f32,
    #[serde(default)]
    offset: f32,
}

/// Scalar fields of the volume decoded response.
/// The `buffer` ArrayBuffer and `sweepMeta` array are extracted separately.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VolumeDecodedResultMsg {
    #[serde(default)]
    total_ms: f64,
    #[serde(default = "default_product")]
    product: String,
    #[serde(default = "default_word_size")]
    word_size: u8,
    #[serde(default)]
    sweep_meta: Vec<VolumeSweepMetaMsg>,
}

fn default_word_size() -> u8 {
    2
}

/// Error message from the worker.
#[derive(Deserialize)]
struct ErrorMsg {
    id: u64,
    #[serde(default = "default_error_message")]
    message: String,
}

fn default_error_message() -> String {
    "Unknown worker error".to_string()
}

// ---------------------------------------------------------------------------
// Typed structs for outgoing worker requests (main → worker)
// ---------------------------------------------------------------------------

/// Request message sent to the worker for ingest operations.
/// The `data` ArrayBuffer is set separately for zero-copy transfer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestRequestMsg<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    id: f64,
    site_id: &'a str,
    timestamp_secs: f64,
    file_name: &'a str,
}

/// Request message sent to the worker for chunk ingest operations.
/// The `data` ArrayBuffer is set separately for zero-copy transfer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestChunkRequestMsg<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    id: f64,
    site_id: &'a str,
    timestamp_secs: f64,
    chunk_index: f64,
    is_start: bool,
    is_end: bool,
    file_name: &'a str,
}

/// Request message sent to the worker for render operations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderRequestMsg<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    id: f64,
    scan_key: &'a str,
    elevation_number: u8,
    product: &'a str,
}

/// Request message sent to the worker for volume render operations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderVolumeRequestMsg<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    id: f64,
    scan_key: &'a str,
    product: &'a str,
    elevation_numbers: &'a [u8],
}

/// Unique ID for tracking worker requests.
type RequestId = u64;

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

/// Manages a dedicated Web Worker for NEXRAD data operations.
///
/// Created once at app startup and kept alive for the entire session.
/// Supports two command types:
/// - `ingest`: Split, probe, and store archive records in IDB
/// - `render`: Selectively decode + render a single elevation
///
/// Results are polled via `try_recv()` each frame.
/// Context for a volume render request.
#[allow(dead_code)]
pub struct VolumeRenderContext {
    pub scan_key: String,
}

pub struct DecodeWorker {
    worker: Worker,
    next_id: u64,
    ready: Rc<RefCell<bool>>,
    pending_ingest: Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    pending_chunk_ingest: Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    pending_render: Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    pending_render_live: Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    pending_volume: Rc<RefCell<HashMap<RequestId, VolumeRenderContext>>>,
    results: Rc<RefCell<Vec<WorkerOutcome>>>,
    /// Requests queued before the worker was ready.
    queue: Vec<QueuedRequest>,
}

/// A request queued before the worker was ready.
enum QueuedRequest {
    Ingest(RequestId, Vec<u8>, String, i64, String),
    IngestChunk(RequestId, Vec<u8>, String, i64, u32, bool, bool, String),
    Render(RequestId, String, u8, String),
    RenderLive(RequestId, u8, String),
    RenderVolume(RequestId, String, String, Vec<u8>),
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
        let pending_render_live: Rc<RefCell<HashMap<RequestId, RenderContext>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pending_volume: Rc<RefCell<HashMap<RequestId, VolumeRenderContext>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let results: Rc<RefCell<Vec<WorkerOutcome>>> = Rc::new(RefCell::new(Vec::new()));

        // Set up the onmessage handler
        {
            let ready_c = ready.clone();
            let pending_ingest_c = pending_ingest.clone();
            let pending_chunk_ingest_c = pending_chunk_ingest.clone();
            let pending_render_c = pending_render.clone();
            let pending_render_live_c = pending_render_live.clone();
            let pending_volume_c = pending_volume.clone();
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
                        handle_chunk_ingested_message(&data, &pending_chunk_ingest_c, &results_c);
                        ctx_c.request_repaint();
                    }
                    Some("decoded") => {
                        handle_decoded_message(&data, &pending_render_c, &results_c);
                        ctx_c.request_repaint();
                    }
                    Some("live_decoded") => {
                        handle_live_decoded_message(&data, &pending_render_live_c, &results_c);
                        ctx_c.request_repaint();
                    }
                    Some("volume_decoded") => {
                        handle_volume_decoded_message(&data, &pending_volume_c, &results_c);
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
            pending_render_live,
            pending_volume,
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
    pub fn render(&mut self, scan_key: String, elevation_number: u8, product: String) {
        let id = self.next_request_id();
        self.pending_render.borrow_mut().insert(
            id,
            RenderContext {
                scan_key: scan_key.clone(),
                elevation_number,
            },
        );

        if *self.ready.borrow() {
            send_render_request(&self.worker, id, &scan_key, elevation_number, &product);
        } else {
            self.queue.push(QueuedRequest::Render(
                id,
                scan_key,
                elevation_number,
                product,
            ));
        }
    }

    /// Submit a live (partial sweep) render request: reads from in-memory accumulator.
    pub fn render_live(&mut self, elevation_number: u8, product: String) {
        let id = self.next_request_id();
        self.pending_render_live.borrow_mut().insert(
            id,
            RenderContext {
                scan_key: String::new(), // Not used for live renders
                elevation_number,
            },
        );

        if *self.ready.borrow() {
            send_render_live_request(&self.worker, id, elevation_number, &product);
        } else {
            self.queue
                .push(QueuedRequest::RenderLive(id, elevation_number, product));
        }
    }

    /// Submit a volume render request: fetch all elevations, pack for ray marching.
    pub fn render_volume(&mut self, scan_key: String, product: String, elevation_numbers: Vec<u8>) {
        let id = self.next_request_id();
        self.pending_volume.borrow_mut().insert(
            id,
            VolumeRenderContext {
                scan_key: scan_key.clone(),
            },
        );

        if *self.ready.borrow() {
            send_render_volume_request(&self.worker, id, &scan_key, &product, &elevation_numbers);
        } else {
            self.queue.push(QueuedRequest::RenderVolume(
                id,
                scan_key,
                product,
                elevation_numbers,
            ));
        }
    }

    /// Submit a single real-time chunk for incremental ingest.
    #[allow(clippy::too_many_arguments)]
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
                id,
                data,
                site_id,
                timestamp_secs,
                chunk_index,
                is_start,
                is_end,
                file_name,
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
                        id,
                        data,
                        site_id,
                        ts,
                        chunk_index,
                        is_start,
                        is_end,
                        file_name,
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
                    QueuedRequest::RenderLive(id, elev, product) => {
                        send_render_live_request(&self.worker, id, elev, &product);
                    }
                    QueuedRequest::RenderVolume(id, scan_key, product, elev_nums) => {
                        send_render_volume_request(
                            &self.worker,
                            id,
                            &scan_key,
                            &product,
                            &elev_nums,
                        );
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
}

/// Handle an "ingested" message from the worker.
fn handle_ingested_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let envelope: MessageEnvelope = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Failed to parse ingested envelope: {}", e);
            return;
        }
    };
    let id = envelope.id;

    let context = match pending.borrow_mut().remove(&id) {
        Some(ctx) => ctx,
        None => {
            log::warn!("Received ingested message for unknown request {}", id);
            return;
        }
    };

    let result_obj = js_sys::Reflect::get(data, &"result".into()).unwrap_or(JsValue::NULL);
    let r: IngestResultMsg = match serde_wasm_bindgen::from_value(result_obj) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to parse ingest result: {}", e);
            return;
        }
    };

    log::info!(
        "Worker ingest complete: {} ({} records, {} elevations, {} sweeps, vcp={}, {:.0}ms)",
        r.scan_key,
        r.records_stored,
        r.elevation_numbers.len(),
        r.sweeps.len(),
        r.vcp
            .as_ref()
            .map(|v| v.number.to_string())
            .unwrap_or_else(|| "none".to_string()),
        r.total_ms,
    );

    results
        .borrow_mut()
        .push(WorkerOutcome::Ingested(IngestResult {
            context,
            scan_key: r.scan_key,
            records_stored: r.records_stored,
            elevation_numbers: r.elevation_numbers,
            sweeps: r.sweeps,
            vcp: r.vcp,
            total_ms: r.total_ms,
            split_ms: r.split_ms,
            decompress_ms: r.decompress_ms,
            decode_ms: r.decode_ms,
            extract_ms: r.extract_ms,
            store_ms: r.store_ms,
            index_ms: r.index_ms,
        }));
}

/// Handle a "chunk_ingested" message from the worker.
fn handle_chunk_ingested_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let envelope: MessageEnvelope = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Failed to parse chunk_ingested envelope: {}", e);
            return;
        }
    };
    let id = envelope.id;

    let context = match pending.borrow_mut().remove(&id) {
        Some(ctx) => ctx,
        None => {
            log::warn!("Received chunk_ingested message for unknown request {}", id);
            return;
        }
    };

    let result_obj = js_sys::Reflect::get(data, &"result".into()).unwrap_or(JsValue::NULL);
    let r: ChunkIngestResultMsg = match serde_wasm_bindgen::from_value(result_obj) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to parse chunk ingest result: {}", e);
            return;
        }
    };

    results
        .borrow_mut()
        .push(WorkerOutcome::ChunkIngested(ChunkIngestResult {
            context,
            scan_key: r.scan_key,
            elevations_completed: r.elevations_completed,
            sweeps_stored: r.sweeps_stored,
            is_end: r.is_end,
            sweeps: r.sweeps,
            vcp: r.vcp,
            total_ms: r.total_ms,
            current_elevation: r.current_elevation,
            current_elevation_radials: r.current_elevation_radials,
            last_radial_azimuth: r.last_radial_azimuth,
            last_radial_time_secs: r.last_radial_time_secs,
            volume_header_time_secs: r.volume_header_time_secs,
            chunk_elev_spans: r.chunk_elev_spans,
        }));
}

/// Handle a "decoded" message from the worker.
fn handle_decoded_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let envelope: MessageEnvelope = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Failed to parse decoded envelope: {}", e);
            return;
        }
    };
    let id = envelope.id;

    let context = match pending.borrow_mut().remove(&id) {
        Some(ctx) => ctx,
        None => {
            log::warn!("Received decoded message for unknown request {}", id);
            return;
        }
    };

    // Extract ArrayBuffer fields (not serializable via serde)
    let az_buffer = js_sys::Reflect::get(data, &"azimuths".into()).unwrap_or(JsValue::NULL);
    let azimuths = js_sys::Float32Array::new(&az_buffer).to_vec();

    let val_buffer = js_sys::Reflect::get(data, &"gateValues".into()).unwrap_or(JsValue::NULL);
    let gate_values = js_sys::Float32Array::new(&val_buffer).to_vec();

    let rt_js = js_sys::Reflect::get(data, &"radialTimes".into()).unwrap_or(JsValue::NULL);
    let radial_times = if rt_js.is_object() && !rt_js.is_null() {
        js_sys::Float64Array::new(&rt_js).to_vec()
    } else {
        Vec::new()
    };

    // Deserialize all scalar fields via serde
    let r: DecodedResultMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to parse decoded result: {}", e);
            return;
        }
    };

    log::info!(
        "Worker decode: {}x{}, {} radials, {}, {:.0}ms (fetch: {:.1}, marshal: {:.1})",
        r.azimuth_count,
        r.gate_count,
        r.radial_count,
        r.product,
        r.total_ms,
        r.fetch_ms,
        r.marshal_ms,
    );

    results
        .borrow_mut()
        .push(WorkerOutcome::Decoded(DecodeResult {
            context,
            azimuths,
            gate_values,
            azimuth_count: r.azimuth_count,
            gate_count: r.gate_count,
            first_gate_range_km: r.first_gate_range_km,
            gate_interval_km: r.gate_interval_km,
            max_range_km: r.max_range_km,
            product: r.product,
            radial_count: r.radial_count,
            fetch_ms: r.fetch_ms,
            deser_ms: r.deser_ms,
            marshal_ms: r.marshal_ms,
            total_ms: r.total_ms,
            scale: r.scale,
            offset: r.offset,
            mean_elevation: r.mean_elevation,
            sweep_start_secs: r.sweep_start_secs,
            sweep_end_secs: r.sweep_end_secs,
            radial_times,
        }));
}

/// Handle an "error" message from the worker.
fn handle_error_message(data: &JsValue, results: &Rc<RefCell<Vec<WorkerOutcome>>>) {
    let e: ErrorMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(e) => e,
        Err(err) => {
            log::error!("Failed to parse error message: {}", err);
            return;
        }
    };

    log::error!("Worker error (request {}): {}", e.id, e.message);

    results.borrow_mut().push(WorkerOutcome::WorkerError {
        id: e.id,
        message: e.message,
    });
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
    let request = IngestRequestMsg {
        msg_type: "ingest",
        id: id as f64,
        site_id,
        timestamp_secs: timestamp_secs as f64,
        file_name,
    };
    let msg = match serde_wasm_bindgen::to_value(&request) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to serialize ingest request {}: {}", id, e);
            return;
        }
    };

    // ArrayBuffer must be set directly for zero-copy transfer
    let array = js_sys::Uint8Array::from(data);
    let buffer = array.buffer();
    js_sys::Reflect::set(&msg, &"data".into(), &buffer).ok();

    let transfer = js_sys::Array::new();
    transfer.push(&buffer);

    if let Err(e) = worker.post_message_with_transfer(&msg, &transfer) {
        log::error!("Failed to send ingest request {}: {:?}", id, e);
    }
}

/// Send a chunk ingest request to the worker.
#[allow(clippy::too_many_arguments)]
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
    let request = IngestChunkRequestMsg {
        msg_type: "ingest_chunk",
        id: id as f64,
        site_id,
        timestamp_secs: timestamp_secs as f64,
        chunk_index: chunk_index as f64,
        is_start,
        is_end,
        file_name,
    };
    let msg = match serde_wasm_bindgen::to_value(&request) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to serialize ingest_chunk request {}: {}", id, e);
            return;
        }
    };

    // ArrayBuffer must be set directly for zero-copy transfer
    let array = js_sys::Uint8Array::from(data);
    let buffer = array.buffer();
    js_sys::Reflect::set(&msg, &"data".into(), &buffer).ok();

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
    let request = RenderRequestMsg {
        msg_type: "render",
        id: id as f64,
        scan_key,
        elevation_number,
        product,
    };
    let msg = match serde_wasm_bindgen::to_value(&request) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to serialize render request {}: {}", id, e);
            return;
        }
    };

    if let Err(e) = worker.post_message(&msg) {
        log::error!("Failed to send render request {}: {:?}", id, e);
    }
}

/// Handle a "volume_decoded" message from the worker.
fn handle_volume_decoded_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, VolumeRenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let envelope: MessageEnvelope = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Failed to parse volume_decoded envelope: {}", e);
            return;
        }
    };
    let id = envelope.id;

    let _volume_ctx = match pending.borrow_mut().remove(&id) {
        Some(ctx) => ctx,
        None => {
            log::warn!("Received volume_decoded message for unknown request {}", id);
            return;
        }
    };

    // Deserialize scalar fields and sweep metadata via serde
    let r: VolumeDecodedResultMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to parse volume decoded result: {}", e);
            return;
        }
    };
    let word_size = r.word_size;

    // Extract packed buffer (ArrayBuffer, not serializable via serde)
    let buf_js = js_sys::Reflect::get(data, &"buffer".into()).unwrap_or(JsValue::NULL);
    let buffer = if !buf_js.is_null() && !buf_js.is_undefined() {
        let u8_view = js_sys::Uint8Array::new(&buf_js);
        u8_view.to_vec()
    } else {
        Vec::new()
    };

    let sweeps: Vec<VolumeSweepMeta> = r
        .sweep_meta
        .into_iter()
        .map(|s| VolumeSweepMeta {
            elevation_deg: s.elevation_deg,
            azimuth_count: s.azimuth_count,
            gate_count: s.gate_count,
            first_gate_km: s.first_gate_km,
            gate_interval_km: s.gate_interval_km,
            max_range_km: s.max_range_km,
            data_offset: s.data_offset,
            scale: s.scale,
            offset: s.offset,
        })
        .collect();

    log::info!(
        "Worker volume decode: {} sweeps, {:.1}KB buffer, product={}, {:.0}ms",
        sweeps.len(),
        buffer.len() as f64 / 1024.0,
        r.product,
        r.total_ms,
    );

    results
        .borrow_mut()
        .push(WorkerOutcome::VolumeDecoded(VolumeData {
            buffer,
            word_size,
            sweeps,
            product: r.product,
            total_ms: r.total_ms,
        }));
}

/// Handle a "live_decoded" message from the worker.
fn handle_live_decoded_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let envelope: MessageEnvelope = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Failed to parse live_decoded envelope: {}", e);
            return;
        }
    };
    let id = envelope.id;

    let context = match pending.borrow_mut().remove(&id) {
        Some(ctx) => ctx,
        None => {
            log::warn!("Received live_decoded message for unknown request {}", id);
            return;
        }
    };

    // Extract ArrayBuffer fields (same as handle_decoded_message)
    let az_buffer = js_sys::Reflect::get(data, &"azimuths".into()).unwrap_or(JsValue::NULL);
    let azimuths = js_sys::Float32Array::new(&az_buffer).to_vec();

    let val_buffer = js_sys::Reflect::get(data, &"gateValues".into()).unwrap_or(JsValue::NULL);
    let gate_values = js_sys::Float32Array::new(&val_buffer).to_vec();

    let rt_js = js_sys::Reflect::get(data, &"radialTimes".into()).unwrap_or(JsValue::NULL);
    let radial_times = if rt_js.is_object() && !rt_js.is_null() {
        js_sys::Float64Array::new(&rt_js).to_vec()
    } else {
        Vec::new()
    };

    let r: DecodedResultMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to parse live_decoded result: {}", e);
            return;
        }
    };

    log::info!(
        "Worker live_decoded: {}x{}, {} radials, {}, {:.0}ms",
        r.azimuth_count,
        r.gate_count,
        r.radial_count,
        r.product,
        r.total_ms,
    );

    results
        .borrow_mut()
        .push(WorkerOutcome::LiveDecoded(DecodeResult {
            context,
            azimuths,
            gate_values,
            azimuth_count: r.azimuth_count,
            gate_count: r.gate_count,
            first_gate_range_km: r.first_gate_range_km,
            gate_interval_km: r.gate_interval_km,
            max_range_km: r.max_range_km,
            product: r.product,
            radial_count: r.radial_count,
            fetch_ms: r.fetch_ms,
            deser_ms: r.deser_ms,
            marshal_ms: r.marshal_ms,
            total_ms: r.total_ms,
            scale: r.scale,
            offset: r.offset,
            mean_elevation: r.mean_elevation,
            sweep_start_secs: r.sweep_start_secs,
            sweep_end_secs: r.sweep_end_secs,
            radial_times,
        }));
}

/// Request message sent to the worker for live render operations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderLiveRequestMsg<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    id: f64,
    elevation_number: u8,
    product: &'a str,
}

/// Send a render_live request to the worker.
fn send_render_live_request(worker: &Worker, id: u64, elevation_number: u8, product: &str) {
    let request = RenderLiveRequestMsg {
        msg_type: "render_live",
        id: id as f64,
        elevation_number,
        product,
    };
    let msg = match serde_wasm_bindgen::to_value(&request) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to serialize render_live request {}: {}", id, e);
            return;
        }
    };

    if let Err(e) = worker.post_message(&msg) {
        log::error!("Failed to send render_live request {}: {:?}", id, e);
    }
}

/// Send a render_volume request to the worker.
fn send_render_volume_request(
    worker: &Worker,
    id: u64,
    scan_key: &str,
    product: &str,
    elevation_numbers: &[u8],
) {
    let request = RenderVolumeRequestMsg {
        msg_type: "render_volume",
        id: id as f64,
        scan_key,
        product,
        elevation_numbers,
    };
    let msg = match serde_wasm_bindgen::to_value(&request) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to serialize render_volume request {}: {}", id, e);
            return;
        }
    };

    if let Err(e) = worker.post_message(&msg) {
        log::error!("Failed to send render_volume request {}: {:?}", id, e);
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
