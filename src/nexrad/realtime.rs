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
    /// Projected time this chunk becomes available (Unix seconds).
    /// `Some` for future chunks (from ScanTimingProjection), `None` for past chunks.
    pub projected_time_secs: Option<f64>,
    /// Whether this chunk starts a new sweep.
    pub starts_new_sweep: bool,
    /// 0-based index of this chunk within its sweep.
    pub chunk_index_in_sweep: usize,
    /// Total chunks in this sweep (3 for standard, 6 for super-res).
    pub chunks_in_sweep: usize,
}

/// Result type for realtime streaming events.
#[derive(Clone, Debug)]
pub enum RealtimeResult {
    /// Iterator initialized, streaming started
    Started { site_id: String },
    /// Chunk received from the stream (UI status update)
    ChunkReceived {
        chunks_in_volume: u32,
        time_until_next: Option<Duration>,
        is_volume_end: bool,
        fetch_latency_ms: f64,
        /// Projected volume end time (Unix seconds), from the library's physics model.
        projected_volume_end_secs: Option<f64>,
        /// Per-chunk projection info for the entire volume.
        /// Structural metadata is present for all chunks; projected times only for future chunks.
        chunk_projections: Option<Vec<ChunkProjectionInfo>>,
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
}

/// Build projection info from the streaming state's current position.
///
/// Combines structural metadata (all chunks) with projected timing (future chunks only).
fn build_chunk_projections(state: &StreamingState) -> Option<Vec<ChunkProjectionInfo>> {
    let all_meta = state.all_chunk_metadata()?;
    let projection = state.project_remaining_scan();

    // Build a lookup from sequence → projected_time for future chunks
    let projected_times: std::collections::HashMap<usize, f64> = projection
        .as_ref()
        .map(|p| {
            p.chunks()
                .iter()
                .map(|c| (c.sequence(), c.projected_time().timestamp() as f64))
                .collect()
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
                projected_time_secs: projected_times.get(&meta.sequence()).copied(),
                starts_new_sweep: meta.is_first_in_sweep(),
                chunk_index_in_sweep: meta.chunk_index_in_sweep(),
                chunks_in_sweep: meta.chunks_in_sweep(),
            })
            .collect(),
    )
}

/// Get the projected volume end time from the streaming state.
fn get_projected_volume_end_secs(state: &StreamingState) -> Option<f64> {
    state
        .projected_volume_end_time()
        .map(|dt| dt.timestamp() as f64)
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
        current_scan_start_secs = current_timestamp();

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
                projected_volume_end_secs: get_projected_volume_end_secs(&iter),
                chunk_projections: build_chunk_projections(&iter),
            });
        }
        ctx.request_repaint();
    } else {
        // Joined at volume start: latest_chunk IS the start chunk
        let latest_data = init_result.latest_chunk.chunk.data().to_vec();
        let latest_type = init_result.latest_chunk.identifier.chunk_type();
        let latest_is_start = latest_type == ChunkType::Start;
        let latest_is_end = latest_type == ChunkType::End;
        current_scan_start_secs = current_timestamp();
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
                projected_volume_end_secs: get_projected_volume_end_secs(&iter),
                chunk_projections: build_chunk_projections(&iter),
            });
        }
        ctx.request_repaint();
    }

    // --- Main streaming loop: emit ChunkData per chunk ---
    let mut none_retries: u32 = 0;
    loop {
        // Check stop signal
        if state.borrow().stop_requested {
            log::debug!("Realtime streaming stopped");
            break;
        }

        // Wait for expected chunk time
        if let Some(wait_duration) = iter.time_until_next().and_then(|d| d.to_std().ok()) {
            let wait_ms = wait_duration.as_millis() as u32;
            if wait_ms > 0 && !interruptible_sleep(&state, &ctx, wait_ms).await {
                log::debug!("Realtime streaming stopped");
                break;
            }
        }

        // Fetch next chunk (with timing)
        let chunk_fetch_start = web_time::Instant::now();
        match iter.try_next().await {
            Ok(Some(chunk)) => {
                none_retries = 0;
                let chunk_fetch_ms = chunk_fetch_start.elapsed().as_secs_f64() * 1000.0;
                stats_tracker.update(&stats, &iter);

                let chunk_data = chunk.chunk.data().to_vec();
                let chunk_type = chunk.identifier.chunk_type();
                let is_end = chunk_type == ChunkType::End;
                let is_start = chunk_type == ChunkType::Start;

                // Reset counters on new volume
                if is_start {
                    chunks_in_volume = 0;
                    current_scan_start_secs = current_timestamp();
                    cache_volume_number(&site_id, *chunk.identifier.volume());
                }

                chunks_in_volume += 1;

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
                        projected_volume_end_secs: get_projected_volume_end_secs(&iter),
                        chunk_projections: build_chunk_projections(&iter),
                    });
                }

                ctx.request_repaint();
            }
            Ok(None) => {
                // Chunk not ready yet, brief retry
                none_retries += 1;
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
                            // Recovered — let the next loop iteration handle it normally
                            none_retries = 0;
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
