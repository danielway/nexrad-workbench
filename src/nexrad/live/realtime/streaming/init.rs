//! Init-phase chunk emission — everything the loop does with the chunks
//! [`super::acquire::acquire_with_timeout`] returns, before the steady-state
//! polling loop starts.
//!
//! Two shapes: we either joined mid-volume (a separate Start chunk plus the
//! latest chunk, with a filter-aware backfill in between — see
//! [`super::backfill::run_init_backfill`]) or we joined at the volume start (the
//! latest chunk *is* the Start chunk).
//!
//! All three helpers derive the volume's scan identity from the Start chunk's
//! volume header via [`volume_header_start_secs`]; a missing header is fatal and
//! reported through [`abort_missing_volume_header`].

use super::current_timestamp_f64;
use super::engine::build_engine_plan;
use super::persist::cache_volume_number;
use crate::core::projection::SharedProjectionEngine;
use crate::core::StreamingFilter;
use crate::nexrad::live::realtime::RealtimeResult;
use crate::nexrad::live::streaming_state::StreamingState;
use eframe::egui;
use futures_channel::mpsc::UnboundedSender;
use nexrad_data::aws::realtime::{ChunkType, DownloadedChunk};
use std::cell::Cell;
use std::collections::HashSet;

/// Canonical scan-start timestamp (Unix seconds, whole-second precision)
/// for a volume, decoded from its Start chunk's volume header.
///
/// This is the single source of scan identity: `header.date_time()`
/// truncated to whole seconds via `.timestamp()`. The archive path
/// derives the exact same value in `worker_ingest`, and the worker
/// derives it in `decode_start_chunk`, so two realtime sessions (or an
/// archive download and a realtime stream) covering the same physical
/// volume always produce an identical `ScanKey`. No AWS upload time,
/// filename string, or lag estimate is involved.
///
/// Returns `None` when the chunk carries no readable volume header.
/// Callers treat that as a fatal stream error rather than falling back
/// to a non-header source — a missing header should not happen, and if
/// it does we want to surface it rather than silently mis-key the scan.
pub(super) fn volume_header_start_secs(start_chunk_bytes: &[u8]) -> Option<f64> {
    let file = nexrad_data::volume::File::new(start_chunk_bytes.to_vec());
    file.header()
        .and_then(|h| h.date_time())
        .map(|dt| dt.timestamp() as f64)
}

/// Report the fatal "Start chunk carries no readable volume header" condition:
/// log it, push an error to the UI, clear the active flag and repaint. Callers
/// return from the streaming loop immediately afterwards.
pub(super) fn abort_missing_volume_header(
    site_id: &str,
    results_tx: &UnboundedSender<RealtimeResult>,
    active: &Cell<bool>,
    ctx: &egui::Context,
) {
    log::error!(
        "Realtime: start chunk for {} has no readable volume header — aborting stream",
        site_id
    );
    let _ = results_tx.unbounded_send(RealtimeResult::Error(
        "Start chunk has no readable volume header — cannot assign scan identity".to_string(),
    ));
    active.set(false);
    ctx.request_repaint();
}

/// Mid-volume join: emit the volume's Start chunk (already downloaded at init)
/// so the worker can open an accumulator for this volume. Returns the volume's
/// canonical scan-start seconds, or `None` when the header is unreadable (the
/// error has already been reported).
pub(super) fn emit_mid_volume_start_chunk(
    site_id: &str,
    start_chunk: &DownloadedChunk,
    iter: &StreamingState,
    engine: &SharedProjectionEngine,
    results_tx: &UnboundedSender<RealtimeResult>,
    active: &Cell<bool>,
    ctx: &egui::Context,
) -> Option<f64> {
    let start_data = start_chunk.chunk.data().to_vec();
    let Some(header_secs) = volume_header_start_secs(&start_data) else {
        abort_missing_volume_header(site_id, results_tx, active, ctx);
        return None;
    };

    log::debug!(
        "Init: emitting start_chunk ({} bytes) for mid-volume join",
        start_data.len()
    );
    let _ = results_tx.unbounded_send(RealtimeResult::ChunkData {
        data: start_data,
        chunk_index: 0,
        source_sequence: 1,
        elevation_number: None,
        chunk_index_in_sweep: None,
        chunks_in_sweep: None,
        is_start: true,
        is_end: false,
        timestamp: header_secs,
        // Skip overlap deletion — we're only backfilling the current
        // sweep, not replacing the full volume.
        // Start chunks are metadata-only and aren't part of any sweep.
        is_last_in_sweep: Some(false),
    });
    // Why: chunk_projections is consumed by the worker fast-path to
    // detect last-chunk-in-sweep. ChunkReceived is the only event
    // that updates it on the main thread, so without this push the
    // resumed sweep's chunks reach the worker with stale/None
    // projections and finalize only on the next sweep's first chunk.
    let _ = results_tx.unbounded_send(RealtimeResult::ChunkReceived {
        chunks_in_volume: 1,
        is_volume_end: false,
        fetch_latency_ms: 0.0,
        plan: build_engine_plan(engine, iter.current_id(), current_timestamp_f64()),
        arrival_stat: None,
    });
    ctx.request_repaint();

    Some(header_secs)
}

/// Emit the latest chunk fetched at init, but only when the filter accepts it
/// (or when it's the volume's End chunk so the rollover signal still lands) and
/// its elevation isn't already cached in IDB. Returns the number of chunks
/// emitted — 0 or 1 — which callers add to `chunks_in_volume`.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_init_latest_chunk(
    latest_chunk: &DownloadedChunk,
    initial_filter: StreamingFilter,
    latest_elev: Option<usize>,
    cached_elevs: &HashSet<u8>,
    iter: &StreamingState,
    engine: &SharedProjectionEngine,
    results_tx: &UnboundedSender<RealtimeResult>,
    ctx: &egui::Context,
    chunks_in_volume_start: u32,
    scan_start_secs: f64,
    emitted_sequences_this_volume: &mut HashSet<usize>,
) -> u32 {
    let latest_seq = latest_chunk.identifier.sequence();
    let latest_data = latest_chunk.chunk.data().to_vec();
    let latest_type = latest_chunk.identifier.chunk_type();
    let latest_is_end = latest_type == ChunkType::End;
    let latest_matches = initial_filter.accepts(latest_elev);
    let latest_already_cached = latest_elev
        .map(|n| cached_elevs.contains(&(n as u8)))
        .unwrap_or(false);

    if (latest_matches && !latest_already_cached) || latest_is_end {
        let chunks_in_volume = chunks_in_volume_start + 1;
        emitted_sequences_this_volume.insert(latest_seq);
        log::debug!(
            "Init: emitting latest_chunk seq {} ({} bytes, is_end={}, matches_filter={})",
            latest_seq,
            latest_data.len(),
            latest_is_end,
            latest_matches,
        );
        let latest_is_last_in_sweep = iter
            .chunk_metadata(latest_seq)
            .map(|m| m.is_last_in_sweep());
        let _ = results_tx.unbounded_send(RealtimeResult::ChunkData {
            data: latest_data,
            chunk_index: chunks_in_volume - 1,
            source_sequence: latest_seq as u32,
            elevation_number: iter
                .chunk_metadata(latest_seq)
                .and_then(|m| m.elevation_number())
                .map(|n| n as u8),
            chunk_index_in_sweep: iter
                .chunk_metadata(latest_seq)
                .map(|m| m.chunk_index_in_sweep() as u8),
            chunks_in_sweep: iter
                .chunk_metadata(latest_seq)
                .map(|m| m.chunks_in_sweep() as u8),
            is_start: false,
            is_end: latest_is_end,
            timestamp: scan_start_secs,
            is_last_in_sweep: latest_is_last_in_sweep,
        });
        let _ = results_tx.unbounded_send(RealtimeResult::ChunkReceived {
            chunks_in_volume,
            is_volume_end: latest_is_end,
            fetch_latency_ms: 0.0,
            plan: build_engine_plan(engine, iter.current_id(), current_timestamp_f64()),
            arrival_stat: None,
        });
        ctx.request_repaint();
        1
    } else {
        log::debug!(
            "Init: skipping latest_chunk seq {} (elev {:?}) — does not match filter {:?}",
            latest_seq,
            latest_elev,
            initial_filter,
        );
        0
    }
}

/// Joined at volume start: the latest chunk IS the Start chunk, so emit it as
/// chunk 0 of a fresh volume. Returns the volume's canonical scan-start seconds,
/// or `None` when the header is unreadable (the error has already been
/// reported). The caller sets `chunks_in_volume` to 1.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_join_at_volume_start(
    site_id: &str,
    latest_chunk: &DownloadedChunk,
    iter: &StreamingState,
    engine: &SharedProjectionEngine,
    results_tx: &UnboundedSender<RealtimeResult>,
    active: &Cell<bool>,
    ctx: &egui::Context,
    emitted_sequences_this_volume: &mut HashSet<usize>,
) -> Option<f64> {
    let latest_data = latest_chunk.chunk.data().to_vec();
    let latest_type = latest_chunk.identifier.chunk_type();
    let latest_is_start = latest_type == ChunkType::Start;
    let latest_is_end = latest_type == ChunkType::End;
    let Some(header_secs) = volume_header_start_secs(&latest_data) else {
        abort_missing_volume_header(site_id, results_tx, active, ctx);
        return None;
    };
    emitted_sequences_this_volume.insert(latest_chunk.identifier.sequence());
    cache_volume_number(site_id, *latest_chunk.identifier.volume());

    log::debug!(
        "Init: emitting latest_chunk as start ({} bytes)",
        latest_data.len()
    );
    let init_is_last_in_sweep = iter
        .chunk_metadata(latest_chunk.identifier.sequence())
        .map(|m| m.is_last_in_sweep());
    let _ = results_tx.unbounded_send(RealtimeResult::ChunkData {
        data: latest_data,
        chunk_index: 0,
        source_sequence: latest_chunk.identifier.sequence() as u32,
        elevation_number: iter
            .chunk_metadata(latest_chunk.identifier.sequence())
            .and_then(|m| m.elevation_number())
            .map(|n| n as u8),
        chunk_index_in_sweep: iter
            .chunk_metadata(latest_chunk.identifier.sequence())
            .map(|m| m.chunk_index_in_sweep() as u8),
        chunks_in_sweep: iter
            .chunk_metadata(latest_chunk.identifier.sequence())
            .map(|m| m.chunks_in_sweep() as u8),
        is_start: latest_is_start,
        is_end: latest_is_end,
        timestamp: header_secs,
        is_last_in_sweep: init_is_last_in_sweep,
    });
    let _ = results_tx.unbounded_send(RealtimeResult::ChunkReceived {
        chunks_in_volume: 1,
        is_volume_end: latest_is_end,
        fetch_latency_ms: 0.0,
        plan: build_engine_plan(engine, iter.current_id(), current_timestamp_f64()),
        arrival_stat: None,
    });
    ctx.request_repaint();

    Some(header_secs)
}
