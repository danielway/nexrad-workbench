//! Worker message reception: onmessage callback setup and result deserialization.
#![allow(clippy::too_many_arguments)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::MessageEvent;

use super::types::*;

// ---------------------------------------------------------------------------
// onmessage callback setup (called from DecodeWorker::new)
// ---------------------------------------------------------------------------

/// Install the `onmessage` callback on the worker.
///
/// This is extracted from `DecodeWorker::new` so the constructor stays concise.
pub(super) fn setup_onmessage(
    worker: &web_sys::Worker,
    ctx: &eframe::egui::Context,
    ready: &Rc<RefCell<bool>>,
    pending_ingest: &Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    pending_chunk_ingest: &Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    pending_render: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    pending_render_live: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    pending_volume: &Rc<RefCell<HashMap<RequestId, VolumeRenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
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

// ---------------------------------------------------------------------------
// Shared deserialization helpers
// ---------------------------------------------------------------------------

fn extract_pending_context<C>(
    data: &JsValue,
    msg_type: &str,
    pending: &Rc<RefCell<HashMap<RequestId, C>>>,
) -> Option<C> {
    let envelope: MessageEnvelope = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Failed to parse {} envelope: {}", msg_type, e);
            return None;
        }
    };
    let id = envelope.id;

    match pending.borrow_mut().remove(&id) {
        Some(ctx) => Some(ctx),
        None => {
            log::warn!("Received {} message for unknown request {}", msg_type, id);
            None
        }
    }
}

fn extract_decode_arrays(data: &JsValue) -> (Vec<f32>, Vec<f32>, Vec<f64>) {
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

    (azimuths, gate_values, radial_times)
}

fn build_decode_result(
    context: RenderContext,
    r: DecodedResultMsg,
    azimuths: Vec<f32>,
    gate_values: Vec<f32>,
    radial_times: Vec<f64>,
) -> DecodeResult {
    DecodeResult {
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
    }
}

// ---------------------------------------------------------------------------
// Per-message-type handlers
// ---------------------------------------------------------------------------

fn handle_ingested_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let context = match extract_pending_context(data, "ingested", pending) {
        Some(ctx) => ctx,
        None => return,
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

fn handle_chunk_ingested_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let context = match extract_pending_context(data, "chunk_ingested", pending) {
        Some(ctx) => ctx,
        None => return,
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
            chunk_elev_az_ranges: r.chunk_elev_az_ranges,
        }));
}

fn handle_decoded_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let context = match extract_pending_context(data, "decoded", pending) {
        Some(ctx) => ctx,
        None => return,
    };

    let (azimuths, gate_values, radial_times) = extract_decode_arrays(data);

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
        .push(WorkerOutcome::Decoded(build_decode_result(
            context,
            r,
            azimuths,
            gate_values,
            radial_times,
        )));
}

fn handle_live_decoded_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let context = match extract_pending_context(data, "live_decoded", pending) {
        Some(ctx) => ctx,
        None => return,
    };

    let (azimuths, gate_values, radial_times) = extract_decode_arrays(data);

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
        .push(WorkerOutcome::LiveDecoded(build_decode_result(
            context,
            r,
            azimuths,
            gate_values,
            radial_times,
        )));
}

fn handle_volume_decoded_message(
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, VolumeRenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let _volume_ctx = match extract_pending_context(data, "volume_decoded", pending) {
        Some(ctx) => ctx,
        None => return,
    };

    let r: VolumeDecodedResultMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to parse volume decoded result: {}", e);
            return;
        }
    };
    let word_size = r.word_size;

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
