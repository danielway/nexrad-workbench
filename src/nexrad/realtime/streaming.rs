//! Streaming-loop implementation and private helpers for `RealtimeChannel`.
//!
//! This module contains the long-running `streaming_loop` async task that
//! polls AWS for new chunks, applies the projector, and dispatches results
//! back through the channel. All other functions here are helpers called
//! from `streaming_loop` (or from `super::mod`'s `RealtimeChannel::start`).

use super::{ControlMessage, RealtimeResult};
use crate::data::facade::DataFacade;
use crate::net::retry::{
    attempt_with_timeout, compute_delay, sleep_duration, sleep_ms, Verdict, REALTIME_CHUNK_POLICY,
};
use crate::nexrad::download::NetworkStats;
use crate::nexrad::streaming_filter::StreamingFilter;
use crate::nexrad::streaming_state::StreamingState;
use eframe::egui;
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures_util::future::join_all;
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
pub enum SleepOutcome {
    Completed,
    Stopped,
    FilterChanged,
}

/// Default provisional lag applied when we have no observed median lag
/// yet. Matches the default in the projector so a cold stream's first
/// ScanKey lands near the eventual real value.
const DEFAULT_PROVISIONAL_LAG_SECS: f64 = 5.0;

/// Provisional scan-start timestamp (Unix seconds, sub-second precision)
/// for a new volume.
///
/// Uses the Start chunk's S3 upload time minus the current median
/// availability lag from `ChunkTimingStats` (falling back to
/// `DEFAULT_PROVISIONAL_LAG_SECS`). That lands close to the real volume
/// header collection time — closer than the wall-clock receipt time it
/// replaces — without needing to wait for the first M chunk's radial
/// parse. If there is no upload time, fall back to wall clock.
///
/// Returns `f64` rather than `i64` so the IDB scan key preserves the
/// volume's true sub-second start time end-to-end. Without this, two
/// volumes whose true starts differ by less than a second would round to
/// the same `i64` and risk colliding under `ScanKey::from_secs`.
fn provisional_scan_start_secs(
    start_upload: Option<chrono::DateTime<chrono::Utc>>,
    iter: &StreamingState,
) -> f64 {
    let median_lag_secs = iter
        .timing_stats()
        .median_availability_lag_secs()
        .unwrap_or(DEFAULT_PROVISIONAL_LAG_SECS);
    if let Some(upload) = start_upload {
        let upload_secs = upload.timestamp_millis() as f64 / 1000.0;
        return upload_secs - median_lag_secs;
    }
    current_timestamp() as f64
}

/// Elevation numbers already fully cached in IndexedDB for the given scan.
///
/// A "fully cached" sweep is one that's been flushed to the `cached_sweeps`
/// list (via an `is_last_in_sweep` or end-of-volume flush). Partial sweeps
/// don't appear here, so on resume we still re-download chunks for sweeps
/// that were interrupted mid-flight.
async fn cached_elevations_for_scan(
    facade: &DataFacade,
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
    targets: &[nexrad_data::aws::realtime::ChunkIdentifier],
    iter: &mut StreamingState,
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
            plan: iter.build_plan(current_timestamp_f64()),
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
    facade: &DataFacade,
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
/// ingest) and apply each to the `StreamingState`'s projector. The
/// dispatch shape — match on [`crate::nexrad::ProjectorObservation`] variant,
/// call the matching projector method — is intentionally explicit so
/// adding a new observation kind is just one new arm here.
fn drain_pending_observations(
    observations_rx: &mut UnboundedReceiver<crate::nexrad::ProjectorObservation>,
    iter: &mut StreamingState,
) {
    while let Ok(obs) = observations_rx.try_recv() {
        match obs {
            crate::nexrad::ProjectorObservation::CollectionEndSecs(secs) => {
                let prior = iter.latest_chunk_collection_end_secs();
                iter.record_chunk_collection_end_secs(secs);
                if prior != Some(secs) {
                    log::debug!(
                        "chunk_collection_end: updated to {:.3}s (prior={:?})",
                        secs,
                        prior
                    );
                }
            }
            crate::nexrad::ProjectorObservation::AvailabilityLagSecs(lag_secs) => {
                iter.record_availability_lag_for_current(lag_secs);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn streaming_loop(
    ctx: egui::Context,
    site_id: String,
    active: Rc<Cell<bool>>,
    stats: NetworkStats,
    facade: DataFacade,
    results_tx: UnboundedSender<RealtimeResult>,
    mut observations_rx: UnboundedReceiver<crate::nexrad::ProjectorObservation>,
    mut control_rx: UnboundedReceiver<ControlMessage>,
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
    iter.set_filter(loop_state.active_filter);
    if let Some(cached) = load_cached_timing_stats(&site_id) {
        iter.preload_timing_stats(cached);
        log::debug!("Loaded cached timing stats for {}", site_id);
    }
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
    // Provisional scan start for the in-progress volume. Newtype-wrapped so
    // a bare `f64` can't be substituted from a different time axis (wall
    // clock, radial-confirmed time, etc.) by mistake. Helpers downstream
    // still take `f64` — unwrap with `.0` at those call sites.
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
        current_scan_start_secs = crate::data::ProvisionalStart(provisional_scan_start_secs(
            start_chunk.identifier.upload_date_time(),
            &iter,
        ));

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
            plan: iter.build_plan(current_timestamp_f64()),
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
                plan: iter.build_plan(current_timestamp_f64()),
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
        current_scan_start_secs = crate::data::ProvisionalStart(provisional_scan_start_secs(
            init_result.latest_chunk.identifier.upload_date_time(),
            &iter,
        ));
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
            plan: iter.build_plan(current_timestamp_f64()),
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
    // Captures `target.forecast.clone()` on a chunk's first attempt so retry
    // iterations don't overwrite it with a fresh (post-anchor-advance)
    // projection. Read at success time into the per-chunk arrival stat.
    let mut cur_forecast: Option<super::ChunkForecast> = None;
    let mut cur_predicted_wait_secs: Option<f64> = None;
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
        drain_pending_observations(&mut observations_rx, &mut iter);

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
            none_retries = 0;
            let emitted = run_mid_stream_backfill(
                &site_id,
                new_filter,
                &mut iter,
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
            iter.set_filter(active_filter);
        }

        // Build the canonical plan once per iteration. Every consumer of
        // forward-looking timing — sleep target here, UI countdown over the
        // wire, the per-chunk arrival diagnostic — reads from this object,
        // so the loop's behavior can't drift from what the UI displays.
        let now_secs = current_timestamp_f64();
        let plan = iter.build_plan(now_secs);

        // Sleep target: poll-time of the immediate next download. POLL_BIAS
        // and the bucket's retry budget are already folded into the
        // forecast's `poll_at_secs` by the projector — no additional
        // padding needed (compare to the old explicit
        // `poll_delay_after_predicted_ms`).
        let time_until_next_opt = plan
            .as_ref()
            .and_then(|p| p.next_target())
            .and_then(|t| t.forecast.as_ref())
            .map(|f| std::time::Duration::from_secs_f64((f.poll_at_secs - now_secs).max(0.0)));

        // Capture the prediction once per chunk so retry iterations don't
        // overwrite it with a near-zero "wait" after a 404.
        let is_first_iter_for_chunk = cur_predicted_at.is_none();
        if is_first_iter_for_chunk {
            if let Some(plan_ref) = plan.as_ref() {
                if let Some(forecast) = plan_ref.next_target().and_then(|t| t.forecast.as_ref()) {
                    cur_predicted_at = Some(forecast.available_at_secs);
                    cur_predicted_wait_secs = Some(forecast.poll_at_secs - now_secs);
                    cur_forecast = Some(forecast.clone());
                    cur_plan_revision = Some(plan_ref.revision);
                }
            }
        }
        if let Some(wait_duration) = time_until_next_opt {
            let wait_ms = wait_duration.as_millis() as u32;
            // POLL_BIAS and the bucket retry budget are already folded into
            // `projected_poll_at_secs` upstream, so we sleep directly to
            // that target without additional padding here.
            if wait_ms > 0 {
                match interruptible_sleep(
                    &mut loop_state,
                    &mut control_rx,
                    &ctx,
                    wait_ms,
                    active_filter_epoch,
                )
                .await
                {
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
            let plan = iter.build_plan(current_timestamp_f64());
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
                    chunks_in_volume = 0;
                    current_scan_start_secs = crate::data::ProvisionalStart(
                        provisional_scan_start_secs(chunk.identifier.upload_date_time(), &iter),
                    );
                    cache_volume_number(&site_id, *chunk.identifier.volume());
                    emitted_sequences_this_volume.clear();
                }

                chunks_in_volume += 1;
                emitted_sequences_this_volume.insert(chunk.identifier.sequence());

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
                if let (Some(upload_secs), Some(collection_end_secs)) =
                    (s3_last_modified_at, iter.latest_chunk_collection_end_secs())
                {
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
                let post_plan = iter.build_plan(current_timestamp_f64());

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
                let anchor_source = Some(iter.current_anchor_source());

                // The forecast that produced this chunk's sleep target was
                // captured into `cur_forecast` on the chunk's first
                // iteration. Map its fields into the arrival stat so the
                // diagnostics modal can compare predicted vs. observed.
                let (bucket_key, stats_n_at_prediction, scheduler_path, physics_breakdown) =
                    match cur_forecast.as_ref() {
                        Some(f) => (
                            f.bucket
                                .as_ref()
                                .map(crate::state::BucketKey::from_characteristics),
                            f.stats_n,
                            Some(f.scheduler_path),
                            Some(f.physics_breakdown),
                        ),
                        None => (None, 0, None, None),
                    };

                let arrival_stat = crate::state::ChunkArrivalStat {
                    sequence: chunks_in_volume,
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

                save_timing_stats(&site_id, iter.timing_stats());

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

/// Map a [`crate::nexrad::streaming_state::TryNextOutcome`] to a retry [`Verdict`]
/// for the filter-aware fetch path. Mirrors [`classify_chunk_result`] for
/// the unfiltered path; the only new case is `SyntheticVolumeEnd`, which is
/// not a retry — it's a terminal-for-this-iteration outcome the loop turns
/// into a synthetic `is_volume_end` signal.
fn classify_filter_outcome(
    result: nexrad_data::result::Result<crate::nexrad::streaming_state::TryNextOutcome>,
) -> Verdict<FilterFetchResult> {
    use crate::nexrad::streaming_state::TryNextOutcome;
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

fn current_timestamp() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

/// Unix seconds with millisecond precision — for diagnostics timestamps.
fn current_timestamp_f64() -> f64 {
    js_sys::Date::now() / 1000.0
}

// ── Volume number cache ────────────────────────────────────────────────

/// Cache the latest volume number in localStorage for fast resume.
fn cache_volume_number(site_id: &str, volume: nexrad_data::aws::realtime::VolumeIndex) {
    let key = format!("nexrad_volume_{}", site_id);
    if let Some(window) = web_sys::window() {
        if let Ok(Some(storage)) = window.local_storage() {
            let _ = storage.set_item(&key, &volume.as_number().to_string());
        }
    }
}

/// Read the cached volume number for a site from localStorage.
///
/// Currently unused — discovery goes through `nexrad_data::aws::realtime::get_latest_volume`,
/// which doesn't take a hint. Kept for the planned reintroduction as a fast-path hint.
#[allow(dead_code)]
fn get_cached_volume(site_id: &str) -> Option<nexrad_data::aws::realtime::VolumeIndex> {
    let key = format!("nexrad_volume_{}", site_id);
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let raw = storage.get_item(&key).ok()??;
    // Tolerate the legacy "VolumeIndex(N)" debug format that older builds wrote.
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let n = digits.parse::<usize>().ok()?;
    if (1..=999).contains(&n) {
        Some(nexrad_data::aws::realtime::VolumeIndex::new(n))
    } else {
        None
    }
}

// ── Timing stats persistence ──────────────────────────────────────────────

fn timing_stats_key(site_id: &str) -> String {
    format!("nexrad_timing_stats_{}", site_id)
}

/// Persist the site's rolling chunk-timing statistics to localStorage so the
/// next session starts warm instead of cold-starting from pure physics.
fn save_timing_stats(site_id: &str, stats: &crate::nexrad::timing::ChunkTimingStats) {
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
fn load_cached_timing_stats(site_id: &str) -> Option<crate::nexrad::timing::ChunkTimingStats> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let raw = storage.get_item(&timing_stats_key(site_id)).ok()??;
    crate::nexrad::timing::ChunkTimingStats::from_json(&raw)
}

/// Run `nexrad_data`'s upstream `get_latest_volume` then initialize a
/// [`StreamingState`] at that volume.
///
/// The cached volume number written by [`cache_volume_number`] is intentionally
/// ignored here — discovery uses the same sequential rotated-array binary
/// search the library ships with. The cache will be reintroduced as a hint in
/// a follow-up.
async fn acquire_streaming_state(
    site_id: &str,
) -> nexrad_data::result::Result<crate::nexrad::streaming_state::StreamingInit> {
    let result = nexrad_data::aws::realtime::get_latest_volume(site_id).await?;
    let volume = result.volume.ok_or(nexrad_data::result::Error::AWS(
        nexrad_data::result::aws::AWSError::LatestVolumeNotFound,
    ))?;
    StreamingState::init_at_volume(site_id, volume, result.calls).await
}
