//! Streaming-loop implementation and private helpers for `RealtimeChannel`.
//!
//! This module contains the long-running `streaming_loop` async task that
//! polls AWS for new chunks, applies the projector, and dispatches results
//! back through the channel. All other functions here are helpers called
//! from `streaming_loop` (or from `super::mod`'s `RealtimeChannel::start`).

use super::{ControlMessage, ProjectorObservation, RealtimeResult};
use crate::core::projection::{ChunkCoord, KnownChunk, SharedProjectionEngine};
use crate::core::StreamingFilter;
use crate::core::StreamingPlan;
use crate::data::facade::MainThreadStore;
use crate::net::retry::{
    attempt_with_timeout, compute_delay, sleep_duration, sleep_ms, Verdict, REALTIME_CHUNK_POLICY,
};
use crate::nexrad::acquisition::download::NetworkStats;
use crate::nexrad::live::streaming_state::StreamingState;
use eframe::egui;
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures_util::future::join_all;
use nexrad_data::aws::realtime::ChunkIdentifier;
use std::cell::Cell;
use std::rc::Rc;

/// Loop-local mirror of the coordination state that used to live on
/// `RealtimeState`. Updated by [`drain_control`] from incoming
/// [`ControlMessage`]s.
struct LoopState {
    /// Set true when the UI sends [`ControlMessage::Stop`]; the loop
    /// checks this at every iteration / sleep boundary.
    stop_requested: bool,
    /// Currently applied chunk filter. Bumped by [`ControlMessage::SetFilter`]
    /// when the value actually changes.
    active_filter: StreamingFilter,
    /// Local counter that increments every time `active_filter` changes.
    /// Used by [`interruptible_sleep`] to signal a sleep-aborting filter
    /// swap to the main loop.
    filter_epoch: u64,
}

impl LoopState {
    fn new() -> Self {
        Self {
            stop_requested: false,
            active_filter: StreamingFilter::All,
            filter_epoch: 0,
        }
    }
}

/// Drain every pending control message into `loop_state`. Returns
/// `true` if `active_filter` changed.
fn drain_control(
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
) -> bool {
    let mut filter_changed = false;
    while let Ok(msg) = control_rx.try_recv() {
        match msg {
            ControlMessage::Stop => loop_state.stop_requested = true,
            ControlMessage::SetFilter(new_filter) => {
                if loop_state.active_filter != new_filter {
                    loop_state.active_filter = new_filter;
                    loop_state.filter_epoch = loop_state.filter_epoch.wrapping_add(1);
                    filter_changed = true;
                }
            }
        }
    }
    filter_changed
}

/// Outcome of `interruptible_sleep`. `Stopped` means the user requested stop;
/// `FilterChanged` means the active filter changed mid-sleep so the caller
/// should re-evaluate before continuing; `Completed` is the normal "slept the
/// full duration" path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SleepOutcome {
    Completed,
    Stopped,
    FilterChanged,
}

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
fn volume_header_start_secs(start_chunk_bytes: &[u8]) -> Option<f64> {
    let file = nexrad_data::volume::File::new(start_chunk_bytes.to_vec());
    file.header()
        .and_then(|h| h.date_time())
        .map(|dt| dt.timestamp() as f64)
}

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
) -> std::collections::HashSet<u8> {
    let scan_key = crate::data::ScanKey::from_secs_f64(site_id, scan_start_secs);
    match facade.scan_availability(&scan_key).await {
        Ok(Some(entry)) => entry
            .cached_sweeps
            .iter()
            .map(|s| s.elevation_number)
            .collect(),
        _ => std::collections::HashSet::new(),
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
    emitted_sequences_this_volume: &mut std::collections::HashSet<usize>,
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
        let _ = results_tx.unbounded_send(RealtimeResult::ChunkData {
            data: chunk_data,
            chunk_index,
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
async fn run_mid_stream_backfill(
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
    emitted_sequences_this_volume: &mut std::collections::HashSet<usize>,
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

/// Drain projector observations queued from `main.rs` (after worker
/// ingest) and apply each to the shared projection engine. The
/// dispatch shape — match on [`ProjectorObservation`] variant,
/// call the matching engine method — is intentionally explicit so
/// adding a new observation kind is just one new arm here.
fn drain_pending_observations(
    observations_rx: &mut UnboundedReceiver<ProjectorObservation>,
    engine: &SharedProjectionEngine,
    iter: &StreamingState,
) {
    while let Ok(obs) = observations_rx.try_recv() {
        match obs {
            ProjectorObservation::CollectionEndSecs(secs) => {
                engine
                    .borrow_mut()
                    .set_collection_anchor(iter.current_id(), secs);
            }
            ProjectorObservation::AvailabilityLagSecs(lag_secs) => {
                engine
                    .borrow_mut()
                    .record_availability_lag_for(iter.current_id(), lag_secs);
            }
        }
    }
}

/// Build a [`StreamingPlan`] from the shared engine, anchored at the loop's
/// current download cursor. The `engine.borrow_mut()` is scoped to this call —
/// per the engine invariant it never spans an `.await`.
fn build_engine_plan(
    engine: &SharedProjectionEngine,
    anchor: &ChunkIdentifier,
    now_secs: f64,
) -> Option<StreamingPlan> {
    engine
        .borrow_mut()
        .projection(anchor, now_secs)
        .map(|p| p.plan.clone())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn streaming_loop(
    ctx: egui::Context,
    site_id: String,
    active: Rc<Cell<bool>>,
    stats: NetworkStats,
    facade: MainThreadStore,
    results_tx: UnboundedSender<RealtimeResult>,
    mut observations_rx: UnboundedReceiver<ProjectorObservation>,
    mut control_rx: UnboundedReceiver<ControlMessage>,
    engine: SharedProjectionEngine,
) {
    let mut loop_state = LoopState::new();
    // First drain: pick up any control messages already queued (e.g.
    // the UI's once-per-frame sync_filter that fired between start()
    // returning and the loop's first iteration).
    drain_control(&mut loop_state, &mut control_rx);
    use nexrad_data::aws::realtime::{list_chunks_in_volume, ChunkType};

    log::debug!("Starting realtime streaming for site: {}", site_id);

    // Initialize with a timeout to avoid indefinite waiting when the site has
    // no data or is unreachable. Each .await is a cancellation point — when
    // the timeout wins the select, the init future is dropped, which drops any
    // in-flight HTTP request futures and cancels them.
    const ACQUIRE_TIMEOUT_SECS: u32 = 10;

    let init_future = acquire_streaming_state(&site_id);
    let timeout_future = sleep_ms(ACQUIRE_TIMEOUT_SECS * 1000);

    futures_util::pin_mut!(init_future);
    futures_util::pin_mut!(timeout_future);

    let init_result = match futures_util::future::select(init_future, timeout_future).await {
        futures_util::future::Either::Left((Ok(init), _)) => init,
        futures_util::future::Either::Left((Err(e), _)) => {
            let _ = results_tx.unbounded_send(RealtimeResult::Error(format!(
                "Failed to initialize: {}",
                e
            )));
            active.set(false);
            ctx.request_repaint();
            return;
        }
        futures_util::future::Either::Right(_) => {
            log::warn!(
                "Realtime acquisition timed out after {}s for site {}",
                ACQUIRE_TIMEOUT_SECS,
                site_id
            );
            let _ = results_tx.unbounded_send(RealtimeResult::Error(format!(
                "Acquisition timed out after {}s — data may be unavailable for this site",
                ACQUIRE_TIMEOUT_SECS
            )));
            active.set(false);
            ctx.request_repaint();
            return;
        }
    };

    let mut iter = init_result.state;
    // Pick up any filter the UI sent before init completed.
    drain_control(&mut loop_state, &mut control_rx);
    // Seed the shared engine: VCP parsed at init, active filter, warm stats.
    if let Some(vcp) = init_result.vcp.clone() {
        engine.borrow_mut().set_vcp(vcp);
    }
    engine.borrow_mut().set_filter(loop_state.active_filter);
    if let Some(cached) = load_cached_timing_stats(&site_id) {
        engine.borrow_mut().preload_timing_stats(cached);
        log::debug!("Loaded cached timing stats for {}", site_id);
    }
    // Tracks the previous chunk's S3 upload time for inter-chunk duration
    // samples (was `StreamingState.last_chunk_time`; now loop-local since the
    // engine owns the stats).
    let mut prev_upload_dt: Option<chrono::DateTime<chrono::Utc>> = iter
        .current_upload_secs()
        .and_then(|s| chrono::DateTime::from_timestamp_millis((s * 1000.0) as i64));
    let mut stats_tracker = StatsTracker::new(&iter);
    stats_tracker.update(&stats, &iter);

    log::debug!(
        "Iterator initialized: {} requests, {} bytes",
        iter.requests_made(),
        iter.bytes_downloaded()
    );

    // Send Started event
    let _ = results_tx.unbounded_send(RealtimeResult::Started {
        site_id: site_id.clone(),
    });
    ctx.request_repaint();

    let mut chunks_in_volume: u32;
    // Canonical scan start for the in-progress volume, decoded from the
    // Start chunk's volume header (see `volume_header_start_secs`). This is
    // the scan's identity — the value that becomes the IDB key, the cache
    // check key, and the main-thread anchor/render key. Newtype-wrapped so a
    // bare `f64` can't be substituted from a different time axis (wall
    // clock, S3 upload time, etc.) by mistake. Helpers downstream still take
    // `f64` — unwrap with `.0` at those call sites.
    let mut current_scan_start_secs: crate::data::ProvisionalStart;
    // Sequences emitted to the worker for the current volume (init backfill,
    // init latest, steady-state, and mid-stream backfill). The mid-stream
    // backfill consults this set to avoid re-downloading chunks the user has
    // already received during this volume.
    let mut emitted_sequences_this_volume: std::collections::HashSet<usize> =
        std::collections::HashSet::new();

    // --- Process init chunks (backfill from mid-volume join) ---
    // If start_chunk is Some, we joined mid-volume: emit start chunk + latest chunk.
    // If start_chunk is None, latest_chunk IS the start chunk.
    if let Some(start_chunk) = init_result.start_chunk {
        // Joined mid-volume: emit the start chunk + current sweep's chunks only.
        let start_data = start_chunk.chunk.data().to_vec();
        let Some(header_secs) = volume_header_start_secs(&start_data) else {
            log::error!(
                "Realtime: start chunk for {} has no readable volume header — aborting stream",
                site_id
            );
            let _ = results_tx.unbounded_send(RealtimeResult::Error(
                "Start chunk has no readable volume header — cannot assign scan identity"
                    .to_string(),
            ));
            active.set(false);
            ctx.request_repaint();
            return;
        };
        current_scan_start_secs = crate::data::ProvisionalStart(header_secs);

        log::debug!(
            "Init: emitting start_chunk ({} bytes) for mid-volume join",
            start_data.len()
        );
        let _ = results_tx.unbounded_send(RealtimeResult::ChunkData {
            data: start_data,
            chunk_index: 0,
            is_start: true,
            is_end: false,
            timestamp: current_scan_start_secs.0,
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
            plan: build_engine_plan(&engine, iter.current_id(), current_timestamp_f64()),
            arrival_stat: None,
        });
        ctx.request_repaint();

        // Filter-aware backfill. With `StreamingFilter::All` we backfill the
        // current sweep's preceding chunks (the historical default — keeps
        // the sweep coherent for the renderer). With
        // `StreamingFilter::Elevation(n)` we backfill every already-published
        // chunk of elevation `n` in this volume, which may be earlier sweeps
        // that already finished — that's by design so the user sees their
        // selected elevation immediately on connect.
        let initial_filter = loop_state.active_filter;
        let latest_seq = init_result.latest_chunk.identifier.sequence();
        let volume = *init_result.latest_chunk.identifier.volume();
        cache_volume_number(&site_id, volume);
        chunks_in_volume = 1; // start chunk already emitted

        let latest_elev = iter
            .chunk_metadata(latest_seq)
            .and_then(|m| m.elevation_number());

        let backfill_filter = match initial_filter {
            StreamingFilter::All => latest_elev
                .map(|n| StreamingFilter::Elevation(n as u8))
                .unwrap_or(StreamingFilter::All),
            other => other,
        };

        // Skip backfilling sweeps whose blobs are already cached in IDB
        // (resume after a stop within the same volume). The worker's
        // `pre_completed` set ignores re-flushes for these, so the network
        // download is pure waste.
        let cached_elevs =
            cached_elevations_for_scan(&facade, &site_id, current_scan_start_secs.0).await;
        if !cached_elevs.is_empty() {
            log::debug!(
                "Filter backfill (init): {} elevation(s) already cached, will skip them: {:?}",
                cached_elevs.len(),
                cached_elevs,
            );
        }
        let backfill_seqs: Vec<usize> =
            filter_backfill_sequences(&iter, backfill_filter, latest_seq.saturating_sub(1))
                .into_iter()
                .filter(|seq| {
                    iter.chunk_metadata(*seq)
                        .and_then(|m| m.elevation_number())
                        .map(|elev| !cached_elevs.contains(&(elev as u8)))
                        .unwrap_or(true)
                })
                .collect();

        if !backfill_seqs.is_empty() {
            match list_chunks_in_volume(&site_id, volume, 100).await {
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
                        &site_id,
                        &to_download,
                        &mut iter,
                        &engine,
                        &mut loop_state,
                        &mut control_rx,
                        &results_tx,
                        &ctx,
                        chunks_in_volume,
                        current_scan_start_secs.0,
                        &mut emitted_sequences_this_volume,
                    )
                    .await;
                    chunks_in_volume += emitted;

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

        // Emit the latest chunk only when the filter accepts it (or when
        // it's the volume's End chunk so the rollover signal still lands).
        let latest_data = init_result.latest_chunk.chunk.data().to_vec();
        let latest_type = init_result.latest_chunk.identifier.chunk_type();
        let latest_is_end = latest_type == ChunkType::End;
        let latest_matches = initial_filter.accepts(latest_elev);
        let latest_already_cached = latest_elev
            .map(|n| cached_elevs.contains(&(n as u8)))
            .unwrap_or(false);

        if (latest_matches && !latest_already_cached) || latest_is_end {
            chunks_in_volume += 1;
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
                is_start: false,
                is_end: latest_is_end,
                timestamp: current_scan_start_secs.0,
                is_last_in_sweep: latest_is_last_in_sweep,
            });
            let _ = results_tx.unbounded_send(RealtimeResult::ChunkReceived {
                chunks_in_volume,
                is_volume_end: latest_is_end,
                fetch_latency_ms: 0.0,
                plan: build_engine_plan(&engine, iter.current_id(), current_timestamp_f64()),
                arrival_stat: None,
            });
            ctx.request_repaint();
        } else {
            log::debug!(
                "Init: skipping latest_chunk seq {} (elev {:?}) — does not match filter {:?}",
                latest_seq,
                latest_elev,
                initial_filter,
            );
        }
    } else {
        // Joined at volume start: latest_chunk IS the start chunk
        let latest_data = init_result.latest_chunk.chunk.data().to_vec();
        let latest_type = init_result.latest_chunk.identifier.chunk_type();
        let latest_is_start = latest_type == ChunkType::Start;
        let latest_is_end = latest_type == ChunkType::End;
        let Some(header_secs) = volume_header_start_secs(&latest_data) else {
            log::error!(
                "Realtime: start chunk for {} has no readable volume header — aborting stream",
                site_id
            );
            let _ = results_tx.unbounded_send(RealtimeResult::Error(
                "Start chunk has no readable volume header — cannot assign scan identity"
                    .to_string(),
            ));
            active.set(false);
            ctx.request_repaint();
            return;
        };
        current_scan_start_secs = crate::data::ProvisionalStart(header_secs);
        chunks_in_volume = 1;
        emitted_sequences_this_volume.insert(init_result.latest_chunk.identifier.sequence());
        cache_volume_number(&site_id, *init_result.latest_chunk.identifier.volume());

        log::debug!(
            "Init: emitting latest_chunk as start ({} bytes)",
            latest_data.len()
        );
        let init_is_last_in_sweep = iter
            .chunk_metadata(init_result.latest_chunk.identifier.sequence())
            .map(|m| m.is_last_in_sweep());
        let _ = results_tx.unbounded_send(RealtimeResult::ChunkData {
            data: latest_data,
            chunk_index: 0,
            is_start: latest_is_start,
            is_end: latest_is_end,
            timestamp: current_scan_start_secs.0,
            is_last_in_sweep: init_is_last_in_sweep,
        });
        let _ = results_tx.unbounded_send(RealtimeResult::ChunkReceived {
            chunks_in_volume,
            is_volume_end: latest_is_end,
            fetch_latency_ms: 0.0,
            plan: build_engine_plan(&engine, iter.current_id(), current_timestamp_f64()),
            arrival_stat: None,
        });
        ctx.request_repaint();
    }

    // --- Main streaming loop: emit ChunkData per chunk ---
    // Per-chunk arrival tracking: captured on the first iteration for each
    // chunk and reset on success (or on final-retry recovery).
    let mut none_retries: u32 = 0;
    let mut cur_predicted_at: Option<f64> = None; // absolute Unix seconds
    let mut cur_last_empty_at: Option<f64> = None;
    // Captures `target.projected.clone()` on a chunk's first attempt so retry
    // iterations don't overwrite it with a fresh (post-anchor-advance)
    // projection. Read at success time into the per-chunk arrival stat.
    let mut cur_forecast: Option<crate::core::ChunkProjectedTimes> = None;
    let mut cur_predicted_wait_secs: Option<f64> = None;
    // How the wait before this chunk's fetch resolved. Set by the adaptive
    // cross-volume wait helper (early-fire / re-anchor); stays
    // `SleptToPrediction` for the plain single-sleep path.
    let mut cur_wait_resolution = crate::core::WaitResolution::SleptToPrediction;
    // Revision of the plan that produced cur_forecast, captured at the
    // same moment so the per-chunk arrival stat can record which plan
    // version made the prediction. Diagnostics use this to tell
    // "model wrong" from "model superseded by a fresh observation."
    let mut cur_plan_revision: Option<u64> = None;
    // Track filter changes across iterations so we can run a mid-stream
    // backfill exactly once per change, and so the in-flight predicted-at
    // diagnostic doesn't outlive its target sequence.
    // Seed the loop's filter mirror from the LoopState (already
    // populated by the init-time drain).
    let mut active_filter_epoch: u64 = loop_state.filter_epoch;
    let mut active_filter: StreamingFilter = loop_state.active_filter;
    loop {
        // Drain control messages once per iteration so stop/filter
        // signals propagate without waiting for a sleep boundary.
        drain_control(&mut loop_state, &mut control_rx);
        if loop_state.stop_requested {
            log::debug!("Realtime streaming stopped");
            break;
        }

        // Ingest any volume header time + availability lag the worker
        // produced from the most recent chunk's radials so projections and
        // stats in this iteration see them.
        drain_pending_observations(&mut observations_rx, &engine, &iter);

        // Filter-change detection: if the user toggled to a new filter
        // (FilterChanged sleep wake or first iteration after a `set_filter`
        // race), run the mid-stream backfill before re-targeting. Discard
        // stale per-chunk diagnostics — they were aimed at the previous
        // target sequence.
        if loop_state.filter_epoch != active_filter_epoch {
            let new_filter = loop_state.active_filter;
            log::debug!(
                "streaming_loop: filter changed {:?} -> {:?}, resolving target",
                active_filter,
                new_filter,
            );
            cur_predicted_at = None;
            cur_last_empty_at = None;
            cur_forecast = None;
            cur_predicted_wait_secs = None;
            cur_plan_revision = None;
            cur_wait_resolution = crate::core::WaitResolution::SleptToPrediction;
            none_retries = 0;
            let emitted = run_mid_stream_backfill(
                &site_id,
                new_filter,
                &mut iter,
                &engine,
                &facade,
                current_scan_start_secs.0,
                &mut loop_state,
                &mut control_rx,
                &results_tx,
                &ctx,
                chunks_in_volume,
                current_scan_start_secs.0,
                &mut emitted_sequences_this_volume,
            )
            .await;
            chunks_in_volume += emitted;
            active_filter = new_filter;
            active_filter_epoch = loop_state.filter_epoch;
            engine.borrow_mut().set_filter(active_filter);
        }

        // Build the canonical plan once per iteration. Every consumer of
        // forward-looking timing — sleep target here, UI countdown over the
        // wire, the per-chunk arrival diagnostic — reads from this object,
        // so the loop's behavior can't drift from what the UI displays.
        let now_secs = current_timestamp_f64();
        let plan = build_engine_plan(&engine, iter.current_id(), now_secs);

        // Sleep target: poll-time of the immediate next download. POLL_BIAS
        // and the bucket's retry budget are already folded into the
        // forecast's `poll_at_secs` by the projector — no additional
        // padding needed (compare to the old explicit
        // `poll_delay_after_predicted_ms`).
        let time_until_next_opt = plan
            .as_ref()
            .and_then(|p| p.next_target())
            .and_then(|t| t.projected.as_ref())
            .map(|f| std::time::Duration::from_secs_f64((f.poll_at_secs - now_secs).max(0.0)));

        // Capture the prediction once per chunk so retry iterations don't
        // overwrite it with a near-zero "wait" after a 404.
        let is_first_iter_for_chunk = cur_predicted_at.is_none();
        if is_first_iter_for_chunk {
            if let Some(plan_ref) = plan.as_ref() {
                if let Some(forecast) = plan_ref.next_target().and_then(|t| t.projected.as_ref()) {
                    cur_predicted_at = Some(forecast.available_at_secs);
                    cur_predicted_wait_secs = Some(forecast.poll_at_secs - now_secs);
                    cur_forecast = Some(forecast.clone());
                    cur_plan_revision = Some(plan_ref.revision);
                }
            }
        }
        // Adaptive-wait inputs: whether the next target is in the next volume
        // (the only case the list probe helps) and its sequence number.
        let next_in_next_volume = plan
            .as_ref()
            .map(|p| p.next_target_in_next_volume())
            .unwrap_or(false);
        let next_target_seq = plan
            .as_ref()
            .and_then(|p| p.next_target())
            .map(|t| t.sequence);

        if let Some(wait_duration) = time_until_next_opt {
            let wait_ms = wait_duration.as_millis() as u32;
            // POLL_BIAS and the bucket retry budget are already folded into
            // `projected_poll_at_secs` upstream, so we sleep directly to
            // that target without additional padding here.
            let threshold_ms = (LIST_PROBE_THRESHOLD_SECS * 1000.0) as u32;
            let outcome = if let (true, Some(target_seq)) = (
                should_list_now(wait_ms, next_in_next_volume, threshold_ms),
                next_target_seq,
            ) {
                // Long wait: list-probe the slot the target lives in (next
                // volume for a cross-volume target, else the current volume) to
                // early-fire / re-anchor / flip future→available instead of
                // dead-reckoning the whole sleep.
                let target_volume = if next_in_next_volume {
                    iter.current_volume().next()
                } else {
                    iter.current_volume()
                };
                let prev_anchor_upload = iter.current_upload_secs();
                let cursor_anchor = iter.current_id().clone();
                let (out, resolution) = wait_for_next_target(
                    &site_id,
                    &engine,
                    &cursor_anchor,
                    &mut loop_state,
                    &mut control_rx,
                    &ctx,
                    wait_ms,
                    target_seq,
                    target_volume,
                    prev_anchor_upload,
                    active_filter_epoch,
                )
                .await;
                cur_wait_resolution = resolution;
                out
            } else if wait_ms > 0 {
                interruptible_sleep(
                    &mut loop_state,
                    &mut control_rx,
                    &ctx,
                    wait_ms,
                    active_filter_epoch,
                )
                .await
            } else {
                SleepOutcome::Completed
            };
            match outcome {
                SleepOutcome::Stopped => {
                    log::debug!("Realtime streaming stopped");
                    break;
                }
                SleepOutcome::FilterChanged => {
                    // Re-enter the loop top so the filter-change branch
                    // runs the mid-stream backfill and re-targets.
                    continue;
                }
                SleepOutcome::Completed => {}
            }
        }

        // Fetch next chunk. The first attempt fires at the timing-prediction
        // sleep above; if it returns 404 (chunk not yet published) or a
        // transient transport error, the loop below applies the standard
        // exponential-backoff-with-jitter policy from `REALTIME_CHUNK_POLICY`.
        // The retry loop is inlined (rather than going through `with_retry`)
        // because each attempt borrows `iter` mutably and the resulting Future
        // can't escape an `FnMut` closure body.
        let chunk_fetch_start = web_time::Instant::now();
        let policy = &REALTIME_CHUNK_POLICY;
        let mut last_msg: Option<String> = None;
        // SyntheticVolumeEnd is reported through the retry loop as a
        // dedicated outcome the post-retry match has to surface; tagging the
        // verdict here lets the existing retry loop continue to drive 404 +
        // transient errors uniformly.
        let mut synthetic_volume_end = false;
        let fetch_outcome: Result<nexrad_data::aws::realtime::DownloadedChunk, String> = 'retry: {
            for attempt in 1..=policy.max_attempts {
                drain_control(&mut loop_state, &mut control_rx);
                if loop_state.stop_requested {
                    break 'retry Err("stopped".into());
                }
                if attempt > 1 {
                    none_retries = attempt - 1;
                    cur_last_empty_at = Some(current_timestamp_f64());
                }
                let verdict = match active_filter {
                    StreamingFilter::All => {
                        attempt_with_timeout(
                            async { classify_chunk_result(iter.try_next().await) },
                            policy.per_attempt_timeout,
                        )
                        .await
                    }
                    StreamingFilter::Elevation(_) => {
                        attempt_with_timeout(
                            async {
                                let outcome = iter
                                    .try_next_matching(
                                        // See the matching `accept_end=false`
                                        // comment on `next_matching_chunk_diagnostics`
                                        // above.
                                        false,
                                        |elev| active_filter.accepts(elev),
                                    )
                                    .await;
                                classify_filter_outcome(outcome)
                            },
                            policy.per_attempt_timeout,
                        )
                        .await
                    }
                };
                match verdict {
                    Verdict::Ok(FilterFetchResult::Downloaded(c)) => {
                        break 'retry Ok(c);
                    }
                    Verdict::Ok(FilterFetchResult::SyntheticEnd) => {
                        synthetic_volume_end = true;
                        break 'retry Err("synthetic_volume_end".into());
                    }
                    Verdict::Terminal(m) => break 'retry Err(m),
                    Verdict::Retry { after } => {
                        last_msg = Some(format!("attempt {} empty/transient", attempt));
                        if attempt >= policy.max_attempts {
                            break;
                        }
                        let elapsed = chunk_fetch_start.elapsed();
                        if elapsed >= policy.total_budget {
                            break 'retry Err(format!(
                                "chunk_fetch: budget {}s exhausted after {} attempts",
                                policy.total_budget.as_secs(),
                                attempt
                            ));
                        }
                        let mut delay = compute_delay(policy, attempt, after);
                        let remaining = policy.total_budget.saturating_sub(elapsed);
                        if delay > remaining {
                            delay = remaining;
                        }
                        sleep_duration(delay).await;
                    }
                }
            }
            Err(format!(
                "chunk_fetch: gave up after {} attempts ({})",
                policy.max_attempts,
                last_msg.as_deref().unwrap_or("retry exhausted")
            ))
        };

        if synthetic_volume_end {
            log::debug!(
                "streaming_loop: synthetic_volume_end emitted (filter={:?})",
                active_filter
            );
            // Emit a UI-only ChunkReceived so the timeline knows the volume
            // boundary even though no actual End chunk was downloaded. The
            // plan's `next_target` (which, under an elevation filter with
            // no current-volume match, points at the next volume's matching
            // chunk via the chained projection) is the single source for
            // the cross-volume countdown.
            chunks_in_volume += 1;
            let plan = build_engine_plan(&engine, iter.current_id(), current_timestamp_f64());
            let _ = results_tx.unbounded_send(RealtimeResult::ChunkReceived {
                chunks_in_volume,
                is_volume_end: true,
                fetch_latency_ms: 0.0,
                plan,
                arrival_stat: None,
            });
            ctx.request_repaint();
            // Reset per-chunk tracking; the next iteration will roll over to
            // the next volume's Start via the existing try_next path.
            none_retries = 0;
            cur_predicted_at = None;
            cur_last_empty_at = None;
            cur_forecast = None;
            cur_predicted_wait_secs = None;
            cur_plan_revision = None;
            cur_wait_resolution = crate::core::WaitResolution::SleptToPrediction;
            continue;
        }

        match fetch_outcome {
            Ok(chunk) => {
                let chunk_fetch_ms = chunk_fetch_start.elapsed().as_secs_f64() * 1000.0;
                let success_at = current_timestamp_f64();
                stats_tracker.update(&stats, &iter);

                let chunk_data = chunk.chunk.data().to_vec();
                let chunk_type = chunk.identifier.chunk_type();
                let is_end = chunk_type == ChunkType::End;
                let is_start = chunk_type == ChunkType::Start;

                // Reset counters on new volume
                if is_start {
                    let Some(header_secs) = volume_header_start_secs(&chunk_data) else {
                        log::error!(
                            "Realtime: start chunk for {} has no readable volume header — aborting stream",
                            site_id
                        );
                        let _ = results_tx.unbounded_send(RealtimeResult::Error(
                            "Start chunk has no readable volume header — cannot assign scan identity"
                                .to_string(),
                        ));
                        active.set(false);
                        ctx.request_repaint();
                        return;
                    };
                    chunks_in_volume = 0;
                    current_scan_start_secs = crate::data::ProvisionalStart(header_secs);
                    cache_volume_number(&site_id, *chunk.identifier.volume());
                    emitted_sequences_this_volume.clear();

                    // Volume boundary: rebuild the navigation mapper
                    // (StreamingState) AND hand the shared engine its
                    // stream-side boundary in one call (new VCP, anchor
                    // reset, inventory bound, scan start).
                    if let Some(vcp) = iter.install_vcp_from_start(&chunk.chunk) {
                        engine.borrow_mut().begin_volume(
                            vcp,
                            current_scan_start_secs.0,
                            *chunk.identifier.volume(),
                        );
                    } else {
                        engine
                            .borrow_mut()
                            .set_current_scan_start_secs(current_scan_start_secs.0);
                    }
                }

                chunks_in_volume += 1;
                emitted_sequences_this_volume.insert(chunk.identifier.sequence());

                // Feed the shared engine: this chunk is now known-available, and
                // its arrival interval feeds the timing-stats blend. Borrows are
                // scoped and never span an `.await`.
                if let Some(upload) = chunk.identifier.upload_date_time() {
                    let upload_secs = upload.timestamp_millis() as f64 / 1000.0;
                    let mut eng = engine.borrow_mut();
                    eng.observe_known_chunk(KnownChunk {
                        coord: ChunkCoord {
                            volume: *chunk.identifier.volume(),
                            sequence: chunk.identifier.sequence(),
                        },
                        upload_secs,
                        chunk_type,
                    });
                    if let Some(prev) = prev_upload_dt {
                        eng.record_inter_chunk_duration(&chunk.identifier, upload - prev, 1);
                    }
                    drop(eng);
                    prev_upload_dt = Some(upload);
                }

                let type_label: &'static str = if is_start {
                    "Start"
                } else if is_end {
                    "End"
                } else {
                    "Intermediate"
                };
                let s3_last_modified_at = chunk
                    .identifier
                    .upload_date_time()
                    .map(|dt| dt.timestamp_millis() as f64 / 1000.0);

                // Empirical NEXRAD ingest lag: difference between the chunk's
                // S3 upload time (AVAILABILITY) and its latest radial
                // collection time (ACTUAL).
                if let (Some(upload_secs), Some(collection_end_secs)) = (
                    s3_last_modified_at,
                    engine.borrow().collection_anchor_secs(),
                ) {
                    log::debug!(
                        "ingest lag: upload={:.3}s collection_end={:.3}s Δ={:+.3}s (seq={} type={})",
                        upload_secs,
                        collection_end_secs,
                        upload_secs - collection_end_secs,
                        chunks_in_volume,
                        type_label,
                    );
                }

                // Build the fresh plan *after* try_next advances `iter.current`,
                // so it describes the NEXT download from this point. Same
                // object feeds both the UI and the next loop iteration's
                // sleep target — keeping them in lock-step.
                let post_plan =
                    build_engine_plan(&engine, iter.current_id(), current_timestamp_f64());

                // Attach structural metadata for the chunk that just arrived
                // by looking it up in the fresh plan's current-volume slice.
                let (elevation_number, chunk_index_in_sweep, chunks_in_sweep) = post_plan
                    .as_ref()
                    .and_then(|p| {
                        p.current_volume_chunks
                            .iter()
                            .find(|c| c.sequence as u32 == chunks_in_volume)
                    })
                    .map(|c| {
                        (
                            c.elevation_number.map(|e| e as u8),
                            Some(c.chunk_index_in_sweep as u32),
                            Some(c.chunks_in_sweep as u32),
                        )
                    })
                    .unwrap_or((None, None, None));

                // Anchor source the projector was using for the *previous*
                // chunk — i.e. the one whose arrival we're recording.
                // Captured AFTER `try_next` advances `iter.current` is fine
                // because `current_anchor_source` reads collection-end +
                // median-lag state, both of which are independent of which
                // chunk is "current".
                let anchor_source = Some(engine.borrow().current_anchor_source());

                // The forecast that produced this chunk's sleep target was
                // captured into `cur_forecast` on the chunk's first
                // iteration. Map its fields into the arrival stat so the
                // diagnostics modal can compare predicted vs. observed.
                let (bucket_key, stats_n_at_prediction, scheduler_path, physics_breakdown) =
                    match cur_forecast.as_ref() {
                        Some(f) => (
                            f.bucket
                                .as_ref()
                                .map(crate::core::BucketKey::from_characteristics),
                            f.stats_n,
                            Some(f.scheduler_path),
                            Some(f.physics_breakdown),
                        ),
                        None => (None, 0, None, None),
                    };

                let arrival_stat = crate::core::ChunkArrivalStat {
                    sequence: chunks_in_volume,
                    wait_resolution: cur_wait_resolution,
                    chunk_type: type_label,
                    elevation_number,
                    chunk_index_in_sweep,
                    chunks_in_sweep,
                    predicted_available_at: cur_predicted_at,
                    empty_polls: none_retries,
                    last_empty_poll_at: cur_last_empty_at,
                    s3_last_modified_at,
                    success_at,
                    bucket_key,
                    stats_n_at_prediction,
                    scheduler_path,
                    physics_breakdown,
                    anchor_source,
                    availability_lag_ms: None,
                    collection_time_secs: None,
                    predicted_wait_secs: cur_predicted_wait_secs,
                    predicted_with_plan_revision: cur_plan_revision,
                };

                // Reset tracking state for the next chunk
                none_retries = 0;
                cur_predicted_at = None;
                cur_last_empty_at = None;
                cur_forecast = None;
                cur_predicted_wait_secs = None;
                cur_plan_revision = None;
                cur_wait_resolution = crate::core::WaitResolution::SleptToPrediction;
                let chunk_is_last_in_sweep = iter
                    .chunk_metadata(chunk.identifier.sequence())
                    .map(|m| m.is_last_in_sweep());

                // Emit the raw chunk for incremental ingest
                let _ = results_tx.unbounded_send(RealtimeResult::ChunkData {
                    data: chunk_data,
                    chunk_index: chunks_in_volume - 1,
                    is_start,
                    is_end,
                    timestamp: current_scan_start_secs.0,
                    is_last_in_sweep: chunk_is_last_in_sweep,
                });
                // Emit UI status update
                let _ = results_tx.unbounded_send(RealtimeResult::ChunkReceived {
                    chunks_in_volume,
                    is_volume_end: is_end,
                    fetch_latency_ms: chunk_fetch_ms,
                    plan: post_plan,
                    arrival_stat: Some(arrival_stat),
                });

                save_timing_stats(&site_id, engine.borrow().timing_stats());

                ctx.request_repaint();
            }
            Err(msg) => {
                log::error!("Streaming error: {}", msg);
                let _ = results_tx.unbounded_send(RealtimeResult::Error(msg));
                active.set(false);
                ctx.request_repaint();
                break;
            }
        }
    }

    active.set(false);
}

/// Either an actual downloaded chunk or a synthetic-volume-end signal from
/// the filter-aware fetch path. Plumbed through the retry loop's `Verdict`
/// so the existing 404 / transient-error handling stays unchanged.
#[derive(Debug)]
enum FilterFetchResult {
    Downloaded(nexrad_data::aws::realtime::DownloadedChunk),
    SyntheticEnd,
}

/// Map a [`crate::nexrad::live::streaming_state::TryNextOutcome`] to a retry [`Verdict`]
/// for the filter-aware fetch path. Mirrors [`classify_chunk_result`] for
/// the unfiltered path; the only new case is `SyntheticVolumeEnd`, which is
/// not a retry — it's a terminal-for-this-iteration outcome the loop turns
/// into a synthetic `is_volume_end` signal.
fn classify_filter_outcome(
    result: nexrad_data::result::Result<crate::nexrad::live::streaming_state::TryNextOutcome>,
) -> Verdict<FilterFetchResult> {
    use crate::nexrad::live::streaming_state::TryNextOutcome;
    use nexrad_data::result::aws::AWSError;
    use nexrad_data::result::Error;
    match result {
        Ok(TryNextOutcome::Downloaded(c)) => Verdict::Ok(FilterFetchResult::Downloaded(c)),
        Ok(TryNextOutcome::NotYetAvailable) => Verdict::Retry { after: None },
        Ok(TryNextOutcome::SyntheticVolumeEnd) => Verdict::Ok(FilterFetchResult::SyntheticEnd),
        Err(Error::AWS(
            AWSError::S3GetObjectRequest(_)
            | AWSError::S3GetObject(_)
            | AWSError::S3Streaming(_)
            | AWSError::S3ListObjects(_)
            | AWSError::TruncatedListObjectsResponse,
        )) => Verdict::Retry { after: None },
        Err(e) => Verdict::Terminal(format!("{}", e)),
    }
}

/// Map a `nexrad-data` chunk-fetch result to a retry [`Verdict`].
///
/// `Ok(None)` (S3 returned 404 — chunk not yet published) is treated as a
/// retryable miss in this call site, since real-time chunks land seconds late
/// by design. Transport-layer errors are also retryable; data-decoding and
/// identifier errors are terminal.
fn classify_chunk_result(
    result: nexrad_data::result::Result<Option<nexrad_data::aws::realtime::DownloadedChunk>>,
) -> Verdict<FilterFetchResult> {
    use nexrad_data::result::aws::AWSError;
    use nexrad_data::result::Error;
    match result {
        Ok(Some(chunk)) => Verdict::Ok(FilterFetchResult::Downloaded(chunk)),
        Ok(None) => Verdict::Retry { after: None },
        Err(Error::AWS(
            AWSError::S3GetObjectRequest(_)
            | AWSError::S3GetObject(_)
            | AWSError::S3Streaming(_)
            | AWSError::S3ListObjects(_)
            | AWSError::TruncatedListObjectsResponse,
        )) => Verdict::Retry { after: None },
        Err(e) => Verdict::Terminal(format!("{}", e)),
    }
}

/// Sleep in increments, draining the control channel and watching for
/// stop + filter-change signals between increments. Returns the reason
/// the sleep ended.
///
/// `wake_epoch` is the `filter_epoch` value the caller observed when it
/// decided how long to sleep — if `drain_control` bumps it past that
/// value mid-sleep, the filter has been mutated and the caller should
/// re-evaluate.
async fn interruptible_sleep(
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
    ctx: &egui::Context,
    total_ms: u32,
    wake_epoch: u64,
) -> SleepOutcome {
    const INCREMENT: u32 = 250;
    let mut remaining = total_ms;

    while remaining > 0 {
        drain_control(loop_state, control_rx);
        if loop_state.stop_requested {
            return SleepOutcome::Stopped;
        }
        if loop_state.filter_epoch != wake_epoch {
            return SleepOutcome::FilterChanged;
        }

        ctx.request_repaint();

        let sleep_time = INCREMENT.min(remaining);
        sleep_ms(sleep_time).await;
        remaining = remaining.saturating_sub(INCREMENT);
    }

    drain_control(loop_state, control_rx);
    if loop_state.stop_requested {
        SleepOutcome::Stopped
    } else if loop_state.filter_epoch != wake_epoch {
        SleepOutcome::FilterChanged
    } else {
        SleepOutcome::Completed
    }
}

// ── Adaptive cross-volume wait (list-probe early-fire + re-anchor) ─────────

/// Minimum predicted wait (seconds) before the adaptive list-probe engages.
/// Below this the single-sleep path is used verbatim — short / same-volume
/// waits don't accumulate enough projection error to be worth an S3 list call.
const LIST_PROBE_THRESHOLD_SECS: f64 = 30.0;

/// Cadence (seconds) of the list probe during a long cross-volume wait. Also
/// the worst-case overshoot: once the target is published, the next probe
/// fires within this window and breaks the sleep. Trades S3 list calls for
/// timing tightness.
const LIST_PROBE_CADENCE_SECS: f64 = 20.0;

/// Whether the adaptive list-probe should engage for this wait. Any long wait
/// (cross-volume OR a filtered current-volume gap) benefits: cross-volume from
/// the re-anchor correction, current-volume from flipping a future cut to
/// `AvailableNotCollected` live as soon as it publishes. The caller lists the
/// volume the target lives in. Pure — split out for testing.
fn should_list_now(wait_ms: u32, _target_in_next_volume: bool, threshold_ms: u32) -> bool {
    wait_ms > threshold_ms
}

/// Whether the next-volume slot has started writing the *new* volume (vs. still
/// holding the previous occupant of this rotating slot). The new volume's
/// chunks upload strictly later than our current-volume anchor, while a
/// recycled old occupant uploaded long ago — so "strictly newer than the
/// anchor" is the rollover signal. Reuses [`probe_should_advance`]. Pure.
fn slot_is_fresh(prev_anchor_upload_secs: f64, slot_newest_upload_secs: Option<f64>) -> bool {
    probe_should_advance(prev_anchor_upload_secs, slot_newest_upload_secs)
}

/// Whether the target chunk is already published in the listing. Presence is by
/// sequence: the target sequence was resolved by the projector to the user's
/// filtered elevation, and chunks publish in order, so any published sequence
/// `>= target_seq` means the target is available. An End chunk also fires
/// (the volume finished — possibly shorter than projected — and the fetch path
/// will roll over). Pure — split out for testing.
fn target_present_in_listing(
    listed_seqs: &[usize],
    listed_has_end: bool,
    target_seq: usize,
) -> bool {
    listed_has_end || listed_seqs.iter().any(|&s| s >= target_seq)
}

/// Convert a fresh projected poll time into a remaining-wait in milliseconds,
/// clamped to zero. Pure — split out for testing.
fn recompute_remaining_wait_ms(new_poll_at_secs: f64, now_secs: f64) -> u32 {
    ((new_poll_at_secs - now_secs).max(0.0) * 1000.0) as u32
}

/// Newest-published chunk's S3 upload time (Unix seconds) from a listing, or
/// `None` when the slot is empty / carries no upload metadata. Assumes the
/// listing is sequence-sorted (S3 ListObjects is lexicographic), so the last
/// entry is newest — same assumption as [`slot_newest_upload_secs`].
fn listing_newest_upload_secs(
    listed: &[nexrad_data::aws::realtime::ChunkIdentifier],
) -> Option<f64> {
    listed
        .last()?
        .upload_date_time()
        .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
}

/// Sleep until the next download target should be available, periodically
/// listing the next-volume S3 slot to (a) early-fire the moment the target is
/// published and (b) re-anchor the remaining-wait projection on a freshly
/// published chunk — correcting the accumulated-error overshoot of the
/// single-sleep path.
///
/// Returns the terminal [`SleepOutcome`] (so the caller keeps its existing
/// stop/filter-change/completed handling) plus how the wait resolved (for
/// per-chunk arrival diagnostics). Never mutates the download cursor: re-anchor
/// goes through [`StreamingState::build_plan_from_anchor`], which leaves
/// `self.current` intact.
///
/// `prev_anchor_upload_secs` is the current-volume anchor's S3 upload time, the
/// reference for the rotating-slot freshness guard.
#[allow(clippy::too_many_arguments)]
async fn wait_for_next_target(
    site_id: &str,
    engine: &SharedProjectionEngine,
    cursor_anchor: &ChunkIdentifier,
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
    ctx: &egui::Context,
    initial_wait_ms: u32,
    target_seq: usize,
    target_volume: nexrad_data::aws::realtime::VolumeIndex,
    prev_anchor_upload_secs: Option<f64>,
    wake_epoch: u64,
) -> (SleepOutcome, crate::core::WaitResolution) {
    use crate::core::WaitResolution;
    use nexrad_data::aws::realtime::{list_chunks_in_volume, ChunkType};

    let cadence_ms = (LIST_PROBE_CADENCE_SECS * 1000.0) as u32;
    let mut remaining_ms = initial_wait_ms;
    let mut resolution = WaitResolution::SleptToPrediction;

    loop {
        // Final approach: within one cadence of the target, just sleep the
        // remainder — no point listing right before the fetch attempt.
        if remaining_ms <= cadence_ms {
            let outcome =
                interruptible_sleep(loop_state, control_rx, ctx, remaining_ms, wake_epoch).await;
            return (outcome, resolution);
        }

        // Sleep one segment; bail immediately on stop / filter change.
        match interruptible_sleep(loop_state, control_rx, ctx, cadence_ms, wake_epoch).await {
            SleepOutcome::Completed => {}
            other => return (other, resolution),
        }
        remaining_ms = remaining_ms.saturating_sub(cadence_ms);

        // Re-check stop before spending a network request.
        drain_control(loop_state, control_rx);
        if loop_state.stop_requested {
            return (SleepOutcome::Stopped, resolution);
        }

        // Probe the next-volume slot. A failure is non-fatal: keep the
        // existing schedule, so we're never worse than the single-sleep path.
        let listed = match list_chunks_in_volume(site_id, target_volume, 100).await {
            Ok(ids) => ids,
            Err(e) => {
                log::warn!(
                    "list-probe: list_chunks_in_volume failed: {} — continuing scheduled wait",
                    e
                );
                continue;
            }
        };

        // Rotating-slot freshness guard: skip both early-fire and re-anchor
        // until the slot actually holds the new volume.
        let slot_newest = listing_newest_upload_secs(&listed);
        let fresh = match prev_anchor_upload_secs {
            Some(prev) => slot_is_fresh(prev, slot_newest),
            None => slot_newest.is_some(),
        };
        if !fresh {
            log::debug!("list-probe: next-volume slot not fresh yet (rollover pending)");
            continue;
        }

        // Feed the listing into the shared inventory so every surface (not just
        // this loop's sleep) reflects what's now published — this is what makes
        // re-anchoring visible in the timeline / VCP panel.
        engine.borrow_mut().observe_listing(target_volume, &listed);

        let listed_seqs: Vec<usize> = listed.iter().map(|c| c.sequence()).collect();
        let has_end = listed.iter().any(|c| c.chunk_type() == ChunkType::End);
        if target_present_in_listing(&listed_seqs, has_end, target_seq) {
            log::debug!(
                "list-probe: early-fire (target seq {} present in next-volume slot)",
                target_seq
            );
            return (SleepOutcome::Completed, WaitResolution::EarlyFired);
        }

        // Re-anchor the remaining wait. We already fed the listing into the
        // inventory above, so the engine's normal cursor-anchored projection
        // now self-anchors the next volume on that fresh measurement — fresh
        // timing in the right frame (offset 1), no separate anchor. Recompute
        // the remaining wait from it.
        let now2 = current_timestamp_f64();
        let new_poll = engine
            .borrow_mut()
            .projection(cursor_anchor, now2)
            .and_then(|p| {
                p.next_target()
                    .and_then(|t| t.projected.as_ref())
                    .map(|f| f.poll_at_secs)
            });
        if let Some(new_poll_at) = new_poll {
            let new_remaining = recompute_remaining_wait_ms(new_poll_at, now2);
            log::debug!(
                "list-probe: re-anchored from inventory — remaining wait {}ms (was {}ms)",
                new_remaining,
                remaining_ms,
            );
            remaining_ms = new_remaining;
            resolution = WaitResolution::ReAnchored;
        }
    }
}

struct StatsTracker {
    last_requests: usize,
    last_bytes: u64,
}

impl StatsTracker {
    fn new(state: &StreamingState) -> Self {
        Self {
            last_requests: state.requests_made(),
            last_bytes: state.bytes_downloaded(),
        }
    }

    fn update(&mut self, stats: &NetworkStats, state: &StreamingState) {
        let new_requests = state.requests_made().saturating_sub(self.last_requests);
        let new_bytes = state.bytes_downloaded().saturating_sub(self.last_bytes);

        for _ in 0..new_requests {
            stats.request_started();
            stats.request_completed(0);
        }
        if new_bytes > 0 {
            *stats.total_bytes.borrow_mut() += new_bytes;
        }

        self.last_requests = state.requests_made();
        self.last_bytes = state.bytes_downloaded();
    }
}

/// Unix seconds with millisecond precision — for diagnostics timestamps.
fn current_timestamp_f64() -> f64 {
    js_sys::Date::now() / 1000.0
}

// ── Volume number cache ────────────────────────────────────────────────

/// How recent the cached volume hint must be (in seconds) for the fast-path
/// resume to trust it. A volume is one VCP (~4–10 min); within ~20 min the true
/// latest is only a few slots ahead, so the forward-probe walk costs far fewer
/// list calls than the library's binary search. Past this the hint is ignored
/// and discovery falls back to `get_latest_volume`.
const VOLUME_HINT_MAX_AGE_SECS: f64 = 20.0 * 60.0;

fn volume_cache_key(site_id: &str) -> String {
    format!("nexrad_volume_{}", site_id)
}

/// Serialize a `(volume, cached-at seconds)` pair to the JSON form stored in
/// localStorage. Pure — split out for testing.
fn encode_volume_cache(volume: usize, cached_at_secs: f64) -> String {
    format!("{{\"v\":{},\"t\":{}}}", volume, cached_at_secs)
}

/// Parse the cached volume value. Returns `(volume, cached-at seconds)` for the
/// current JSON form. Legacy bare-number entries (older builds) carry no
/// timestamp, so they return `None` and are simply ignored — the next Start
/// chunk rewrites them in the new form. Pure — split out for testing.
fn decode_volume_cache(raw: &str) -> Option<(nexrad_data::aws::realtime::VolumeIndex, f64)> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let v = value.get("v")?.as_u64()? as usize;
    let t = value.get("t")?.as_f64()?;
    if (1..=999).contains(&v) {
        Some((nexrad_data::aws::realtime::VolumeIndex::new(v), t))
    } else {
        None
    }
}

/// Cache the latest volume number (with the current wall-clock time) in
/// localStorage for fast resume.
fn cache_volume_number(site_id: &str, volume: nexrad_data::aws::realtime::VolumeIndex) {
    if let Some(window) = web_sys::window() {
        if let Ok(Some(storage)) = window.local_storage() {
            let payload = encode_volume_cache(volume.as_number(), current_timestamp_f64());
            let _ = storage.set_item(&volume_cache_key(site_id), &payload);
        }
    }
}

/// Read the cached volume hint for a site: the slot and its cached-at seconds.
/// Returns `None` when absent, malformed, or in the legacy timestamp-less form.
fn get_cached_volume_hint(site_id: &str) -> Option<(nexrad_data::aws::realtime::VolumeIndex, f64)> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let raw = storage.get_item(&volume_cache_key(site_id)).ok()??;
    decode_volume_cache(&raw)
}

/// Decide whether the forward-probe should advance from the current candidate to
/// the next slot, given each slot's newest-chunk S3 upload time (seconds). We
/// advance only while the next slot is strictly newer; a not-yet-written or
/// recycled-older slot stops the walk. Pure — split out for testing.
fn probe_should_advance(candidate_upload_secs: f64, next_upload_secs: Option<f64>) -> bool {
    matches!(next_upload_secs, Some(next) if next > candidate_upload_secs)
}

/// Newest-chunk S3 upload time (seconds) for a volume slot, or `None` if the
/// slot has no chunks / no upload metadata.
async fn slot_newest_upload_secs(
    site_id: &str,
    volume: nexrad_data::aws::realtime::VolumeIndex,
) -> Option<f64> {
    let chunks = nexrad_data::aws::realtime::list_chunks_in_volume(site_id, volume, 100)
        .await
        .ok()?;
    chunks
        .last()?
        .upload_date_time()
        .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
}

/// Forward-probe from a fresh cache hint to the true latest volume, comparing S3
/// upload times (list-only, no chunk downloads). Returns the resolved volume and
/// the number of list calls made (threaded into `init_at_volume` for accurate
/// request accounting). Returns `None` — forcing the caller to fall back to the
/// library's binary search — when the hint slot itself is empty or its upload
/// time is already older than [`VOLUME_HINT_MAX_AGE_SECS`].
async fn probe_latest_from_hint(
    site_id: &str,
    hint: nexrad_data::aws::realtime::VolumeIndex,
) -> Option<(nexrad_data::aws::realtime::VolumeIndex, usize)> {
    let mut calls = 1;
    let mut candidate = hint;
    let mut candidate_upload = slot_newest_upload_secs(site_id, hint).await?;

    // Guard against a slot that survived the recency gate by cached-at time but
    // whose data is actually stale (e.g. the site went offline): if the hint's
    // own newest chunk is old, don't trust it — fall back to a full search.
    if current_timestamp_f64() - candidate_upload >= VOLUME_HINT_MAX_AGE_SECS {
        return None;
    }

    loop {
        let next = candidate.next();
        let next_upload = slot_newest_upload_secs(site_id, next).await;
        calls += 1;
        if probe_should_advance(candidate_upload, next_upload) {
            candidate = next;
            candidate_upload = next_upload.expect("Some by probe_should_advance");
        } else {
            break;
        }
    }

    Some((candidate, calls))
}

// ── Timing stats persistence ──────────────────────────────────────────────

fn timing_stats_key(site_id: &str) -> String {
    format!("nexrad_timing_stats_{}", site_id)
}

/// Persist the site's rolling chunk-timing statistics to localStorage so the
/// next session starts warm instead of cold-starting from pure physics.
fn save_timing_stats(site_id: &str, stats: &crate::core::timing::ChunkTimingStats) {
    let Some(json) = stats.to_json() else {
        return;
    };
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(storage)) = window.local_storage() else {
        return;
    };
    let _ = storage.set_item(&timing_stats_key(site_id), &json);
}

/// Read a previously-persisted timing stats snapshot for the site, if any.
fn load_cached_timing_stats(site_id: &str) -> Option<crate::core::timing::ChunkTimingStats> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let raw = storage.get_item(&timing_stats_key(site_id)).ok()??;
    crate::core::timing::ChunkTimingStats::from_json(&raw)
}

/// Discover the latest volume for a site and initialize a [`StreamingState`] at
/// it.
///
/// Fast path: if [`get_cached_volume_hint`] returns a hint cached within
/// [`VOLUME_HINT_MAX_AGE_SECS`], forward-probe from it ([`probe_latest_from_hint`])
/// — usually 1–3 list calls — and init there. Any miss (no hint, stale, empty
/// slot, or stale slot data) falls through to the library's rotated-array binary
/// search via `get_latest_volume`, so there is no correctness regression.
async fn acquire_streaming_state(
    site_id: &str,
) -> nexrad_data::result::Result<crate::nexrad::live::streaming_state::StreamingInit> {
    if let Some((hint, cached_at)) = get_cached_volume_hint(site_id) {
        if current_timestamp_f64() - cached_at < VOLUME_HINT_MAX_AGE_SECS {
            if let Some((volume, calls)) = probe_latest_from_hint(site_id, hint).await {
                log::debug!(
                    "Realtime: resumed {} from cached volume hint {} → latest {} ({} list calls)",
                    site_id,
                    hint.as_number(),
                    volume.as_number(),
                    calls
                );
                return StreamingState::init_at_volume(site_id, volume, calls).await;
            }
        }
    }

    let result = nexrad_data::aws::realtime::get_latest_volume(site_id).await?;
    let volume = result.volume.ok_or(nexrad_data::result::Error::AWS(
        nexrad_data::result::aws::AWSError::LatestVolumeNotFound,
    ))?;
    StreamingState::init_at_volume(site_id, volume, result.calls).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn volume_cache_round_trips_through_json() {
        let encoded = encode_volume_cache(347, 1_700_000_000.5);
        let (vol, t) = decode_volume_cache(&encoded).expect("decodes own output");
        assert_eq!(vol.as_number(), 347);
        assert_eq!(t, 1_700_000_000.5);
    }

    #[wasm_bindgen_test]
    fn legacy_bare_number_is_ignored() {
        // Older builds wrote a bare decimal (no timestamp). It must decode to
        // None so the fast path is skipped until the entry is rewritten.
        assert!(decode_volume_cache("347").is_none());
        assert!(decode_volume_cache("VolumeIndex(347)").is_none());
    }

    #[wasm_bindgen_test]
    fn out_of_range_volume_rejected() {
        assert!(decode_volume_cache(&encode_volume_cache(0, 1.0)).is_none());
        assert!(decode_volume_cache(&encode_volume_cache(1000, 1.0)).is_none());
        assert!(decode_volume_cache("garbage").is_none());
    }

    #[wasm_bindgen_test]
    fn recency_gate_boundary() {
        let now = 1_700_001_200.0;
        // Just inside the 20-minute window is fresh.
        let cached_at = now - (VOLUME_HINT_MAX_AGE_SECS - 1.0);
        assert!(now - cached_at < VOLUME_HINT_MAX_AGE_SECS);
        // Just outside is stale.
        let cached_at = now - (VOLUME_HINT_MAX_AGE_SECS + 1.0);
        assert!(now - cached_at >= VOLUME_HINT_MAX_AGE_SECS);
    }

    #[wasm_bindgen_test]
    fn should_list_now_for_any_long_wait() {
        let threshold = 30_000;
        // Above threshold → probe (cross-volume or same-volume).
        assert!(should_list_now(45_000, true, threshold));
        assert!(should_list_now(120_000, false, threshold));
        // Below / at threshold → no probe.
        assert!(!should_list_now(20_000, true, threshold));
        assert!(!should_list_now(30_000, false, threshold));
    }

    #[wasm_bindgen_test]
    fn slot_is_fresh_detects_rollover() {
        // New volume's chunks upload later than our anchor → fresh.
        assert!(slot_is_fresh(100.0, Some(150.0)));
        // Slot empty / no metadata → not fresh.
        assert!(!slot_is_fresh(100.0, None));
        // Recycled older occupant (uploaded long ago) → not fresh.
        assert!(!slot_is_fresh(100.0, Some(50.0)));
        // Equal upload time (no progress) → not fresh.
        assert!(!slot_is_fresh(100.0, Some(100.0)));
    }

    #[wasm_bindgen_test]
    fn target_present_in_listing_fires_on_seq_or_end() {
        // Exact target sequence present → fire.
        assert!(target_present_in_listing(&[1, 2, 3], false, 3));
        // A higher sequence present → fire (target already passed).
        assert!(target_present_in_listing(&[1, 2, 3, 4], false, 3));
        // Target not yet reached → wait.
        assert!(!target_present_in_listing(&[1, 2], false, 3));
        // End chunk present (volume shorter than projected) → fire.
        assert!(target_present_in_listing(&[1, 2], true, 9));
        // Empty listing → wait.
        assert!(!target_present_in_listing(&[], false, 3));
    }

    #[wasm_bindgen_test]
    fn recompute_remaining_wait_clamps_to_zero() {
        // Future poll time → positive remaining wait in ms.
        assert_eq!(recompute_remaining_wait_ms(105.0, 100.0), 5_000);
        // Poll time already passed → clamps to zero (fetch immediately).
        assert_eq!(recompute_remaining_wait_ms(95.0, 100.0), 0);
        // Exactly now → zero.
        assert_eq!(recompute_remaining_wait_ms(100.0, 100.0), 0);
    }

    #[wasm_bindgen_test]
    fn probe_advances_only_to_strictly_newer_slots() {
        // Next slot newer → advance.
        assert!(probe_should_advance(100.0, Some(150.0)));
        // Next slot not yet written → stop.
        assert!(!probe_should_advance(100.0, None));
        // Next slot is an older recycled slot (e.g. wrap to slot 1) → stop.
        assert!(!probe_should_advance(100.0, Some(50.0)));
        // Equal upload time (no progress) → stop.
        assert!(!probe_should_advance(100.0, Some(100.0)));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── LoopState / drain_control state machine ────────────────────────────
    //
    // `drain_control` is the loop's control-channel state machine: it folds
    // every queued `ControlMessage` into a `LoopState` and reports whether the
    // active filter changed. The existing `mod tests` covers only the pure
    // probe/cache helpers, never this. We drive it through an in-process
    // unbounded channel (no async, no browser): keeping the sender alive means
    // `try_recv` returns `Err(Empty)` on drain-out, exactly the production path.

    #[wasm_bindgen_test]
    fn loop_state_starts_unstopped_all_filter() {
        let ls = LoopState::new();
        assert!(!ls.stop_requested);
        assert!(ls.active_filter == StreamingFilter::All);
        assert_eq!(ls.filter_epoch, 0);
    }

    #[wasm_bindgen_test]
    fn drain_control_empty_channel_is_noop() {
        let (_tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(!changed);
        assert!(!ls.stop_requested);
        assert!(ls.active_filter == StreamingFilter::All);
        assert_eq!(ls.filter_epoch, 0);
    }

    #[wasm_bindgen_test]
    fn drain_control_stop_sets_flag_without_filter_change() {
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::Stop);
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        // Stop is not a filter change.
        assert!(!changed);
        assert!(ls.stop_requested);
        // Filter + epoch untouched by a Stop.
        assert!(ls.active_filter == StreamingFilter::All);
        assert_eq!(ls.filter_epoch, 0);
    }

    #[wasm_bindgen_test]
    fn drain_control_filter_change_bumps_epoch_and_reports_change() {
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(3)));
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.active_filter == StreamingFilter::Elevation(3));
        // One real change → epoch 0 → 1.
        assert_eq!(ls.filter_epoch, 1);
        assert!(!ls.stop_requested);
    }

    #[wasm_bindgen_test]
    fn drain_control_redundant_filter_is_noop() {
        // Setting the filter to its current value must NOT bump the epoch or
        // report a change (mirrors the old `pending_filter == filter` guard).
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::All));
        let mut ls = LoopState::new(); // already All
        let changed = drain_control(&mut ls, &mut rx);
        assert!(!changed);
        assert_eq!(ls.filter_epoch, 0);
        assert!(ls.active_filter == StreamingFilter::All);
    }

    #[wasm_bindgen_test]
    fn drain_control_coalesces_multiple_distinct_changes() {
        // Two distinct changes queued before a single drain → both applied,
        // epoch counts each real transition, final value is the last one.
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(1)));
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(2)));
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.active_filter == StreamingFilter::Elevation(2));
        // 0 → 1 (to Elevation(1)) → 2 (to Elevation(2)).
        assert_eq!(ls.filter_epoch, 2);
    }

    #[wasm_bindgen_test]
    fn drain_control_duplicate_change_only_bumps_once() {
        // Same target sent twice: first transition counts, the second is a
        // no-op against the now-current value.
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(5)));
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(5)));
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.active_filter == StreamingFilter::Elevation(5));
        assert_eq!(ls.filter_epoch, 1);
    }

    #[wasm_bindgen_test]
    fn drain_control_change_back_to_all_is_a_real_change() {
        // Starting from Elevation, a SetFilter(All) is a genuine transition.
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::All));
        let mut ls = LoopState::new();
        ls.active_filter = StreamingFilter::Elevation(4);
        // Pretend we'd already advanced the epoch once before this drain.
        ls.filter_epoch = 1;
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.active_filter == StreamingFilter::All);
        assert_eq!(ls.filter_epoch, 2);
    }

    #[wasm_bindgen_test]
    fn drain_control_applies_stop_and_filter_together() {
        // A filter change AND a stop queued in the same drain: both land. The
        // change is reported and the stop flag is set.
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(7)));
        let _ = tx.unbounded_send(ControlMessage::Stop);
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.stop_requested);
        assert!(ls.active_filter == StreamingFilter::Elevation(7));
        assert_eq!(ls.filter_epoch, 1);
    }

    // ── localStorage key derivation (pure formatting) ─────────────────────

    #[wasm_bindgen_test]
    fn volume_cache_key_is_site_namespaced() {
        assert_eq!(volume_cache_key("KTLX"), "nexrad_volume_KTLX");
        assert_eq!(volume_cache_key("KFWS"), "nexrad_volume_KFWS");
        // Distinct sites must not collide.
        assert!(volume_cache_key("KTLX") != volume_cache_key("KFWS"));
    }

    #[wasm_bindgen_test]
    fn timing_stats_key_is_site_namespaced() {
        assert_eq!(timing_stats_key("KTLX"), "nexrad_timing_stats_KTLX");
        assert_eq!(timing_stats_key("KOUN"), "nexrad_timing_stats_KOUN");
        // The two key families never collide for the same site.
        assert!(timing_stats_key("KTLX") != volume_cache_key("KTLX"));
    }

    // ── decode_volume_cache: gaps the existing tests leave open ────────────

    #[wasm_bindgen_test]
    fn decode_volume_cache_accepts_range_endpoints() {
        // The inclusive range is 1..=999; both endpoints decode.
        match decode_volume_cache(&encode_volume_cache(1, 10.0)) {
            Some((vol, t)) => {
                assert_eq!(vol.as_number(), 1);
                assert_eq!(t, 10.0);
            }
            None => panic!("v=1 should be accepted"),
        }
        match decode_volume_cache(&encode_volume_cache(999, 20.0)) {
            Some((vol, t)) => {
                assert_eq!(vol.as_number(), 999);
                assert_eq!(t, 20.0);
            }
            None => panic!("v=999 should be accepted"),
        }
    }

    #[wasm_bindgen_test]
    fn decode_volume_cache_requires_timestamp_field() {
        // Valid volume but missing the `t` field → None (incomplete entry).
        assert!(decode_volume_cache("{\"v\":42}").is_none());
    }

    #[wasm_bindgen_test]
    fn decode_volume_cache_requires_volume_field() {
        // Timestamp present but no `v` → None.
        assert!(decode_volume_cache("{\"t\":1700000000.0}").is_none());
    }

    #[wasm_bindgen_test]
    fn decode_volume_cache_rejects_wrong_typed_fields() {
        // `v` as a string is not a u64 → None.
        assert!(decode_volume_cache("{\"v\":\"42\",\"t\":1.0}").is_none());
        // `t` as a non-number → None.
        assert!(decode_volume_cache("{\"v\":42,\"t\":\"soon\"}").is_none());
        // A JSON array (no object fields) → None.
        assert!(decode_volume_cache("[42,1.0]").is_none());
    }
}
