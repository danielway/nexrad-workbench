//! Streaming-loop implementation and private helpers for `RealtimeChannel`.
//!
//! This module contains the long-running [`streaming_loop`] async task that
//! polls AWS for new chunks, applies the projector, and dispatches results
//! back through the channel. Its helpers are split by concern into
//! submodules — the loop itself is the only thing that sequences them, and
//! its ordering of `.await` points is load-bearing for live timing:
//!
//! - [`loop_state`] — loop-local control state, the interruptible sleep, and
//!   the network-stats delta tracker.
//! - [`acquire`] — volume discovery and stream initialization under a timeout.
//! - [`init`] — emission of the chunks acquired at init (mid-volume join vs.
//!   join-at-volume-start).
//! - [`backfill`] — filter-aware backfill of already-published chunks.
//! - [`poll`] — the adaptive cross-volume wait and the chunk-fetch retry loop.
//! - [`engine`] — every interaction with the shared projection engine.
//! - [`arrival`] — per-chunk arrival diagnostics derivations.
//! - [`persist`] — localStorage volume hint + timing stats.

mod acquire;
mod arrival;
mod backfill;
mod engine;
mod init;
mod loop_state;
mod persist;
mod poll;

use super::{ControlMessage, ProjectorObservation, RealtimeResult};
use crate::core::projection::SharedProjectionEngine;
use crate::core::StreamingFilter;
use crate::data::facade::MainThreadStore;
use crate::nexrad::acquisition::download::NetworkStats;
use eframe::egui;
use engine::{build_engine_plan, drain_pending_observations};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
use loop_state::{drain_control, interruptible_sleep, LoopState, SleepOutcome, StatsTracker};
use persist::{cache_volume_number, load_cached_timing_stats, save_timing_stats};
use std::cell::Cell;
use std::rc::Rc;

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
    use nexrad_data::aws::realtime::ChunkType;

    log::debug!("Starting realtime streaming for site: {}", site_id);

    // Initialize with a timeout to avoid indefinite waiting when the site has
    // no data or is unreachable.
    let Some(init_result) =
        acquire::acquire_with_timeout(&site_id, &active, &results_tx, &ctx).await
    else {
        return;
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
    // Start chunk's volume header (see `init::volume_header_start_secs`). This
    // is the scan's identity — the value that becomes the IDB key, the cache
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
        let Some(header_secs) = init::emit_mid_volume_start_chunk(
            &site_id,
            &start_chunk,
            &iter,
            &engine,
            &results_tx,
            &active,
            &ctx,
        ) else {
            return;
        };
        current_scan_start_secs = crate::data::ProvisionalStart(header_secs);

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

        let (emitted, cached_elevs) = backfill::run_init_backfill(
            &site_id,
            backfill_filter,
            latest_seq,
            latest_elev,
            volume,
            &mut iter,
            &engine,
            &facade,
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

        // Emit the latest chunk only when the filter accepts it (or when
        // it's the volume's End chunk so the rollover signal still lands).
        chunks_in_volume += init::emit_init_latest_chunk(
            &init_result.latest_chunk,
            initial_filter,
            latest_elev,
            &cached_elevs,
            &iter,
            &engine,
            &results_tx,
            &ctx,
            chunks_in_volume,
            current_scan_start_secs.0,
            &mut emitted_sequences_this_volume,
        );
    } else {
        // Joined at volume start: latest_chunk IS the start chunk
        let Some(header_secs) = init::emit_join_at_volume_start(
            &site_id,
            &init_result.latest_chunk,
            &iter,
            &engine,
            &results_tx,
            &active,
            &ctx,
            &mut emitted_sequences_this_volume,
        ) else {
            return;
        };
        current_scan_start_secs = crate::data::ProvisionalStart(header_secs);
        chunks_in_volume = 1;
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
            let emitted = backfill::run_mid_stream_backfill(
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
            let threshold_ms = (poll::LIST_PROBE_THRESHOLD_SECS * 1000.0) as u32;
            let outcome = if let (true, Some(target_seq)) = (
                poll::should_list_now(wait_ms, next_in_next_volume, threshold_ms),
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
                let (out, resolution) = poll::wait_for_next_target(
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

        let chunk_fetch_start = web_time::Instant::now();
        let (fetch_outcome, synthetic_volume_end) = poll::fetch_next_chunk(
            &mut iter,
            active_filter,
            &mut loop_state,
            &mut control_rx,
            chunk_fetch_start,
            &mut none_retries,
            &mut cur_last_empty_at,
        )
        .await;

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
                    let Some(header_secs) = init::volume_header_start_secs(&chunk_data) else {
                        init::abort_missing_volume_header(&site_id, &results_tx, &active, &ctx);
                        return;
                    };
                    chunks_in_volume = 0;
                    current_scan_start_secs = crate::data::ProvisionalStart(header_secs);
                    cache_volume_number(&site_id, *chunk.identifier.volume());
                    emitted_sequences_this_volume.clear();

                    engine::install_volume_boundary(
                        &mut iter,
                        &engine,
                        &chunk,
                        current_scan_start_secs.0,
                    );
                }

                chunks_in_volume += 1;
                emitted_sequences_this_volume.insert(chunk.identifier.sequence());

                // Feed the shared engine: this chunk is now known-available, and
                // its arrival interval feeds the timing-stats blend.
                engine::observe_chunk_arrival(
                    &engine,
                    &chunk.identifier,
                    chunk_type,
                    &mut prev_upload_dt,
                );

                let type_label = arrival::chunk_type_label(is_start, is_end);
                let s3_last_modified_at = chunk
                    .identifier
                    .upload_date_time()
                    .map(|dt| dt.timestamp_millis() as f64 / 1000.0);

                arrival::log_ingest_lag(&engine, s3_last_modified_at, chunks_in_volume, type_label);

                // Build the fresh plan *after* try_next advances `iter.current`,
                // so it describes the NEXT download from this point. Same
                // object feeds both the UI and the next loop iteration's
                // sleep target — keeping them in lock-step.
                let post_plan =
                    build_engine_plan(&engine, iter.current_id(), current_timestamp_f64());

                // Attach structural metadata for the chunk that just arrived
                // by looking it up in the fresh plan's current-volume slice.
                let (elevation_number, chunk_index_in_sweep, chunks_in_sweep) =
                    arrival::chunk_structure_from_plan(post_plan.as_ref(), chunks_in_volume);

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
                    arrival::forecast_stat_fields(cur_forecast.as_ref());

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

/// Unix seconds with millisecond precision — for diagnostics timestamps.
///
/// Shared by every submodule; it lives at the module root rather than in a
/// leaf so no submodule has to depend on a sibling just to read the clock.
fn current_timestamp_f64() -> f64 {
    js_sys::Date::now() / 1000.0
}
