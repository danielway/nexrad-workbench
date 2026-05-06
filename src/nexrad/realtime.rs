//! Real-time NEXRAD streaming channel.
//!
//! Provides a channel-based interface for real-time NEXRAD data streaming
//! from AWS. Resolves the current volume via `nexrad_data`'s upstream
//! `get_latest_volume` and then drives the stream through our own
//! [`super::streaming_state::StreamingState`] (a slimmed replacement for the
//! library's `ChunkIterator`).

use super::download::NetworkStats;
use super::streaming_state::StreamingState;
use futures_util::future::join_all;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use eframe::egui;

use crate::data::facade::DataFacade;
use crate::net::retry::{
    attempt_with_timeout, compute_delay, sleep_duration, Verdict, REALTIME_CHUNK_POLICY,
};

/// User-driven filter applied to the real-time chunk stream.
///
/// `All` is the default and downloads every chunk in the volume — required for
/// `ElevationSelection::Latest` because the renderer chooses whichever
/// elevation completed most recently. `Elevation(n)` restricts the loop to
/// the Start chunk plus chunks belonging to elevation `n`; the loop uses the
/// VCP's `ElevationChunkMapper` and the physics-based timing model to wait
/// through chunks that don't match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StreamingFilter {
    #[default]
    All,
    Elevation(u8),
}

impl StreamingFilter {
    /// Whether the filter accepts a chunk for the given elevation number.
    /// `None` (Start chunk) is always accepted.
    pub fn accepts(self, elevation_number: Option<usize>) -> bool {
        match (self, elevation_number) {
            (StreamingFilter::All, _) => true,
            (StreamingFilter::Elevation(_), None) => true,
            (StreamingFilter::Elevation(target), Some(elev)) => elev as u8 == target,
        }
    }
}

impl From<&crate::state::ElevationSelection> for StreamingFilter {
    fn from(selection: &crate::state::ElevationSelection) -> Self {
        match selection {
            crate::state::ElevationSelection::Latest => StreamingFilter::All,
            crate::state::ElevationSelection::Fixed {
                elevation_number, ..
            } => StreamingFilter::Elevation(*elevation_number),
        }
    }
}

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
        /// Whether this chunk is the last chunk of its sweep, derived from
        /// the VCP mapper at emission time. The worker accumulator uses this
        /// to flush the in-progress elevation as soon as the last chunk is
        /// ingested rather than waiting for the next elevation's first chunk
        /// — important under filter mode where the next-elevation chunk may
        /// never arrive in this volume. `None` means the projection didn't
        /// resolve (rare; e.g. for the Start chunk).
        is_last_in_sweep: Option<bool>,
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
    /// Active filter on the chunk stream. Updated from the UI thread via
    /// `RealtimeChannel::set_filter`; the streaming loop snapshots this on
    /// each iteration and uses it to skip chunks that don't match.
    pending_filter: StreamingFilter,
    /// Bumped by `set_filter` on every change so a sleeping loop can detect
    /// "the filter just changed" via epoch comparison and wake up to
    /// re-target without polling the filter value itself for equality.
    filter_epoch: u64,
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

    /// Update the active streaming filter. Bumps the filter epoch so a
    /// sleeping `streaming_loop` wakes within ~250 ms and re-targets.
    /// Setting the same value the loop already has is a no-op.
    pub fn set_filter(&self, filter: StreamingFilter) {
        let mut state = self.state.borrow_mut();
        if state.pending_filter == filter {
            return;
        }
        state.pending_filter = filter;
        state.filter_epoch = state.filter_epoch.wrapping_add(1);
    }
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
    iter: &StreamingState,
    state: &Rc<RefCell<RealtimeState>>,
    ctx: &egui::Context,
    chunks_in_volume_start: u32,
    timestamp: i64,
    emitted_sequences_this_volume: &mut std::collections::HashSet<usize>,
) -> u32 {
    use nexrad_data::aws::realtime::download_chunk;

    if targets.is_empty() || state.borrow().stop_requested {
        return 0;
    }

    let results = join_all(targets.iter().map(|id| download_chunk(site_id, id))).await;
    if state.borrow().stop_requested {
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
        let mut s = state.borrow_mut();
        s.results.push(RealtimeResult::ChunkData {
            data: chunk_data,
            chunk_index,
            is_start: false,
            is_end: false,
            timestamp,
            skip_overlap_delete: false,
            is_last_in_sweep,
        });
        s.results.push(RealtimeResult::ChunkReceived {
            chunks_in_volume: chunks_in_volume_start + emitted,
            time_until_next: None,
            is_volume_end: false,
            fetch_latency_ms: 0.0,
            projected_volume_end_available_at_secs: get_projected_volume_end_available_at_secs(
                iter,
            ),
            projected_volume_end_collection_secs: iter.projected_volume_end_collection_secs(),
            chunk_projections: build_chunk_projections(iter),
            arrival_stat: None,
        });
        drop(s);
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
    iter: &StreamingState,
    state: &Rc<RefCell<RealtimeState>>,
    ctx: &egui::Context,
    chunks_in_volume_start: u32,
    timestamp: i64,
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
    let candidate_seqs: Vec<usize> = iter
        .mapper_matching_sequences_in_range(2, upper, |elev| elev.is_some() && filter.accepts(elev))
        .into_iter()
        .filter(|seq| !emitted_sequences_this_volume.contains(seq))
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
        state,
        ctx,
        chunks_in_volume_start,
        timestamp,
        emitted_sequences_this_volume,
    )
    .await
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
    use nexrad_data::aws::realtime::{list_chunks_in_volume, ChunkType};

    log::debug!("Starting realtime streaming for site: {}", site_id);

    // Initialize with a timeout to avoid indefinite waiting when the site has
    // no data or is unreachable. Each .await is a cancellation point — when
    // the timeout wins the select, the init future is dropped, which drops any
    // in-flight HTTP request futures and cancels them.
    const ACQUIRE_TIMEOUT_SECS: u32 = 10;

    // Pad added to the first poll wait per chunk; collapses the "fire a hair
    // early → empty poll → backoff → fire" path on chunks whose predictions
    // are tight. Retry waits are governed by `REALTIME_CHUNK_POLICY`.
    //
    // Sized from observed availability-space prediction error: across all
    // buckets in a representative VCP 212 volume, scheduler predictions ran
    // ~900 ms early in availability-space (collection-space prediction is
    // accurate; the residual is S3-availability lag the projector under-
    // estimates). `wait_after_last_empty_ms` clusters tightly at ~670 ms,
    // confirming that when we fire early we miss by roughly one poll cycle.
    // 750 ms covers the typical case while leaving outliers (one-chunk lag
    // spikes) to the existing retry path. The deeper fix is per-bucket lag
    // in the projector and an EWMA on lag history.
    const POLL_DELAY_AFTER_PREDICTED_MS: u32 = 750;

    let init_future = acquire_streaming_state(&site_id);
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
                // Start chunks are metadata-only and aren't part of any sweep.
                is_last_in_sweep: Some(false),
            });
            // Why: chunk_projections is consumed by the worker fast-path to
            // detect last-chunk-in-sweep. ChunkReceived is the only event
            // that updates it on the main thread, so without this push the
            // resumed sweep's chunks reach the worker with stale/None
            // projections and finalize only on the next sweep's first chunk.
            s.results.push(RealtimeResult::ChunkReceived {
                chunks_in_volume: 1,
                time_until_next: None,
                is_volume_end: false,
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

        // Filter-aware backfill. With `StreamingFilter::All` we backfill the
        // current sweep's preceding chunks (the historical default — keeps
        // the sweep coherent for the renderer). With
        // `StreamingFilter::Elevation(n)` we backfill every already-published
        // chunk of elevation `n` in this volume, which may be earlier sweeps
        // that already finished — that's by design so the user sees their
        // selected elevation immediately on connect.
        let initial_filter = state.borrow().pending_filter;
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

        let backfill_seqs =
            filter_backfill_sequences(&iter, backfill_filter, latest_seq.saturating_sub(1));

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
                        &iter,
                        &state,
                        &ctx,
                        chunks_in_volume,
                        current_scan_start_secs,
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

        if latest_matches || latest_is_end {
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
            let mut s = state.borrow_mut();
            s.results.push(RealtimeResult::ChunkData {
                data: latest_data,
                chunk_index: chunks_in_volume - 1,
                is_start: false,
                is_end: latest_is_end,
                timestamp: current_scan_start_secs,
                skip_overlap_delete: false,
                is_last_in_sweep: latest_is_last_in_sweep,
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
            drop(s);
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
        current_scan_start_secs = provisional_scan_start_secs(
            init_result.latest_chunk.identifier.upload_date_time(),
            &iter,
        );
        chunks_in_volume = 1;
        emitted_sequences_this_volume.insert(init_result.latest_chunk.identifier.sequence());
        cache_volume_number(&site_id, *init_result.latest_chunk.identifier.volume());

        log::debug!(
            "Init: emitting latest_chunk as start ({} bytes)",
            latest_data.len()
        );
        {
            let init_is_last_in_sweep = iter
                .chunk_metadata(init_result.latest_chunk.identifier.sequence())
                .map(|m| m.is_last_in_sweep());
            let mut s = state.borrow_mut();
            s.results.push(RealtimeResult::ChunkData {
                data: latest_data,
                chunk_index: 0,
                is_start: latest_is_start,
                is_end: latest_is_end,
                timestamp: current_scan_start_secs,
                skip_overlap_delete: false,
                is_last_in_sweep: init_is_last_in_sweep,
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
    // Track filter changes across iterations so we can run a mid-stream
    // backfill exactly once per change, and so the in-flight predicted-at
    // diagnostic doesn't outlive its target sequence.
    let mut active_filter: StreamingFilter = state.borrow().pending_filter;
    let mut active_filter_epoch: u64 = state.borrow().filter_epoch;
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

        // Filter-change detection: if the user toggled to a new filter
        // (FilterChanged sleep wake or first iteration after a `set_filter`
        // race), run the mid-stream backfill before re-targeting. Discard
        // stale per-chunk diagnostics — they were aimed at the previous
        // target sequence.
        let live_epoch = state.borrow().filter_epoch;
        if live_epoch != active_filter_epoch {
            let new_filter = state.borrow().pending_filter;
            log::debug!(
                "streaming_loop: filter changed {:?} -> {:?}, resolving target",
                active_filter,
                new_filter,
            );
            cur_predicted_at = None;
            cur_last_empty_at = None;
            cur_diagnostics = None;
            none_retries = 0;
            let emitted = run_mid_stream_backfill(
                &site_id,
                new_filter,
                &iter,
                &state,
                &ctx,
                chunks_in_volume,
                current_scan_start_secs,
                &mut emitted_sequences_this_volume,
            )
            .await;
            chunks_in_volume += emitted;
            active_filter = new_filter;
            active_filter_epoch = live_epoch;
        }

        // Wait for expected chunk time. Only capture the prediction on the
        // first iteration for a given chunk — subsequent retry iterations
        // re-enter here with a near-zero wait, which would overwrite the
        // original estimate.
        let is_first_iter_for_chunk = cur_predicted_at.is_none();
        let (time_until_next_opt, _target_sequence) = match active_filter {
            StreamingFilter::All => (iter.time_until_next().and_then(|d| d.to_std().ok()), None),
            StreamingFilter::Elevation(elev_n) => match iter.next_matching_chunk_diagnostics(
                // accept_end=false: synthesize the volume-boundary signal
                // when the filter excludes the End chunk's elevation rather
                // than wasting a download on data the worker would discard.
                false,
                |elev| active_filter.accepts(elev),
            ) {
                Some((target, diag)) => {
                    let dur = if diag.duration.num_milliseconds() > 0 {
                        diag.duration.to_std().ok()
                    } else {
                        None
                    };
                    if is_first_iter_for_chunk {
                        cur_diagnostics = Some(diag);
                    }
                    (dur, Some(target))
                }
                None => {
                    // Filter excludes every remaining sequence in this volume.
                    // Without a wait estimate here the loop would burn its
                    // retry budget polling for the next volume's Start before
                    // the inter-volume gap (and the intra-volume time before
                    // the user's elevation reappears) has passed. Estimate
                    // the projected availability of the user's elevation in
                    // the next volume; fall back to the legacy single-hop
                    // estimate when projection data isn't available yet.
                    let cross_volume = iter
                        .time_until_next_filtered_chunk_across_volumes(elev_n)
                        .or_else(|| iter.time_until_next().and_then(|d| d.to_std().ok()));
                    if let Some(d) = cross_volume {
                        log::debug!(
                            "streaming_loop: no match remains in current volume for filter \
                             elev {}, sleeping {:.1}s until next-volume target",
                            elev_n,
                            d.as_secs_f64(),
                        );
                    }
                    (cross_volume, None)
                }
            },
        };
        if is_first_iter_for_chunk {
            if let Some(d) = time_until_next_opt {
                cur_predicted_at = Some(current_timestamp_f64() + d.as_secs_f64());
            }
            // Capture single-hop diagnostics for the no-filter path (the
            // multi-hop path already captured above).
            if matches!(active_filter, StreamingFilter::All) {
                cur_diagnostics = iter.next_chunk_processing_diagnostics();
            }
        }
        if let Some(wait_duration) = time_until_next_opt {
            let mut wait_ms = wait_duration.as_millis() as u32;
            // Pad the first (prediction-driven) wait per chunk so we fire
            // slightly after `predicted_available_at`. Retry waits after an
            // empty poll come through the `REALTIME_CHUNK_POLICY` backoff
            // loop below, not here, so the pad applies exactly once per chunk.
            if is_first_iter_for_chunk && wait_ms > 0 {
                wait_ms = wait_ms.saturating_add(POLL_DELAY_AFTER_PREDICTED_MS);
            }
            if wait_ms > 0 {
                match interruptible_sleep(&state, &ctx, wait_ms, active_filter_epoch).await {
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
                if state.borrow().stop_requested {
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
            // Estimate the wait until the user's elevation reappears so the
            // status bar / timeline can show a countdown instead of a stale
            // "receiving…". Without this, the synthetic-end emit hands a
            // `time_until_next: None` to the UI and the phase sticks on
            // Streaming for the whole inter-volume gap.
            let synthetic_time_until_next = match active_filter {
                StreamingFilter::Elevation(elev_n) => iter
                    .time_until_next_filtered_chunk_across_volumes(elev_n)
                    .or_else(|| iter.time_until_next().and_then(|d| d.to_std().ok())),
                StreamingFilter::All => iter.time_until_next().and_then(|d| d.to_std().ok()),
            };
            // Emit a UI-only ChunkReceived so the timeline knows the volume
            // boundary even though no actual End chunk was downloaded.
            chunks_in_volume += 1;
            {
                let mut s = state.borrow_mut();
                s.results.push(RealtimeResult::ChunkReceived {
                    chunks_in_volume,
                    time_until_next: synthetic_time_until_next,
                    is_volume_end: true,
                    fetch_latency_ms: 0.0,
                    projected_volume_end_available_at_secs:
                        get_projected_volume_end_available_at_secs(&iter),
                    projected_volume_end_collection_secs: iter
                        .projected_volume_end_collection_secs(),
                    chunk_projections: build_chunk_projections(&iter),
                    arrival_stat: None,
                });
            }
            ctx.request_repaint();
            // Reset per-chunk tracking; the next iteration will roll over to
            // the next volume's Start via the existing try_next path.
            none_retries = 0;
            cur_predicted_at = None;
            cur_last_empty_at = None;
            cur_diagnostics = None;
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
                    current_scan_start_secs =
                        provisional_scan_start_secs(chunk.identifier.upload_date_time(), &iter);
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

                let (
                    bucket_key,
                    stats_n_at_prediction,
                    scheduler_path,
                    physics_breakdown,
                    predicted_wait_secs,
                ) = match cur_diagnostics.as_ref() {
                    Some(d) => (
                        d.bucket
                            .as_ref()
                            .map(crate::state::BucketKey::from_characteristics),
                        d.stats_n_at_prediction,
                        Some(d.path),
                        d.physics_breakdown,
                        Some(d.duration.num_milliseconds() as f64 / 1000.0),
                    ),
                    None => (None, 0, None, None, None),
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
                    predicted_wait_secs,
                };

                // Reset tracking state for the next chunk
                none_retries = 0;
                cur_predicted_at = None;
                cur_last_empty_at = None;
                cur_diagnostics = None;

                let time_until_next = iter.time_until_next().and_then(|td| td.to_std().ok());
                let chunk_is_last_in_sweep = iter
                    .chunk_metadata(chunk.identifier.sequence())
                    .map(|m| m.is_last_in_sweep());

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
                        is_last_in_sweep: chunk_is_last_in_sweep,
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
            Err(msg) => {
                log::error!("Streaming error: {}", msg);
                let mut s = state.borrow_mut();
                s.results.push(RealtimeResult::Error(msg));
                s.active = false;
                ctx.request_repaint();
                break;
            }
        }
    }

    state.borrow_mut().active = false;
}

/// Either an actual downloaded chunk or a synthetic-volume-end signal from
/// the filter-aware fetch path. Plumbed through the retry loop's `Verdict`
/// so the existing 404 / transient-error handling stays unchanged.
#[derive(Debug)]
enum FilterFetchResult {
    Downloaded(nexrad_data::aws::realtime::DownloadedChunk),
    SyntheticEnd,
}

/// Map a [`super::streaming_state::TryNextOutcome`] to a retry [`Verdict`]
/// for the filter-aware fetch path. Mirrors [`classify_chunk_result`] for
/// the unfiltered path; the only new case is `SyntheticVolumeEnd`, which is
/// not a retry — it's a terminal-for-this-iteration outcome the loop turns
/// into a synthetic `is_volume_end` signal.
fn classify_filter_outcome(
    result: nexrad_data::result::Result<super::streaming_state::TryNextOutcome>,
) -> Verdict<FilterFetchResult> {
    use super::streaming_state::TryNextOutcome;
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

/// Sleep in increments, updating countdown UI and watching for stop +
/// filter-change signals. Returns the reason the sleep ended.
///
/// `wake_epoch` is the `filter_epoch` value the caller observed when it
/// decided how long to sleep — if it differs from the current epoch when we
/// look, the filter has been mutated and the caller should re-evaluate.
async fn interruptible_sleep(
    state: &Rc<RefCell<RealtimeState>>,
    ctx: &egui::Context,
    total_ms: u32,
    wake_epoch: u64,
) -> SleepOutcome {
    const INCREMENT: u32 = 250;
    let mut remaining = total_ms;

    while remaining > 0 {
        {
            let s = state.borrow();
            if s.stop_requested {
                return SleepOutcome::Stopped;
            }
            if s.filter_epoch != wake_epoch {
                return SleepOutcome::FilterChanged;
            }
        }

        state.borrow_mut().time_until_next =
            Some(std::time::Duration::from_millis(remaining as u64));
        ctx.request_repaint();

        let sleep_time = INCREMENT.min(remaining);
        sleep_ms(sleep_time).await;
        remaining = remaining.saturating_sub(INCREMENT);
    }

    state.borrow_mut().time_until_next = None;
    if state.borrow().stop_requested {
        SleepOutcome::Stopped
    } else if state.borrow().filter_epoch != wake_epoch {
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

/// Run `nexrad_data`'s upstream `get_latest_volume` then initialize a
/// [`StreamingState`] at that volume.
///
/// The cached volume number written by [`cache_volume_number`] is intentionally
/// ignored here — discovery uses the same sequential rotated-array binary
/// search the library ships with. The cache will be reintroduced as a hint in
/// a follow-up.
async fn acquire_streaming_state(
    site_id: &str,
) -> nexrad_data::result::Result<super::streaming_state::StreamingInit> {
    let result = nexrad_data::aws::realtime::get_latest_volume(site_id).await?;
    let volume = result.volume.ok_or(nexrad_data::result::Error::AWS(
        nexrad_data::result::aws::AWSError::LatestVolumeNotFound,
    ))?;
    StreamingState::init_at_volume(site_id, volume, result.calls).await
}
