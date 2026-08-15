//! Filter-aware backfill of chunks the stream has already passed.
//!
//! Two entry points share one emitter ([`emit_backfill_chunks`]):
//! [`run_init_backfill`] fills in the current volume when the user joins
//! mid-volume, and [`run_mid_stream_backfill`] fills in the newly-selected
//! elevation after a mid-stream filter change. Both skip sweeps whose blobs are
//! already cached in IndexedDB — the worker treats their re-flushes as no-ops,
//! so the download would be pure waste.

use super::current_timestamp_f64;
use super::engine::build_engine_plan;
use super::loop_state::{drain_control, LoopState};
use crate::core::projection::SharedProjectionEngine;
use crate::core::StreamingFilter;
use crate::data::facade::MainThreadStore;
use crate::nexrad::live::realtime::{ControlMessage, RealtimeResult};
use crate::nexrad::live::streaming_state::StreamingState;
use eframe::egui;
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures_util::future::join_all;
use nexrad_data::aws::realtime::{ChunkIdentifier, VolumeIndex};
use std::collections::HashSet;

/// Elevation numbers already fully cached in IndexedDB for the given scan.
///
/// A "fully cached" sweep is one that's been flushed to the `cached_sweeps`
/// list (via an `is_last_in_sweep` or end-of-volume flush). Partial sweeps
/// don't appear here, so on resume we still re-download chunks for sweeps
/// that were interrupted mid-flight.
async fn cached_elevations_for_scan(
    facade: &MainThreadStore,
    site_id: &str,
    scan_start_secs: f64,
) -> HashSet<u8> {
    let scan_key = crate::data::ScanKey::from_secs_f64(site_id, scan_start_secs);
    match facade.scan_availability(&scan_key).await {
        Ok(Some(entry)) => entry
            .cached_sweeps
            .iter()
            .map(|s| s.elevation_number)
            .collect(),
        _ => HashSet::new(),
    }
}

/// Sequences in `[2, upper]` whose elevation matches `filter`, ordered by
/// sequence. Used by both the init-time backfill and the mid-stream
/// filter-change backfill to find already-published chunks of the user's
/// selected elevation in the current volume.
fn filter_backfill_sequences(
    iter: &StreamingState,
    filter: StreamingFilter,
    upper: usize,
) -> Vec<usize> {
    if upper < 2 {
        return Vec::new();
    }
    iter.mapper_matching_sequences_in_range(2, upper, |elev| {
        // Skip the Start chunk; only data chunks belong in a backfill set.
        elev.is_some() && filter.accepts(elev)
    })
}

/// Download the chunks listed in `targets` in parallel and emit them into
/// the realtime channel as [`RealtimeResult::ChunkData`] +
/// [`RealtimeResult::ChunkReceived`] pairs in sequence order. Returns the
/// number of chunks emitted (callers add this to `chunks_in_volume`).
///
/// Used both at init (backfilling the user's sweep on mid-volume join) and
/// when the filter changes mid-stream to fetch already-passed chunks of the
/// newly-selected elevation.
#[allow(clippy::too_many_arguments)]
async fn emit_backfill_chunks(
    site_id: &str,
    targets: &[ChunkIdentifier],
    iter: &mut StreamingState,
    engine: &SharedProjectionEngine,
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
    results_tx: &UnboundedSender<RealtimeResult>,
    ctx: &egui::Context,
    chunks_in_volume_start: u32,
    timestamp: f64,
    emitted_sequences_this_volume: &mut HashSet<usize>,
) -> u32 {
    use nexrad_data::aws::realtime::download_chunk;

    drain_control(loop_state, control_rx);
    if targets.is_empty() || loop_state.stop_requested {
        return 0;
    }

    let results = join_all(targets.iter().map(|id| download_chunk(site_id, id))).await;
    drain_control(loop_state, control_rx);
    if loop_state.stop_requested {
        return 0;
    }

    let mut downloaded: Vec<(usize, Vec<u8>)> = Vec::with_capacity(results.len());
    for (chunk_id, res) in targets.iter().zip(results) {
        match res {
            Ok((_id, chunk)) => {
                let chunk_data = chunk.data().to_vec();
                log::debug!(
                    "Filter backfill: downloaded chunk seq {} ({} bytes)",
                    chunk_id.sequence(),
                    chunk_data.len(),
                );
                downloaded.push((chunk_id.sequence(), chunk_data));
            }
            Err(e) => {
                log::warn!(
                    "Filter backfill: failed to download chunk seq {}: {}",
                    chunk_id.sequence(),
                    e
                );
            }
        }
    }
    downloaded.sort_by_key(|(seq, _)| *seq);

    let mut emitted: u32 = 0;
    for (seq, chunk_data) in downloaded {
        emitted += 1;
        let chunk_index = chunks_in_volume_start + emitted - 1;
        let is_last_in_sweep = iter.chunk_metadata(seq).map(|m| m.is_last_in_sweep());
        let metadata = iter.chunk_metadata(seq);
        let _ = results_tx.unbounded_send(RealtimeResult::ChunkData {
            data: chunk_data,
            chunk_index,
            source_sequence: seq as u32,
            elevation_number: metadata.and_then(|m| m.elevation_number()).map(|n| n as u8),
            chunk_index_in_sweep: metadata.map(|m| m.chunk_index_in_sweep() as u8),
            chunks_in_sweep: metadata.map(|m| m.chunks_in_sweep() as u8),
            is_start: false,
            is_end: false,
            timestamp,
            is_last_in_sweep,
        });
        let _ = results_tx.unbounded_send(RealtimeResult::ChunkReceived {
            chunks_in_volume: chunks_in_volume_start + emitted,
            is_volume_end: false,
            fetch_latency_ms: 0.0,
            plan: build_engine_plan(engine, iter.current_id(), current_timestamp_f64()),
            arrival_stat: None,
        });
        emitted_sequences_this_volume.insert(seq);
    }
    ctx.request_repaint();
    emitted
}

/// Run the init-time backfill for a mid-volume join: fetch the already-published
/// chunks of `backfill_filter` that precede `latest_seq` in this volume.
///
/// Returns the number of chunks emitted (callers add this to
/// `chunks_in_volume`) plus the set of elevations found already cached in IDB —
/// the caller reuses that set to decide whether the latest chunk itself still
/// needs emitting.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_init_backfill(
    site_id: &str,
    backfill_filter: StreamingFilter,
    latest_seq: usize,
    latest_elev: Option<usize>,
    volume: VolumeIndex,
    iter: &mut StreamingState,
    engine: &SharedProjectionEngine,
    facade: &MainThreadStore,
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
    results_tx: &UnboundedSender<RealtimeResult>,
    ctx: &egui::Context,
    chunks_in_volume_start: u32,
    scan_start_secs: f64,
    emitted_sequences_this_volume: &mut HashSet<usize>,
) -> (u32, HashSet<u8>) {
    use nexrad_data::aws::realtime::list_chunks_in_volume;

    // Skip backfilling sweeps whose blobs are already cached in IDB
    // (resume after a stop within the same volume). The worker's
    // `pre_completed` set ignores re-flushes for these, so the network
    // download is pure waste.
    let cached_elevs = cached_elevations_for_scan(facade, site_id, scan_start_secs).await;
    if !cached_elevs.is_empty() {
        log::debug!(
            "Filter backfill (init): {} elevation(s) already cached, will skip them: {:?}",
            cached_elevs.len(),
            cached_elevs,
        );
    }
    let backfill_seqs: Vec<usize> =
        filter_backfill_sequences(iter, backfill_filter, latest_seq.saturating_sub(1))
            .into_iter()
            .filter(|seq| {
                iter.chunk_metadata(*seq)
                    .and_then(|m| m.elevation_number())
                    .map(|elev| !cached_elevs.contains(&(elev as u8)))
                    .unwrap_or(true)
            })
            .collect();

    let mut emitted_total: u32 = 0;
    if !backfill_seqs.is_empty() {
        match list_chunks_in_volume(site_id, volume, 100).await {
            Ok(chunk_ids) => {
                let to_download: Vec<_> = chunk_ids
                    .into_iter()
                    .filter(|id| backfill_seqs.contains(&id.sequence()))
                    .collect();

                log::debug!(
                    "Filter backfill (init): downloading {} chunks for filter {:?} (latest_elev {:?}, seqs {:?})",
                    to_download.len(),
                    backfill_filter,
                    latest_elev,
                    backfill_seqs,
                );

                let emitted = emit_backfill_chunks(
                    site_id,
                    &to_download,
                    iter,
                    engine,
                    loop_state,
                    control_rx,
                    results_tx,
                    ctx,
                    chunks_in_volume_start,
                    scan_start_secs,
                    emitted_sequences_this_volume,
                )
                .await;
                emitted_total = emitted;

                log::debug!(
                    "Filter backfill (init): completed, {} chunks emitted",
                    emitted
                );
            }
            Err(e) => {
                log::warn!(
                    "Filter backfill (init): failed to list chunks: {}, skipping",
                    e
                );
            }
        }
    } else {
        log::debug!(
            "Filter backfill (init): no preceding chunks for latest seq {} (filter {:?})",
            latest_seq,
            backfill_filter,
        );
    }

    (emitted_total, cached_elevs)
}

/// Run a filter-aware backfill of chunks already published in the current
/// volume that match the new filter and haven't been emitted yet. Used after
/// a mid-stream filter change so the user sees their newly-selected elevation
/// without waiting for the next volume.
///
/// Returns the number of chunks emitted (callers add this to
/// `chunks_in_volume`). Updates `emitted_sequences_this_volume` for every
/// chunk it actually emits so a subsequent toggle back to a previous filter
/// doesn't double-fetch.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_mid_stream_backfill(
    site_id: &str,
    filter: StreamingFilter,
    iter: &mut StreamingState,
    engine: &SharedProjectionEngine,
    facade: &MainThreadStore,
    scan_start_secs: f64,
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
    results_tx: &UnboundedSender<RealtimeResult>,
    ctx: &egui::Context,
    chunks_in_volume_start: u32,
    timestamp: f64,
    emitted_sequences_this_volume: &mut HashSet<usize>,
) -> u32 {
    use nexrad_data::aws::realtime::list_chunks_in_volume;

    let StreamingFilter::Elevation(_) = filter else {
        return 0;
    };
    let current_seq = iter.current_sequence();
    if current_seq <= 1 {
        return 0;
    }
    let upper = current_seq.saturating_sub(1);
    // Skip elevations already in the IDB cache. Same rationale as the init
    // backfill: the worker treats their re-flushes as no-ops, so the
    // download is pure waste.
    let cached_elevs = cached_elevations_for_scan(facade, site_id, scan_start_secs).await;
    let candidate_seqs: Vec<usize> = iter
        .mapper_matching_sequences_in_range(2, upper, |elev| elev.is_some() && filter.accepts(elev))
        .into_iter()
        .filter(|seq| !emitted_sequences_this_volume.contains(seq))
        .filter(|seq| {
            iter.chunk_metadata(*seq)
                .and_then(|m| m.elevation_number())
                .map(|elev| !cached_elevs.contains(&(elev as u8)))
                .unwrap_or(true)
        })
        .collect();

    if candidate_seqs.is_empty() {
        return 0;
    }

    let volume = iter.current_volume();
    let chunk_ids = match list_chunks_in_volume(site_id, volume, 100).await {
        Ok(ids) => ids,
        Err(e) => {
            log::warn!(
                "Filter backfill (mid-stream): failed to list chunks: {}, skipping",
                e
            );
            return 0;
        }
    };

    let to_download: Vec<_> = chunk_ids
        .into_iter()
        .filter(|id| candidate_seqs.contains(&id.sequence()))
        .collect();

    log::debug!(
        "Filter backfill (mid-stream): downloading {} chunks for filter {:?} (seqs {:?})",
        to_download.len(),
        filter,
        candidate_seqs,
    );

    emit_backfill_chunks(
        site_id,
        &to_download,
        iter,
        engine,
        loop_state,
        control_rx,
        results_tx,
        ctx,
        chunks_in_volume_start,
        timestamp,
        emitted_sequences_this_volume,
    )
    .await
}
