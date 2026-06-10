//! Worker message reception: onmessage callback setup and result deserialization.
//!
//! Every incoming message is first parsed as a [`ResponseHeader`] to extract
//! the message type and request id. Dispatch then matches a typed
//! [`ResponseType`] (not a magic string) and removes the pending-context
//! entry by id. If the body fails to parse after that point, the caller is
//! informed via [`WorkerOutcome::WorkerError`] rather than silently leaving
//! a zombie entry in the pending map.
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

        // Step 1: parse the header (type + id). This MUST succeed —
        // anything else is a protocol violation we can't correlate, so
        // there's no caller to notify.
        let header: ResponseHeader = match serde_wasm_bindgen::from_value(data.clone()) {
            Ok(h) => h,
            Err(e) => {
                log::error!("Failed to parse worker response header: {}", e);
                return;
            }
        };

        // Step 2: tag the message via the typed enum.
        let kind = match ResponseType::parse(&header.msg_type) {
            Some(k) => k,
            None => {
                log::error!("Unknown worker response type: {:?}", header.msg_type);
                return;
            }
        };
        let kind_str = kind.as_str();

        // Step 3: dispatch. Handlers that need an id receive it from the
        // header so they don't reparse the envelope.
        match kind {
            ResponseType::Ready => {
                *ready_c.borrow_mut() = true;
                log::debug!("Decode worker ready");
            }
            ResponseType::Ingested => {
                if let Some(id) = require_id(&header, kind_str) {
                    handle_ingested_message(id, &data, &pending_ingest_c, &results_c);
                    ctx_c.request_repaint();
                }
            }
            ResponseType::ChunkIngested => {
                if let Some(id) = require_id(&header, kind_str) {
                    handle_chunk_ingested_message(id, &data, &pending_chunk_ingest_c, &results_c);
                    ctx_c.request_repaint();
                }
            }
            ResponseType::Decoded => {
                if let Some(id) = require_id(&header, kind_str) {
                    handle_decoded_message(id, &data, &pending_render_c, &results_c);
                    ctx_c.request_repaint();
                }
            }
            ResponseType::LiveDecoded => {
                if let Some(id) = require_id(&header, kind_str) {
                    handle_live_decoded_message(id, &data, &pending_render_live_c, &results_c);
                    ctx_c.request_repaint();
                }
            }
            ResponseType::VolumeDecoded => {
                if let Some(id) = require_id(&header, kind_str) {
                    handle_volume_decoded_message(id, &data, &pending_volume_c, &results_c);
                    ctx_c.request_repaint();
                }
            }
            ResponseType::Error => {
                if let Some(id) = require_id(&header, kind_str) {
                    handle_error_message(
                        id,
                        &data,
                        &pending_ingest_c,
                        &pending_chunk_ingest_c,
                        &pending_render_c,
                        &pending_render_live_c,
                        &pending_volume_c,
                        &results_c,
                    );
                    ctx_c.request_repaint();
                }
            }
        }
    });

    worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget(); // Lives for app lifetime
}

// ---------------------------------------------------------------------------
// Shared deserialization helpers
// ---------------------------------------------------------------------------

/// Require an `id` field on a response that isn't `ready`. Logs and
/// returns `None` if the worker shipped a correlated message with no id.
fn require_id(header: &ResponseHeader, msg_type: &str) -> Option<RequestId> {
    match header.id {
        Some(id) => Some(id),
        None => {
            log::error!("Worker '{}' response missing id field", msg_type);
            None
        }
    }
}

/// Remove a pending context entry by id. Returns `None` (with a warning)
/// if the id isn't tracked — typically a duplicate response or a stale
/// request that was already cancelled.
fn take_pending_context<C>(
    id: RequestId,
    msg_type: &str,
    pending: &Rc<RefCell<HashMap<RequestId, C>>>,
) -> Option<C> {
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
        azimuth_spacing_deg: r.azimuth_spacing_deg,
    }
}

/// Push a `WorkerError` outcome for a request whose body failed to parse.
///
/// Called after the pending-context entry has already been removed — the
/// caller would otherwise see a silent drop and never learn the request
/// failed. Tagged with [`WorkerErrorKind::InvalidData`] since the wire
/// payload itself was the source of the failure.
fn push_payload_parse_error(
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
    id: RequestId,
    msg_type: &str,
    err: impl std::fmt::Display,
    failed_scan_timestamp_secs: Option<f64>,
) {
    let message = format!("Failed to parse {} payload: {}", msg_type, err);
    log::error!("{} (request {})", message, id);
    results.borrow_mut().push(WorkerOutcome::WorkerError {
        id,
        kind: WorkerErrorKind::InvalidData,
        message,
        failed_scan_timestamp_secs,
    });
}

// ---------------------------------------------------------------------------
// Per-message-type handlers
// ---------------------------------------------------------------------------

fn handle_ingested_message(
    id: RequestId,
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let context = match take_pending_context(id, "ingested", pending) {
        Some(ctx) => ctx,
        None => return,
    };

    let result_obj = js_sys::Reflect::get(data, &"result".into()).unwrap_or(JsValue::NULL);
    let r: IngestResultMsg = match serde_wasm_bindgen::from_value(result_obj) {
        Ok(r) => r,
        Err(e) => {
            push_payload_parse_error(results, id, "ingested", e, Some(context.timestamp_secs));
            return;
        }
    };

    log::debug!(
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

    // Scan identity is the decoded volume-header time the worker actually
    // keyed under (`r.scan_key`), NOT the dispatch-time filename key carried
    // in `context`. Parse the worker's storage-key string; fall back to the
    // context key only if it's somehow unparseable (shouldn't happen).
    let scan_key = crate::data::ScanKey::from_storage_key(&r.scan_key).unwrap_or_else(|e| {
        log::warn!(
            "Worker ingest returned unparseable scan key {:?} ({e}); using dispatch key",
            r.scan_key
        );
        context.scan_key.clone()
    });
    results
        .borrow_mut()
        .push(WorkerOutcome::Ingested(IngestResult {
            context,
            scan_key,
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
    id: RequestId,
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let context = match take_pending_context(id, "chunk_ingested", pending) {
        Some(ctx) => ctx,
        None => return,
    };

    let result_obj = js_sys::Reflect::get(data, &"result".into()).unwrap_or(JsValue::NULL);
    let r: ChunkIngestResultMsg = match serde_wasm_bindgen::from_value(result_obj) {
        Ok(r) => r,
        Err(e) => {
            push_payload_parse_error(
                results,
                id,
                "chunk_ingested",
                e,
                Some(context.timestamp_secs),
            );
            return;
        }
    };

    // Log chunk receipt with per-elevation azimuth ranges
    {
        let elev_summary: Vec<String> = r
            .chunk_elev_az_ranges
            .iter()
            .map(|(elev, az_start, az_end)| format!("e{}:[{:.1}..{:.1}]", elev, az_start, az_end))
            .collect();
        let completed = if r.elevations_completed.is_empty() {
            String::new()
        } else {
            format!(" completed={:?}", r.elevations_completed)
        };
        log::debug!(
            "Chunk received #{}: elevations=[{}]{} {:.0}ms",
            context.chunk_index,
            elev_summary.join(", "),
            completed,
            r.total_ms,
        );
    }

    let scan_key = context.scan_key.clone();
    results
        .borrow_mut()
        .push(WorkerOutcome::ChunkIngested(ChunkIngestResult {
            context,
            scan_key,
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
            chunk_min_time_secs: r.chunk_min_time_secs,
            chunk_max_time_secs: r.chunk_max_time_secs,
            chunk_elev_spans: r.chunk_elev_spans,
            chunk_elev_az_ranges: r.chunk_elev_az_ranges,
        }));
}

fn handle_decoded_message(
    id: RequestId,
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let context = match take_pending_context(id, "decoded", pending) {
        Some(ctx) => ctx,
        None => return,
    };

    let (azimuths, gate_values, radial_times) = extract_decode_arrays(data);

    let r: DecodedResultMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(r) => r,
        Err(e) => {
            push_payload_parse_error(
                results,
                id,
                "decoded",
                e,
                Some(context.scan_key.scan_start.as_secs_f64()),
            );
            return;
        }
    };

    log::debug!(
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
    id: RequestId,
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let context = match take_pending_context(id, "live_decoded", pending) {
        Some(ctx) => ctx,
        None => return,
    };

    let (azimuths, gate_values, radial_times) = extract_decode_arrays(data);

    let r: DecodedResultMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(r) => r,
        Err(e) => {
            push_payload_parse_error(
                results,
                id,
                "live_decoded",
                e,
                Some(context.scan_key.scan_start.as_secs_f64()),
            );
            return;
        }
    };

    log::debug!(
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
    id: RequestId,
    data: &JsValue,
    pending: &Rc<RefCell<HashMap<RequestId, VolumeRenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let volume_ctx = match take_pending_context(id, "volume_decoded", pending) {
        Some(ctx) => ctx,
        None => return,
    };

    let r: VolumeDecodedResultMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(r) => r,
        Err(e) => {
            push_payload_parse_error(
                results,
                id,
                "volume_decoded",
                e,
                Some(volume_ctx.scan_key.scan_start.as_secs_f64()),
            );
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

    log::debug!(
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
///
/// The id has already been extracted from the response header. Looks up
/// the failing request across all pending maps and removes it — otherwise
/// a failed ingest would leak its context indefinitely. When the request
/// can be correlated, the associated scan's start timestamp is surfaced
/// on the outcome so the main loop can reset per-scan UI state (e.g. the
/// timeline "processing" ghost) for the correct scan rather than guessing
/// from `displayed_scan_timestamp`.
#[allow(clippy::too_many_arguments)]
fn handle_error_message(
    id: RequestId,
    data: &JsValue,
    pending_ingest: &Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    pending_chunk_ingest: &Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    pending_render: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    pending_render_live: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    pending_volume: &Rc<RefCell<HashMap<RequestId, VolumeRenderContext>>>,
    results: &Rc<RefCell<Vec<WorkerOutcome>>>,
) {
    let e: ErrorMsg = match serde_wasm_bindgen::from_value(data.clone()) {
        Ok(e) => e,
        Err(err) => {
            // Even when the body fails to parse, the id from the header
            // is still authoritative — clean up whatever pending entry
            // matches so it doesn't leak.
            push_payload_parse_error(
                results,
                id,
                "error",
                err,
                cleanup_pending_by_id(
                    id,
                    pending_ingest,
                    pending_chunk_ingest,
                    pending_render,
                    pending_render_live,
                    pending_volume,
                ),
            );
            return;
        }
    };

    log::warn!(
        "Worker error (request {}, kind {:?}): {}",
        e.id,
        e.kind,
        e.message
    );

    let failed_scan_timestamp_secs = cleanup_pending_by_id(
        e.id,
        pending_ingest,
        pending_chunk_ingest,
        pending_render,
        pending_render_live,
        pending_volume,
    );

    results.borrow_mut().push(WorkerOutcome::WorkerError {
        id: e.id,
        kind: e.kind,
        message: e.message,
        failed_scan_timestamp_secs,
    });
}

/// Remove a pending entry across every pending-context map (whichever one
/// owns it). Returns the failing scan's start timestamp when it could be
/// correlated, so callers can reset per-scan UI state.
fn cleanup_pending_by_id(
    id: RequestId,
    pending_ingest: &Rc<RefCell<HashMap<RequestId, IngestContext>>>,
    pending_chunk_ingest: &Rc<RefCell<HashMap<RequestId, ChunkIngestContext>>>,
    pending_render: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    pending_render_live: &Rc<RefCell<HashMap<RequestId, RenderContext>>>,
    pending_volume: &Rc<RefCell<HashMap<RequestId, VolumeRenderContext>>>,
) -> Option<f64> {
    if let Some(ctx) = pending_ingest.borrow_mut().remove(&id) {
        Some(ctx.timestamp_secs)
    } else if let Some(ctx) = pending_chunk_ingest.borrow_mut().remove(&id) {
        Some(ctx.timestamp_secs)
    } else if let Some(ctx) = pending_render.borrow_mut().remove(&id) {
        Some(ctx.scan_key.scan_start.as_secs_f64())
    } else if let Some(ctx) = pending_render_live.borrow_mut().remove(&id) {
        Some(ctx.scan_key.scan_start.as_secs_f64())
    } else if let Some(ctx) = pending_volume.borrow_mut().remove(&id) {
        Some(ctx.scan_key.scan_start.as_secs_f64())
    } else {
        None
    }
}
