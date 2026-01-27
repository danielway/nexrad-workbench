//! Real-time NEXRAD streaming channel.
//!
//! Provides a channel-based interface for real-time NEXRAD data streaming
//! from AWS using the ChunkIterator from nexrad-data.

use super::download::NetworkStats;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

#[cfg(target_arch = "wasm32")]
use eframe::egui;

use crate::data::facade::{process_realtime_chunk, DataFacade};
use crate::data::keys::ScanKey;

/// Result type for realtime streaming events.
#[derive(Clone, Debug)]
pub enum RealtimeResult {
    /// Iterator initialized, streaming started
    Started { site_id: String },
    /// Chunk received from the stream
    ChunkReceived {
        chunks_in_volume: u32,
        time_until_next: Option<Duration>,
        is_volume_end: bool,
        fetch_latency_ms: f64,
    },
    /// Record stored in cache (emitted for each chunk)
    RecordStored {
        scan_key: ScanKey,
        record_id: u32,
        records_available: u32,
    },
    /// Partial volume successfully decoded (some sweeps available)
    PartialVolumeReady {
        scan_key: ScanKey,
        sweep_count: usize,
        timestamp_ms: i64,
    },
    /// Volume complete (all chunks received and assembled)
    VolumeComplete { data: Vec<u8>, timestamp: i64 },
    /// Error occurred during streaming
    Error(String),
}

/// Internal state for the realtime streaming channel.
struct RealtimeState {
    results: Vec<RealtimeResult>,
    active: bool,
    time_until_next: Option<Duration>,
    stop_requested: bool,
}

impl Default for RealtimeState {
    fn default() -> Self {
        Self {
            results: Vec::new(),
            active: false,
            time_until_next: None,
            stop_requested: false,
        }
    }
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

    #[cfg(target_arch = "wasm32")]
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

    #[cfg(not(target_arch = "wasm32"))]
    pub fn start(&self, _ctx: egui::Context, site_id: String, _facade: DataFacade) {
        let mut state = self.state.borrow_mut();
        state.results.push(RealtimeResult::Error(format!(
            "Realtime streaming not implemented for native platform (site: {})",
            site_id
        )));
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

#[cfg(target_arch = "wasm32")]
async fn streaming_loop(
    ctx: egui::Context,
    site_id: String,
    state: Rc<RefCell<RealtimeState>>,
    stats: NetworkStats,
    facade: DataFacade,
) {
    use nexrad_data::aws::realtime::{ChunkIterator, ChunkType};

    log::info!("Starting realtime streaming for site: {}", site_id);

    // Initialize iterator
    let init_result = match ChunkIterator::start(&site_id).await {
        Ok(init) => init,
        Err(e) => {
            let mut s = state.borrow_mut();
            s.results.push(RealtimeResult::Error(format!(
                "Failed to initialize: {}",
                e
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
        s.results
            .push(RealtimeResult::Started { site_id: site_id.clone() });
    }
    ctx.request_repaint();

    let mut volume_data: Vec<u8> = Vec::new();
    let mut chunks_in_volume: u32 = 0;
    // Track current scan for record storage
    let mut current_scan_start_secs: i64 = current_timestamp();
    let mut record_seq: u32 = 0;

    loop {
        // Check stop signal
        if state.borrow().stop_requested {
            log::info!("Realtime streaming stopped");
            break;
        }

        // Wait for expected chunk time
        if let Some(wait_duration) = iter.time_until_next().and_then(|d| d.to_std().ok()) {
            let wait_ms = wait_duration.as_millis() as u32;
            if wait_ms > 0 {
                // Wait in increments, updating countdown UI
                if !interruptible_sleep(&state, &ctx, wait_ms).await {
                    log::info!("Realtime streaming stopped");
                    break;
                }
            }
        }

        // Fetch next chunk (with timing)
        let chunk_fetch_start = web_time::Instant::now();
        match iter.try_next().await {
            Ok(Some(chunk)) => {
                let chunk_fetch_ms = chunk_fetch_start.elapsed().as_secs_f64() * 1000.0;
                stats_tracker.update(&stats, &iter);

                let chunk_data = chunk.chunk.data();
                let chunk_type = chunk.identifier.chunk_type();
                let is_end = chunk_type == ChunkType::End;
                let is_start = chunk_type == ChunkType::Start;

                // Reset on new volume
                if is_start {
                    volume_data.clear();
                    chunks_in_volume = 0;
                    current_scan_start_secs = current_timestamp();
                    record_seq = 0;
                }

                chunks_in_volume += 1;
                volume_data.extend_from_slice(chunk_data);

                let time_until_next = iter.time_until_next().and_then(|td| td.to_std().ok());

                {
                    let mut s = state.borrow_mut();
                    s.results.push(RealtimeResult::ChunkReceived {
                        chunks_in_volume,
                        time_until_next,
                        is_volume_end: is_end,
                        fetch_latency_ms: chunk_fetch_ms,
                    });
                }

                // Store record immediately in cache
                let is_first_chunk = record_seq == 0;
                match process_realtime_chunk(
                    &facade,
                    &site_id,
                    current_scan_start_secs,
                    record_seq,
                    chunk_data,
                    is_first_chunk,
                )
                .await
                {
                    Ok(record_key) => {
                        record_seq += 1;
                        let scan_key = record_key.scan.clone();

                        // Emit RecordStored event
                        {
                            let mut s = state.borrow_mut();
                            s.results.push(RealtimeResult::RecordStored {
                                scan_key: scan_key.clone(),
                                record_id: record_key.record_id,
                                records_available: record_seq,
                            });
                        }

                        // Attempt incremental decode every few records (to avoid overhead)
                        // Decode after every 3rd record, or on volume end
                        if record_seq >= 3 && (record_seq % 3 == 0 || is_end) {
                            if let Ok(volume) = facade.decode_available_records(&scan_key).await {
                                let sweep_count = volume.sweeps().len();
                                let timestamp_ms =
                                    scan_key.scan_start.as_millis();
                                log::debug!(
                                    "Partial decode succeeded: {} sweeps at {}",
                                    sweep_count,
                                    timestamp_ms
                                );
                                let mut s = state.borrow_mut();
                                s.results.push(RealtimeResult::PartialVolumeReady {
                                    scan_key,
                                    sweep_count,
                                    timestamp_ms,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to store realtime chunk: {}", e);
                        // Continue anyway - don't break the stream
                    }
                }

                if is_end {
                    let volume_size = volume_data.len();
                    {
                        let mut s = state.borrow_mut();
                        s.results.push(RealtimeResult::VolumeComplete {
                            data: std::mem::take(&mut volume_data),
                            timestamp: current_scan_start_secs,
                        });
                    }
                    log::info!("Volume complete: {} bytes", volume_size);
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
#[cfg(target_arch = "wasm32")]
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

#[cfg(target_arch = "wasm32")]
struct StatsTracker {
    last_requests: usize,
    last_bytes: u64,
}

#[cfg(target_arch = "wasm32")]
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
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as i64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

#[cfg(target_arch = "wasm32")]
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
