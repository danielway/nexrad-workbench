//! Real-time NEXRAD streaming channel.
//!
//! Provides a channel-based interface for real-time NEXRAD data streaming
//! from AWS using the ChunkIterator from nexrad-data.

use super::download::NetworkStats;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use eframe::egui;

use crate::data::facade::DataFacade;

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
    },
    /// Raw chunk data for incremental ingest
    ChunkData {
        data: Vec<u8>,
        chunk_index: u32,
        is_start: bool,
        is_end: bool,
        timestamp: i64,
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

async fn streaming_loop(
    ctx: egui::Context,
    site_id: String,
    state: Rc<RefCell<RealtimeState>>,
    stats: NetworkStats,
    _facade: DataFacade,
) {
    use nexrad_data::aws::realtime::{
        download_chunk, list_chunks_in_volume, ChunkIterator, ChunkType,
    };

    log::info!("Starting realtime streaming for site: {}", site_id);

    // Initialize iterator with a timeout to avoid indefinite waiting when
    // the site has no data or is unreachable. ChunkIterator::start() performs
    // a binary search over 999 round-robin volume directories (~10 S3 LIST
    // requests) plus chunk fetches (~2-3 more). Each .await is a cancellation
    // point — when the timeout wins the select, the init future is dropped,
    // which drops any in-flight HTTP request future and cancels it.
    const ACQUIRE_TIMEOUT_SECS: u32 = 10;
    let init_future = ChunkIterator::start(&site_id);
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

    let mut iter = init_result.iterator;
    let mut stats_tracker = StatsTracker::new(&iter);
    stats_tracker.update(&stats, &iter);

    log::info!(
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
        // Joined mid-volume: emit the start chunk, backfill intermediates, then latest
        let start_data = start_chunk.chunk.data().to_vec();
        current_scan_start_secs = current_timestamp();

        log::info!(
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
            });
        }
        ctx.request_repaint();

        // Backfill: download all intermediate chunks between start and latest
        let latest_seq = init_result.latest_chunk.identifier.sequence();
        let volume = *init_result.latest_chunk.identifier.volume();
        chunks_in_volume = 1; // start chunk already emitted

        if latest_seq > 2 {
            // List all chunks in the volume to get their identifiers
            match list_chunks_in_volume(&site_id, volume, 100).await {
                Ok(chunk_ids) => {
                    // Filter to intermediate chunks (after start, before latest)
                    let intermediates: Vec<_> = chunk_ids
                        .into_iter()
                        .filter(|id| id.sequence() > 1 && id.sequence() < latest_seq)
                        .collect();

                    let total_backfill = intermediates.len();
                    log::info!(
                        "Backfill: downloading {} intermediate chunks (seq 2..{})",
                        total_backfill,
                        latest_seq - 1
                    );

                    for chunk_id in &intermediates {
                        if state.borrow().stop_requested {
                            break;
                        }

                        match download_chunk(&site_id, chunk_id).await {
                            Ok((_id, chunk)) => {
                                chunks_in_volume += 1;
                                let chunk_data = chunk.data().to_vec();
                                let chunk_type = chunk_id.chunk_type();
                                log::debug!(
                                    "Backfill: chunk seq {} ({} bytes, {:?})",
                                    chunk_id.sequence(),
                                    chunk_data.len(),
                                    chunk_type
                                );
                                {
                                    let mut s = state.borrow_mut();
                                    s.results.push(RealtimeResult::ChunkData {
                                        data: chunk_data,
                                        chunk_index: chunks_in_volume - 1,
                                        is_start: false,
                                        is_end: false,
                                        timestamp: current_scan_start_secs,
                                    });
                                }
                                ctx.request_repaint();
                            }
                            Err(e) => {
                                log::warn!(
                                    "Backfill: failed to download chunk seq {}: {}",
                                    chunk_id.sequence(),
                                    e
                                );
                            }
                        }
                    }

                    log::info!(
                        "Backfill: completed, {} chunks downloaded",
                        chunks_in_volume - 1
                    );
                }
                Err(e) => {
                    log::warn!("Backfill: failed to list chunks: {}, skipping backfill", e);
                }
            }
        }

        // Emit the latest chunk (where the iterator is positioned)
        let latest_data = init_result.latest_chunk.chunk.data().to_vec();
        let latest_type = init_result.latest_chunk.identifier.chunk_type();
        let latest_is_end = latest_type == ChunkType::End;
        chunks_in_volume += 1;

        log::info!(
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
            });
            s.results.push(RealtimeResult::ChunkReceived {
                chunks_in_volume,
                time_until_next: iter.time_until_next().and_then(|td| td.to_std().ok()),
                is_volume_end: latest_is_end,
                fetch_latency_ms: 0.0,
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

        log::info!(
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
            });
            s.results.push(RealtimeResult::ChunkReceived {
                chunks_in_volume,
                time_until_next: iter.time_until_next().and_then(|td| td.to_std().ok()),
                is_volume_end: latest_is_end,
                fetch_latency_ms: 0.0,
            });
        }
        ctx.request_repaint();
    }

    // --- Main streaming loop: emit ChunkData per chunk ---
    loop {
        // Check stop signal
        if state.borrow().stop_requested {
            log::info!("Realtime streaming stopped");
            break;
        }

        // Wait for expected chunk time
        if let Some(wait_duration) = iter.time_until_next().and_then(|d| d.to_std().ok()) {
            let wait_ms = wait_duration.as_millis() as u32;
            if wait_ms > 0 && !interruptible_sleep(&state, &ctx, wait_ms).await {
                log::info!("Realtime streaming stopped");
                break;
            }
        }

        // Fetch next chunk (with timing)
        let chunk_fetch_start = web_time::Instant::now();
        match iter.try_next().await {
            Ok(Some(chunk)) => {
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
                    });
                    // Emit UI status update
                    s.results.push(RealtimeResult::ChunkReceived {
                        chunks_in_volume,
                        time_until_next,
                        is_volume_end: is_end,
                        fetch_latency_ms: chunk_fetch_ms,
                    });
                }

                ctx.request_repaint();
            }
            Ok(None) => {
                // Chunk not ready yet, brief retry
                sleep_ms(500).await;
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
    fn new(iter: &nexrad_data::aws::realtime::ChunkIterator) -> Self {
        Self {
            last_requests: iter.requests_made(),
            last_bytes: iter.bytes_downloaded(),
        }
    }

    fn update(&mut self, stats: &NetworkStats, iter: &nexrad_data::aws::realtime::ChunkIterator) {
        let new_requests = iter.requests_made().saturating_sub(self.last_requests);
        let new_bytes = iter.bytes_downloaded().saturating_sub(self.last_bytes);

        for _ in 0..new_requests {
            stats.request_started();
            stats.request_completed(0);
        }
        if new_bytes > 0 {
            *stats.total_bytes.borrow_mut() += new_bytes;
        }

        self.last_requests = iter.requests_made();
        self.last_bytes = iter.bytes_downloaded();
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
