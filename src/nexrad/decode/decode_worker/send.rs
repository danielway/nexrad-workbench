//! Methods and helpers for sending messages to the Web Worker.

use super::types::*;
use super::DecodeWorker;
use crate::core::{ChunkIngestContext, IngestContext, RenderContext};
use web_sys::Worker;

// ---------------------------------------------------------------------------
// DecodeWorker send methods
// ---------------------------------------------------------------------------

impl DecodeWorker {
    /// Submit an archive for ingestion: split, probe elevations, store in IDB.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ingest(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp_secs: f64,
        file_name: String,
        fetch_latency_ms: f64,
        wanted_elevations: Vec<u8>,
    ) {
        let id = self.next_request_id();
        // Build the typed scan-key once at dispatch; the response path
        // reads it from this context instead of parsing the storage-key
        // string the worker echoes back.
        let scan_key = crate::data::ScanKey::from_secs_f64(&site_id, timestamp_secs);
        self.pending_ingest.borrow_mut().insert(
            id,
            IngestContext {
                scan_key,
                timestamp_secs,
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
                &wanted_elevations,
            );
        } else {
            self.queue.push(super::QueuedRequest::Ingest(
                id,
                data,
                site_id,
                timestamp_secs,
                file_name,
                wanted_elevations,
            ));
        }
    }

    /// Submit a decode request: fetch records from IDB, decode target elevation, return raw data.
    pub(crate) fn render(
        &mut self,
        scan_key: crate::data::ScanKey,
        elevation_number: u8,
        product: String,
    ) {
        let id = self.next_request_id();
        let storage_key = scan_key.to_storage_key();
        self.pending_render.borrow_mut().insert(
            id,
            RenderContext {
                scan_key,
                elevation_number,
            },
        );

        if *self.ready.borrow() {
            send_render_request(&self.worker, id, &storage_key, elevation_number, &product);
        } else {
            self.queue.push(super::QueuedRequest::Render(
                id,
                storage_key,
                elevation_number,
                product,
            ));
        }
    }

    /// Submit a live (partial sweep) render request: reads `site`'s
    /// in-memory accumulator.
    pub(crate) fn render_live(
        &mut self,
        site: crate::data::SiteId,
        elevation_number: u8,
        product: String,
    ) {
        let id = self.next_request_id();
        // Live renders aren't bound to a stored scan; supply a sentinel
        // ScanKey for the context (the receive path doesn't read it for
        // LiveDecoded, only for the error path, where it surfaces as the
        // failed scan timestamp).
        let scan_key =
            crate::data::ScanKey::new(crate::data::SiteId::new(""), crate::data::UnixMillis(0));
        self.pending_render_live.borrow_mut().insert(
            id,
            super::types::LiveRenderContext {
                site: site.clone(),
                render: RenderContext {
                    scan_key,
                    elevation_number,
                },
            },
        );

        if *self.ready.borrow() {
            send_render_live_request(&self.worker, id, &site, elevation_number, &product);
        } else {
            self.queue.push(super::QueuedRequest::RenderLive(
                id,
                site,
                elevation_number,
                product,
            ));
        }
    }

    /// Submit a volume render request: fetch all elevations, pack for ray marching.
    pub(crate) fn render_volume(
        &mut self,
        scan_key: crate::data::ScanKey,
        product: String,
        elevation_numbers: Vec<u8>,
    ) {
        let id = self.next_request_id();
        let storage_key = scan_key.to_storage_key();
        self.pending_volume
            .borrow_mut()
            .insert(id, VolumeRenderContext { scan_key });

        if *self.ready.borrow() {
            send_render_volume_request(
                &self.worker,
                id,
                &storage_key,
                &product,
                &elevation_numbers,
            );
        } else {
            self.queue.push(super::QueuedRequest::RenderVolume(
                id,
                storage_key,
                product,
                elevation_numbers,
            ));
        }
    }

    /// Submit a storm cell detection on one cut.
    pub(crate) fn detect(&mut self, request: crate::core::storm_cells::DetectRequest) {
        let id = self.next_request_id();
        self.pending_detect.borrow_mut().insert(
            id,
            DetectFor {
                request_id: request.request_id,
                scene: false,
            },
        );
        if *self.ready.borrow() {
            send_detect_request(&self.worker, id, &request);
        } else {
            self.queue.push(super::QueuedRequest::Detect(id, request));
        }
    }

    /// Submit a storm cell detection on the composite grid of several
    /// sites' cuts. Its reply is a `detected` message like `detect`'s.
    pub(crate) fn detect_composite(
        &mut self,
        request: crate::core::storm_cells::SceneDetectRequest,
    ) {
        let id = self.next_request_id();
        self.pending_detect.borrow_mut().insert(
            id,
            DetectFor {
                request_id: request.request_id,
                scene: true,
            },
        );
        if *self.ready.borrow() {
            send_detect_composite_request(&self.worker, id, &request);
        } else {
            self.queue
                .push(super::QueuedRequest::DetectComposite(id, request));
        }
    }

    /// Submit a single real-time chunk for incremental ingest.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ingest_chunk(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp_secs: f64,
        chunk_index: u32,
        source_sequence: u32,
        elevation_number: Option<u8>,
        chunk_index_in_sweep: Option<u8>,
        chunks_in_sweep: Option<u8>,
        is_start: bool,
        is_end: bool,
        file_name: String,
        is_last_in_sweep: bool,
    ) {
        let id = self.next_request_id();
        let scan_key = crate::data::ScanKey::from_secs_f64(&site_id, timestamp_secs);
        self.pending_chunk_ingest.borrow_mut().insert(
            id,
            ChunkIngestContext {
                scan_key,
                timestamp_secs,
                chunk_index,
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
                source_sequence,
                elevation_number,
                chunk_index_in_sweep,
                chunks_in_sweep,
                is_start,
                is_end,
                &file_name,
                is_last_in_sweep,
            );
        } else {
            self.queue.push(super::QueuedRequest::IngestChunk(
                id,
                data,
                site_id,
                timestamp_secs,
                chunk_index,
                source_sequence,
                elevation_number,
                chunk_index_in_sweep,
                chunks_in_sweep,
                is_start,
                is_end,
                file_name,
                is_last_in_sweep,
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Free-standing send helpers (used by both DecodeWorker methods and flush_queue)
// ---------------------------------------------------------------------------

/// Send an ingest request to the worker.
pub(super) fn send_ingest_request(
    worker: &Worker,
    id: u64,
    data: &[u8],
    site_id: &str,
    timestamp_secs: f64,
    file_name: &str,
    wanted_elevations: &[u8],
) {
    let request = IngestRequestMsg {
        msg_type: RequestType::Ingest.as_str(),
        id: id as f64,
        site_id,
        timestamp_secs,
        file_name,
        wanted_elevations,
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
pub(super) fn send_ingest_chunk_request(
    worker: &Worker,
    id: u64,
    data: &[u8],
    site_id: &str,
    timestamp_secs: f64,
    chunk_index: u32,
    source_sequence: u32,
    elevation_number: Option<u8>,
    chunk_index_in_sweep: Option<u8>,
    chunks_in_sweep: Option<u8>,
    is_start: bool,
    is_end: bool,
    file_name: &str,
    is_last_in_sweep: bool,
) {
    let request = IngestChunkRequestMsg {
        msg_type: RequestType::IngestChunk.as_str(),
        id: id as f64,
        site_id,
        timestamp_secs,
        chunk_index: chunk_index as f64,
        source_sequence,
        elevation_number,
        chunk_index_in_sweep,
        chunks_in_sweep,
        is_start,
        is_end,
        file_name,
        is_last_in_sweep,
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
pub(super) fn send_render_request(
    worker: &Worker,
    id: u64,
    scan_key: &str,
    elevation_number: u8,
    product: &str,
) {
    let request = RenderRequestMsg {
        msg_type: RequestType::Render.as_str(),
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

/// Send a render_live request to the worker.
pub(super) fn send_render_live_request(
    worker: &Worker,
    id: u64,
    site: &crate::data::SiteId,
    elevation_number: u8,
    product: &str,
) {
    let request = RenderLiveRequestMsg {
        msg_type: RequestType::RenderLive.as_str(),
        id: id as f64,
        site_id: &site.0,
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
pub(super) fn send_render_volume_request(
    worker: &Worker,
    id: u64,
    scan_key: &str,
    product: &str,
    elevation_numbers: &[u8],
) {
    let request = RenderVolumeRequestMsg {
        msg_type: RequestType::RenderVolume.as_str(),
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

/// The wire name of a cut's input.
fn input_str(input: crate::core::storm_cells::CutInput) -> &'static str {
    match input {
        crate::core::storm_cells::CutInput::Stored => "stored",
        crate::core::storm_cells::CutInput::Live { .. } => "live",
    }
}

/// Send a composite storm cell detection request to the worker.
pub(super) fn send_detect_composite_request(
    worker: &Worker,
    id: u64,
    request: &crate::core::storm_cells::SceneDetectRequest,
) {
    let msg = DetectCompositeRequestMsg {
        msg_type: RequestType::DetectComposite.as_str(),
        id: id as f64,
        sites: request
            .key
            .sites
            .iter()
            .zip(&request.radars)
            .map(|(k, &(radar_lat, radar_lon))| DetectCompositeSiteMsg {
                scan_key: k.scan_key.to_storage_key(),
                elevation_number: k.elevation_number,
                input: input_str(k.input),
                radar_lat,
                radar_lon,
            })
            .collect(),
        params: &request.params,
    };
    let msg = match serde_wasm_bindgen::to_value(&msg) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to serialize detect_composite request {}: {}", id, e);
            return;
        }
    };
    if let Err(e) = worker.post_message(&msg) {
        log::error!("Failed to send detect_composite request {}: {:?}", id, e);
    }
}

/// Send a storm cell detection request to the worker.
pub(super) fn send_detect_request(
    worker: &Worker,
    id: u64,
    request: &crate::core::storm_cells::DetectRequest,
) {
    let key = &request.key;
    let scan_key = key.scan_key.to_storage_key();
    let msg = DetectRequestMsg {
        msg_type: RequestType::Detect.as_str(),
        id: id as f64,
        scan_key: &scan_key,
        elevation_number: key.elevation_number,
        input: input_str(key.input),
        params: &request.params,
        radar_lat: request.radar_lat,
        radar_lon: request.radar_lon,
    };
    let msg = match serde_wasm_bindgen::to_value(&msg) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Failed to serialize detect request {}: {}", id, e);
            return;
        }
    };
    if let Err(e) = worker.post_message(&msg) {
        log::error!("Failed to send detect request {}: {:?}", id, e);
    }
}
