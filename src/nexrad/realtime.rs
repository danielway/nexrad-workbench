//! Real-time NEXRAD streaming channel.
//!
//! Provides a channel-based interface for real-time NEXRAD data streaming
//! from AWS. Uses our own [`super::volume_discovery::find_latest_volume`] +
//! [`super::streaming_state::StreamingState`] instead of `ChunkIterator::start()`
//! so we can resolve the current volume with 1-2 round trips of parallel
//! probes instead of ~10 sequential binary-search LISTs.

use super::download::NetworkStats;
use super::streaming_state::StreamingState;
use super::volume_discovery::find_latest_volume;
use futures_util::future::join_all;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use eframe::egui;

use crate::data::facade::DataFacade;

/// Projected timing and structural info for a single chunk in the volume.
///
/// Combines structural metadata from `ChunkMetadata` (available for all chunks)
/// with projected timing from `ChunkProjection` (available for future chunks).
/// This decouples the UI layer from the nexrad-data library types.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ChunkProjectionInfo {
    /// 1-based sequence number in the volume.
    pub sequence: usize,
    /// Elevation number (1-based), None for the Start chunk.
    pub elevation_number: Option<usize>,
    /// Elevation angle in degrees.
    pub elevation_angle_deg: f64,
    /// Azimuth rotation rate in degrees/second from the VCP.
    pub azimuth_rate_dps: f64,
    /// COLLECTION category: projected Unix-seconds time the radar physically
    /// emits/receives for this chunk. `Some` for future chunks (from
    /// ScanTimingProjection), `None` for past chunks. Drives timeline
    /// placeholders for future sweeps.
    pub projected_collection_time_secs: Option<f64>,
    /// AVAILABILITY category: projected Unix-seconds time this chunk becomes
    /// available in S3. `Some` for future chunks (from ScanTimingProjection),
    /// `None` for past chunks.
    pub projected_available_at_secs: Option<f64>,
    /// Whether this chunk starts a new sweep.
    pub starts_new_sweep: bool,
    /// 0-based index of this chunk within its sweep.
    pub chunk_index_in_sweep: usize,
    /// Total chunks in this sweep (3 for standard, 6 for super-res).
    pub chunks_in_sweep: usize,
}

/// Result type for realtime streaming events.
///
/// `ChunkReceived` is significantly larger than the other variants because it
/// carries the per-chunk diagnostic bundle (`arrival_stat`) used by the VCP
/// forecast modal. Boxing it would add an allocation per chunk for no gain —
/// these values are produced and consumed within the same frame.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum RealtimeResult {
    /// Iterator initialized, streaming started
    Started { site_id: String },
    /// Chunk received from the stream (UI status update)
    ChunkReceived {
        chunks_in_volume: u32,
        time_until_next: Option<Duration>,
        is_volume_end: bool,
        fetch_latency_ms: f64,
        /// AVAILABILITY category: projected Unix-seconds time the final chunk
        /// of the current volume becomes available in S3, from the library's
        /// physics model.
        projected_volume_end_available_at_secs: Option<f64>,
        /// COLLECTION category: projected Unix-seconds time the radar finishes
        /// physically scanning the final chunk of the current volume. Drives
        /// the timeline's projected end-of-volume marker.
        projected_volume_end_collection_secs: Option<f64>,
        /// Per-chunk projection info for the entire volume.
        /// Structural metadata is present for all chunks; projected times only for future chunks.
        chunk_projections: Option<Vec<ChunkProjectionInfo>>,
        /// Arrival diagnostics (empty-poll counts, predicted vs. actual time).
        /// `None` on synthetic emissions such as the resume-from-cache path.
        arrival_stat: Option<crate::state::ChunkArrivalStat>,
    },
    /// Raw chunk data for incremental ingest
    ChunkData {
        data: Vec<u8>,
        chunk_index: u32,
        is_start: bool,
        is_end: bool,
        timestamp: i64,
        /// When true, the worker should skip deleting overlapping scans on
        /// is_start. Set when resuming a volume that already has cached data
        /// in IDB, to avoid destroying previously-stored sweep blobs.
        skip_overlap_delete: bool,
    },
    /// Error occurred during streaming
    Error(String),
}

/// Internal state for the realtime streaming channel.
#[derive(Default)]
struct RealtimeState {
    results: Vec<RealtimeResult>,
    active: bool,
    time_until_next: Option<Duration>,
    stop_requested: bool,
    /// ACTUAL category: latest radial collection time (Unix seconds) of
    /// the most recently ingested chunk. Pushed in from `main.rs` after
    /// each ingest response and drained by the streaming loop into
    /// `StreamingState` so projections anchor on the current chunk's true
    /// collection time rather than the volume's start.
    pending_chunk_collection_end_secs: Option<f64>,
    /// Empirical availability lag (seconds) for the most recently ingested
    /// chunk, computed in `main.rs` as `s3_last_modified - chunk_max_time`.
    /// Drained by the streaming loop and attached to the latest
    /// `ChunkTimingStats` sample.
    pending_availability_lag_secs: Option<f64>,
}

/// Channel for real-time NEXRAD streaming.
pub struct RealtimeChannel {
    state: Rc<RefCell<RealtimeState>>,
    stats: NetworkStats,
}

impl Default for RealtimeChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl RealtimeChannel {
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(RealtimeState::default())),
            stats: NetworkStats::new(),
        }
    }

    pub fn with_stats(stats: NetworkStats) -> Self {
        Self {
            state: Rc::new(RefCell::new(RealtimeState::default())),
            stats,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state.borrow().active
    }

    pub fn time_until_next(&self) -> Option<Duration> {
        self.state.borrow().time_until_next
    }

    pub fn start(&self, ctx: egui::Context, site_id: String, facade: DataFacade) {
        {
            let mut state = self.state.borrow_mut();
            state.active = true;
            state.stop_requested = false;
            state.results.clear();
            state.time_until_next = None;
        }

        let state = self.state.clone();
        let stats = self.stats.clone();

        wasm_bindgen_futures::spawn_local(async move {
            streaming_loop(ctx, site_id, state, stats, facade).await;
        });
    }

    pub fn stop(&self) {
        let mut state = self.state.borrow_mut();
        state.stop_requested = true;
        state.active = false;
    }

    pub fn try_recv(&self) -> Option<RealtimeResult> {
        let mut state = self.state.borrow_mut();
        if state.results.is_empty() {
            None
        } else {
            Some(state.results.remove(0))
        }
    }

    /// Push the latest radial collection time (Unix seconds) parsed from
    /// the chunk that was just ingested. The streaming loop drains this
    /// and stamps it onto `StreamingState` so the next projection anchors
    /// on the current chunk's actual collection time.
    pub fn record_chunk_collection_end_secs(&self, secs: f64) {
        self.state.borrow_mut().pending_chunk_collection_end_secs = Some(secs);
    }

    /// Push an empirical availability lag (S3 upload − ACTUAL chunk
    /// collection time, seconds) for the chunk just ingested. The streaming
    /// loop attaches it to the most recent `ChunkTimingStats` sample so
    /// future projections can use a median lag rather than a hard default.
    pub fn record_availability_lag_secs(&self, lag_secs: f64) {
        self.state.borrow_mut().pending_availability_lag_secs = Some(lag_secs);
    }
}

/// Build projection info from the streaming state's current position.
///
/// Combines structural metadata (all chunks) with projected timing (future chunks only).
fn build_chunk_projections(state: &StreamingState) -> Option<Vec<ChunkProjectionInfo>> {
    let all_meta = state.all_chunk_metadata()?;
    let projection = state.project_remaining_scan();

    // Build lookups from sequence → {collection, availability} times.
    let (projected_collection_by_seq, projected_available_at_by_seq): (
        std::collections::HashMap<usize, f64>,
        std::collections::HashMap<usize, f64>,
    ) = projection
        .as_ref()
        .map(|p| {
            let mut collection = std::collections::HashMap::new();
            let mut available = std::collections::HashMap::new();
            for c in p.chunks() {
                collection.insert(c.sequence(), c.projected_collection_time_secs());
                available.insert(c.sequence(), c.projected_available_at().timestamp() as f64);
            }
            (collection, available)
        })
        .unwrap_or_default();

    Some(
        all_meta
            .iter()
            .map(|meta| ChunkProjectionInfo {
                sequence: meta.sequence(),
                elevation_number: meta.elevation_number(),
                elevation_angle_deg: meta.elevation_angle_deg(),
                azimuth_rate_dps: meta.azimuth_rate_dps(),
                projected_collection_time_secs: projected_collection_by_seq
                    .get(&meta.sequence())
                    .copied(),
                projected_available_at_secs: projected_available_at_by_seq
                    .get(&meta.sequence())
                    .copied(),
                starts_new_sweep: meta.is_first_in_sweep(),
                chunk_index_in_sweep: meta.chunk_index_in_sweep(),
                chunks_in_sweep: meta.chunks_in_sweep(),
            })
            .collect(),
    )
}

/// Get the projected S3-availability time of the current volume's final
/// chunk (Unix seconds).
fn get_projected_volume_end_available_at_secs(state: &StreamingState) -> Option<f64> {
    state
        .projected_volume_end_available_at()
        .map(|dt| dt.timestamp() as f64)
}

/// Default provisional lag applied when we have no observed median lag
/// yet. Matches the default in the projector so a cold stream's first
/// ScanKey lands near the eventual real value.
const DEFAULT_PROVISIONAL_LAG_SECS: f64 = 5.0;

/// Provisional scan-start timestamp (Unix seconds) for a new volume.
///
/// Uses the Start chunk's S3 upload time minus the current median
/// availability lag from `ChunkTimingStats` (falling back to
/// `DEFAULT_PROVISIONAL_LAG_SECS`). That lands close to the real volume
/// header collection time — closer than the wall-clock receipt time it
/// replaces — without needing to wait for the first M chunk's radial
/// parse. If there is no upload time, fall back to wall clock.
fn provisional_scan_start_secs(
    start_upload: Option<chrono::DateTime<chrono::Utc>>,
    iter: &StreamingState,
) -> i64 {
    let median_lag_secs = iter
        .timing_stats()
        .median_availability_lag_secs()
        .unwrap_or(DEFAULT_PROVISIONAL_LAG_SECS);
    if let Some(upload) = start_upload {
        let upload_secs = upload.timestamp_millis() as f64 / 1000.0;
        return (upload_secs - median_lag_secs).round() as i64;
    }
    current_timestamp()
}

/// Drain anything pushed in from `main.rs` after a worker ingest and stamp
/// it onto the `StreamingState`: the ACTUAL volume header time (anchors
/// collection-time projections) and the empirical per-chunk availability
/// lag (feeds `ChunkTimingStats`).
fn drain_pending_ingest_observations(
    state_cell: &Rc<RefCell<RealtimeState>>,
    iter: &mut StreamingState,
) {
    let (pending_collection_end, pending_lag) = {
        let mut s = state_cell.borrow_mut();
        (
            s.pending_chunk_collection_end_secs.take(),
            s.pending_availability_lag_secs.take(),
        )
    };
    if let Some(secs) = pending_collection_end {
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
    if let Some(lag_secs) = pending_lag {
        iter.record_availability_lag_for_current(lag_secs);
    }
}

async fn streaming_loop(
    ctx: egui::Context,
    site_id: String,
    state: Rc<RefCell<RealtimeState>>,
    stats: NetworkStats,
    _facade: DataFacade,
) {
    use nexrad_data::aws::realtime::{download_chunk, list_chunks_in_volume, ChunkType};

    log::debug!("Starting realtime streaming for site: {}", site_id);

    // Initialize with a timeout to avoid indefinite waiting when the site has
    // no data or is unreachable. Each .await is a cancellation point — when
    // the timeout wins the select, the init future is dropped, which drops any
    // in-flight HTTP request futures and cancels them.
    const ACQUIRE_TIMEOUT_SECS: u32 = 10;
    const CHUNK_POLL_INTERVAL_MS: u32 = 500;
    const CHUNK_POLL_MAX_RETRIES: u32 = 25; // 25 × 500ms = 12.5s
    const CHUNK_POLL_GRACE_MS: u32 = 2500; // 2.5s final grace → 15s total

    // Pad added to the first poll wait per chunk; collapses the "fire a hair
    // early → empty poll → sleep 500ms → fire" path on chunks whose
    // predictions are tight. Retry waits are unaffected.
    //
    // Sized to cover the ~300 ms of WASM/setTimeout slop plus a small margin
    // for prediction bias. Previously bumped to 600 ms as a blunt hedge for
    // sweep-transition mispredictions; the proper fix for those lives in the
    // timing model + historical-stats lookup, so this can stay modest and
    // avoid adding latency to the ~90% of chunks that are intra-sweep and
    // predicted accurately.
    const POLL_DELAY_AFTER_PREDICTED_MS: u32 = 400;

    let hint = get_cached_volume(&site_id);
    let init_future = acquire_streaming_state(&site_id, hint);
    let timeout_future = sleep_ms(ACQUIRE_TIMEOUT_SECS * 1000);

    futures_util::pin_mut!(init_future);
    futures_util::pin_mut!(timeout_future);

    let init_result = match futures_util::future::select(init_future, timeout_future).await {
        futures_util::future::Either::Left((Ok(init), _)) => init,
        futures_util::future::Either::Left((Err(e), _)) => {
            let mut s = state.borrow_mut();
            s.results.push(RealtimeResult::Error(format!(
                "Failed to initialize: {}",
                e
            )));
            s.active = false;
            ctx.request_repaint();
            return;
        }
        futures_util::future::Either::Right(_) => {
            log::warn!(
                "Realtime acquisition timed out after {}s for site {}",
                ACQUIRE_TIMEOUT_SECS,
                site_id
            );
            let mut s = state.borrow_mut();
            s.results.push(RealtimeResult::Error(format!(
                "Acquisition timed out after {}s — data may be unavailable for this site",
                ACQUIRE_TIMEOUT_SECS
            )));
            s.active = false;
            ctx.request_repaint();
            return;
        }
    };

    let mut iter = init_result.state;
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
    {
        let mut s = state.borrow_mut();
        s.results.push(RealtimeResult::Started {
            site_id: site_id.clone(),
        });
    }
    ctx.request_repaint();

    let mut chunks_in_volume: u32;
    let mut current_scan_start_secs: i64;

    // --- Process init chunks (backfill from mid-volume join) ---
    // If start_chunk is Some, we joined mid-volume: emit start chunk + latest chunk.
    // If start_chunk is None, latest_chunk IS the start chunk.
    if let Some(start_chunk) = init_result.start_chunk {
        // Joined mid-volume: emit the start chunk + current sweep's chunks only.
        let start_data = start_chunk.chunk.data().to_vec();
        current_scan_start_secs =
            provisional_scan_start_secs(start_chunk.identifier.upload_date_time(), &iter);

        log::debug!(
            "Init: emitting start_chunk ({} bytes) for mid-volume join",
            start_data.len()
        );
        {
            let mut s = state.borrow_mut();
            s.results.push(RealtimeResult::ChunkData {
                data: start_data,
                chunk_index: 0,
                is_start: true,
                is_end: false,
                timestamp: current_scan_start_secs,
                // Skip overlap deletion — we're only backfilling the current
                // sweep, not replacing the full volume.
                skip_overlap_delete: true,
            });
        }
        ctx.request_repaint();

        // Download only the current sweep's preceding chunks (not the full volume).
        // Use chunk metadata to find which sequences share the latest chunk's
        // elevation, then download only those that precede it.
        let latest_seq = init_result.latest_chunk.identifier.sequence();
        let volume = *init_result.latest_chunk.identifier.volume();
        cache_volume_number(&site_id, volume);
        chunks_in_volume = 1; // start chunk already emitted

        let latest_elev = iter
            .chunk_metadata(latest_seq)
            .and_then(|m| m.elevation_number());

        // Collect sequences for the same sweep that precede the latest chunk.
        let sweep_seqs: Vec<usize> = if let Some(elev) = latest_elev {
            iter.all_chunk_metadata()
                .map(|metas| {
                    metas
                        .iter()
                        .filter(|m| {
                            m.elevation_number() == Some(elev)
                                && m.sequence() > 1
                                && m.sequence() < latest_seq
                        })
                        .map(|m| m.sequence())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        if !sweep_seqs.is_empty() {
            match list_chunks_in_volume(&site_id, volume, 100).await {
                Ok(chunk_ids) => {
                    let to_download: Vec<_> = chunk_ids
                        .into_iter()
                        .filter(|id| sweep_seqs.contains(&id.sequence()))
                        .collect();

                    log::debug!(
                        "Sweep backfill: downloading {} chunks for current sweep (elev {:?}, seq {:?})",
                        to_download.len(),
                        latest_elev,
                        sweep_seqs,
                    );

                    // Download all missing sweep chunks in parallel. The list
                    // is small (typically 2–6), so issuing them concurrently
                    // cuts wall-clock latency substantially compared to
                    // staircasing sequential requests. We collect into a Vec
                    // first to preserve deterministic order when emitting.
                    let mut downloaded: Vec<(u32, Vec<u8>)> = if state.borrow().stop_requested {
                        Vec::new()
                    } else {
                        let results =
                            join_all(to_download.iter().map(|id| download_chunk(&site_id, id)))
                                .await;
                        let mut out = Vec::with_capacity(results.len());
                        for (chunk_id, res) in to_download.iter().zip(results) {
                            match res {
                                Ok((_id, chunk)) => {
                                    let chunk_data = chunk.data().to_vec();
                                    log::debug!(
                                        "Sweep backfill: downloaded chunk seq {} ({} bytes)",
                                        chunk_id.sequence(),
                                        chunk_data.len(),
                                    );
                                    out.push((chunk_id.sequence() as u32, chunk_data));
                                }
                                Err(e) => {
                                    log::warn!(
                                        "Sweep backfill: failed to download chunk seq {}: {}",
                                        chunk_id.sequence(),
                                        e
                                    );
                                }
                            }
                        }
                        // If stop was requested while we were fetching, discard
                        // the results so we don't emit chunks after shutdown.
                        if state.borrow().stop_requested {
                            Vec::new()
                        } else {
                            out
                        }
                    };
                    // Emit in sequence order so chunk_index stays monotonic.
                    downloaded.sort_by_key(|(seq, _)| *seq);

                    for (_seq, chunk_data) in downloaded {
                        chunks_in_volume += 1;
                        {
                            let mut s = state.borrow_mut();
                            s.results.push(RealtimeResult::ChunkData {
                                data: chunk_data,
                                chunk_index: chunks_in_volume - 1,
                                is_start: false,
                                is_end: false,
                                timestamp: current_scan_start_secs,
                                skip_overlap_delete: false,
                            });
                        }
                        ctx.request_repaint();
                    }

                    log::debug!(
                        "Sweep backfill: completed, {} chunks downloaded for elev {:?}",
                        chunks_in_volume - 1,
                        latest_elev,
                    );
                }
                Err(e) => {
                    log::warn!("Sweep backfill: failed to list chunks: {}, skipping", e);
                }
            }
        } else {
            log::debug!(
                "Sweep backfill: no preceding chunks for latest seq {} (elev {:?})",
                latest_seq,
                latest_elev,
            );
        }

        // Emit the latest chunk (where the iterator is positioned)
        let latest_data = init_result.latest_chunk.chunk.data().to_vec();
        let latest_type = init_result.latest_chunk.identifier.chunk_type();
        let latest_is_end = latest_type == ChunkType::End;
        chunks_in_volume += 1;

        log::debug!(
            "Init: emitting latest_chunk seq {} ({} bytes, is_end={})",
            latest_seq,
            latest_data.len(),
            latest_is_end
        );
        {
            let mut s = state.borrow_mut();
            s.results.push(RealtimeResult::ChunkData {
                data: latest_data,
                chunk_index: chunks_in_volume - 1,
                is_start: false,
                is_end: latest_is_end,
                timestamp: current_scan_start_secs,
                skip_overlap_delete: false,
            });
            s.results.push(RealtimeResult::ChunkReceived {
                chunks_in_volume,
                time_until_next: iter.time_until_next().and_then(|td| td.to_std().ok()),
                is_volume_end: latest_is_end,
                fetch_latency_ms: 0.0,
                projected_volume_end_available_at_secs: get_projected_volume_end_available_at_secs(
                    &iter,
                ),
                projected_volume_end_collection_secs: iter.projected_volume_end_collection_secs(),
                chunk_projections: build_chunk_projections(&iter),
                arrival_stat: None,
            });
        }
        ctx.request_repaint();
    } else {
        // Joined at volume start: latest_chunk IS the start chunk
        let latest_data = init_result.latest_chunk.chunk.data().to_vec();
        let latest_type = init_result.latest_chunk.identifier.chunk_type();
        let latest_is_start = latest_type == ChunkType::Start;
        let latest_is_end = latest_type == ChunkType::End;
        current_scan_start_secs = provisional_scan_start_secs(
            init_result.latest_chunk.identifier.upload_date_time(),
            &iter,
        );
        chunks_in_volume = 1;
        cache_volume_number(&site_id, *init_result.latest_chunk.identifier.volume());

        log::debug!(
            "Init: emitting latest_chunk as start ({} bytes)",
            latest_data.len()
        );
        {
            let mut s = state.borrow_mut();
            s.results.push(RealtimeResult::ChunkData {
                data: latest_data,
                chunk_index: 0,
                is_start: latest_is_start,
                is_end: latest_is_end,
                timestamp: current_scan_start_secs,
                skip_overlap_delete: false,
            });
            s.results.push(RealtimeResult::ChunkReceived {
                chunks_in_volume,
                time_until_next: iter.time_until_next().and_then(|td| td.to_std().ok()),
                is_volume_end: latest_is_end,
                fetch_latency_ms: 0.0,
                projected_volume_end_available_at_secs: get_projected_volume_end_available_at_secs(
                    &iter,
                ),
                projected_volume_end_collection_secs: iter.projected_volume_end_collection_secs(),
                chunk_projections: build_chunk_projections(&iter),
                arrival_stat: None,
            });
        }
        ctx.request_repaint();
    }

    // --- Main streaming loop: emit ChunkData per chunk ---
    // Per-chunk arrival tracking: captured on the first iteration for each
    // chunk and reset on success (or on final-retry recovery).
    let mut none_retries: u32 = 0;
    let mut cur_predicted_at: Option<f64> = None; // absolute Unix seconds
    let mut cur_last_empty_at: Option<f64> = None;
    let mut cur_diagnostics: Option<super::timing::EstimatedChunkProcessing> = None;
    loop {
        // Check stop signal
        if state.borrow().stop_requested {
            log::debug!("Realtime streaming stopped");
            break;
        }

        // Ingest any volume header time + availability lag the worker
        // produced from the most recent chunk's radials so projections and
        // stats in this iteration see them.
        drain_pending_ingest_observations(&state, &mut iter);

        // Wait for expected chunk time. Only capture the prediction on the
        // first iteration for a given chunk — subsequent retry iterations
        // re-enter here with a near-zero wait, which would overwrite the
        // original estimate.
        let is_first_iter_for_chunk = cur_predicted_at.is_none();
        let time_until_next_opt = iter.time_until_next().and_then(|d| d.to_std().ok());
        if is_first_iter_for_chunk {
            if let Some(d) = time_until_next_opt {
                cur_predicted_at = Some(current_timestamp_f64() + d.as_secs_f64());
            }
            // Capture the rich estimator diagnostics (which path, sample
            // count, physics breakdown) at the first poll for this chunk
            // so we can attribute prediction error component-by-component.
            cur_diagnostics = iter.next_chunk_processing_diagnostics();
        }
        if let Some(wait_duration) = time_until_next_opt {
            let mut wait_ms = wait_duration.as_millis() as u32;
            // Pad the first (prediction-driven) wait per chunk so we fire
            // slightly after `predicted_available_at`. Retry waits after an
            // empty poll come through `sleep_ms(CHUNK_POLL_INTERVAL_MS)` in
            // the `Ok(None)` arm, not here, so the pad applies exactly once
            // per chunk.
            if is_first_iter_for_chunk && wait_ms > 0 {
                wait_ms = wait_ms.saturating_add(POLL_DELAY_AFTER_PREDICTED_MS);
            }
            if wait_ms > 0 && !interruptible_sleep(&state, &ctx, wait_ms).await {
                log::debug!("Realtime streaming stopped");
                break;
            }
        }

        // Fetch next chunk (with timing)
        let chunk_fetch_start = web_time::Instant::now();
        match iter.try_next().await {
            Ok(Some(chunk)) => {
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
                    current_scan_start_secs =
                        provisional_scan_start_secs(chunk.identifier.upload_date_time(), &iter);
                    cache_volume_number(&site_id, *chunk.identifier.volume());
                }

                chunks_in_volume += 1;

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

                // Look up this chunk's entry in the library's projection list
                // so we can attach elevation_number and chunk-within-sweep
                // position to the arrival stat.
                let chunk_projections = build_chunk_projections(&iter);
                let (elevation_number, chunk_index_in_sweep, chunks_in_sweep) =
                    match chunk_projections.as_ref().and_then(|projs| {
                        projs.iter().find(|p| p.sequence as u32 == chunks_in_volume)
                    }) {
                        Some(p) => (
                            p.elevation_number.map(|e| e as u8),
                            Some(p.chunk_index_in_sweep as u32),
                            Some(p.chunks_in_sweep as u32),
                        ),
                        None => (None, None, None),
                    };

                // Anchor source the projector was using for the *previous*
                // chunk — i.e. the one whose arrival we're recording.
                // Captured AFTER `try_next` advances `iter.current` is fine
                // because `current_anchor_source` reads collection-end +
                // median-lag state, both of which are independent of which
                // chunk is "current".
                let anchor_source = Some(iter.current_anchor_source());

                let (bucket_key, stats_n_at_prediction, scheduler_path, physics_breakdown) =
                    match cur_diagnostics.as_ref() {
                        Some(d) => (
                            d.bucket
                                .as_ref()
                                .map(crate::state::BucketKey::from_characteristics),
                            d.stats_n_at_prediction,
                            Some(d.path),
                            d.physics_breakdown,
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
                };

                // Reset tracking state for the next chunk
                none_retries = 0;
                cur_predicted_at = None;
                cur_last_empty_at = None;
                cur_diagnostics = None;

                let time_until_next = iter.time_until_next().and_then(|td| td.to_std().ok());

                {
                    let mut s = state.borrow_mut();
                    // Emit the raw chunk for incremental ingest
                    s.results.push(RealtimeResult::ChunkData {
                        data: chunk_data,
                        chunk_index: chunks_in_volume - 1,
                        is_start,
                        is_end,
                        timestamp: current_scan_start_secs,
                        skip_overlap_delete: false,
                    });
                    // Emit UI status update
                    s.results.push(RealtimeResult::ChunkReceived {
                        chunks_in_volume,
                        time_until_next,
                        is_volume_end: is_end,
                        fetch_latency_ms: chunk_fetch_ms,
                        projected_volume_end_available_at_secs:
                            get_projected_volume_end_available_at_secs(&iter),
                        projected_volume_end_collection_secs: iter
                            .projected_volume_end_collection_secs(),
                        chunk_projections,
                        arrival_stat: Some(arrival_stat),
                    });
                }

                save_timing_stats(&site_id, iter.timing_stats());

                ctx.request_repaint();
            }
            Ok(None) => {
                // Chunk not ready yet, brief retry
                none_retries += 1;
                cur_last_empty_at = Some(current_timestamp_f64());
                if none_retries >= CHUNK_POLL_MAX_RETRIES {
                    let elapsed_secs =
                        (none_retries * CHUNK_POLL_INTERVAL_MS + CHUNK_POLL_GRACE_MS) / 1000;
                    log::warn!(
                        "Streaming: {} consecutive empty polls, attempting final fetch after {}ms delay",
                        none_retries,
                        CHUNK_POLL_GRACE_MS,
                    );
                    sleep_ms(CHUNK_POLL_GRACE_MS).await;
                    if state.borrow().stop_requested {
                        break;
                    }
                    match iter.try_next().await {
                        Ok(Some(_chunk)) => {
                            // Recovered — let the next loop iteration handle it normally.
                            // Reset the per-chunk tracking so the next successful fetch
                            // emits a fresh ChunkArrivalStat (the discarded chunk here
                            // is an existing quirk of the recovery path).
                            none_retries = 0;
                            cur_predicted_at = None;
                            cur_last_empty_at = None;
                            cur_diagnostics = None;
                            continue;
                        }
                        Ok(None) => {
                            log::error!(
                                "Streaming: final retry still empty after ~{}s, giving up",
                                elapsed_secs
                            );
                            let mut s = state.borrow_mut();
                            s.results.push(RealtimeResult::Error(format!(
                                "Chunk polling timed out — no data received for ~{} seconds",
                                elapsed_secs
                            )));
                            s.active = false;
                            ctx.request_repaint();
                            break;
                        }
                        Err(e) => {
                            log::error!("Streaming error on final retry: {}", e);
                            let mut s = state.borrow_mut();
                            s.results.push(RealtimeResult::Error(format!("{}", e)));
                            s.active = false;
                            ctx.request_repaint();
                            break;
                        }
                    }
                }
                sleep_ms(CHUNK_POLL_INTERVAL_MS).await;
            }
            Err(e) => {
                log::error!("Streaming error: {}", e);
                let mut s = state.borrow_mut();
                s.results.push(RealtimeResult::Error(format!("{}", e)));
                s.active = false;
                ctx.request_repaint();
                break;
            }
        }
    }

    state.borrow_mut().active = false;
}

/// Sleep in increments, updating countdown UI and checking stop flag.
/// Returns false if stop requested.
async fn interruptible_sleep(
    state: &Rc<RefCell<RealtimeState>>,
    ctx: &egui::Context,
    total_ms: u32,
) -> bool {
    const INCREMENT: u32 = 250;
    let mut remaining = total_ms;

    while remaining > 0 {
        if state.borrow().stop_requested {
            return false;
        }

        // Update countdown in UI
        state.borrow_mut().time_until_next =
            Some(std::time::Duration::from_millis(remaining as u64));
        ctx.request_repaint();

        let sleep_time = INCREMENT.min(remaining);
        sleep_ms(sleep_time).await;
        remaining = remaining.saturating_sub(INCREMENT);
    }

    // Clear countdown when done waiting
    state.borrow_mut().time_until_next = None;
    !state.borrow().stop_requested
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

async fn sleep_ms(ms: u32) {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        fn set_timeout(closure: &Closure<dyn FnMut()>, millis: u32) -> i32;
    }

    let (tx, rx) = futures_channel::oneshot::channel::<()>();
    let closure = Closure::once(move || {
        let _ = tx.send(());
    });
    set_timeout(&closure, ms);
    let _ = rx.await;
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
fn save_timing_stats(site_id: &str, stats: &super::timing::ChunkTimingStats) {
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
fn load_cached_timing_stats(site_id: &str) -> Option<super::timing::ChunkTimingStats> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let raw = storage.get_item(&timing_stats_key(site_id)).ok()??;
    super::timing::ChunkTimingStats::from_json(&raw)
}

/// Run [`find_latest_volume`] then initialize a [`StreamingState`] at that volume.
///
/// The returned [`super::streaming_state::StreamingInit`] has the same shape as
/// `ChunkIteratorInit` so the rest of the streaming loop is unchanged.
async fn acquire_streaming_state(
    site_id: &str,
    hint: Option<nexrad_data::aws::realtime::VolumeIndex>,
) -> nexrad_data::result::Result<super::streaming_state::StreamingInit> {
    let search = find_latest_volume(site_id, hint).await?;
    let volume = search.volume.ok_or(nexrad_data::result::Error::AWS(
        nexrad_data::result::aws::AWSError::LatestVolumeNotFound,
    ))?;
    StreamingState::init_at_volume(site_id, volume, search.requests_made).await
}
