//! Pool of decode workers for parallel ingest and render.
//!
//! Dispatch strategy:
//! - `ingest` (archive) — round-robin across all workers so concurrent downloads
//!   don't serialize on a single bzip2/decode pipeline.
//! - `render`, `render_volume` — round-robin; render requests just read from
//!   IDB and every worker has its own connection.
//! - `ingest_chunk` and `render_live` — pinned to worker 0 because the live
//!   accumulator (`CHUNK_ACCUM`) is a per-worker thread-local.
//!
//! Each worker in the pool owns its own JS `Worker` handle and its own
//! `pending_*` maps, so message correlation remains per-worker. `try_recv`
//! drains outcomes from every worker into a single vector.

use super::DecodeWorker;
use super::WorkerOutcome;
use eframe::egui;

/// Index of the worker that exclusively handles live chunk ingest and
/// partial-sweep render_live requests (these rely on a thread-local
/// accumulator, so they must stay pinned to one worker).
const LIVE_WORKER_INDEX: usize = 0;

/// Pool of [`DecodeWorker`]s that parallelizes archive ingest and render.
pub struct WorkerPool {
    workers: Vec<DecodeWorker>,
    next_ingest: usize,
    next_render: usize,
}

impl WorkerPool {
    /// Create a pool of `count` workers. `count` is clamped to at least 1.
    ///
    /// Returns an error if **any** worker fails to spawn — the caller then
    /// treats the pool as unavailable and surfaces a retry affordance, just
    /// as it did for a single worker.
    pub fn new(ctx: egui::Context, count: usize) -> Result<Self, String> {
        let count = count.max(1);
        let mut workers = Vec::with_capacity(count);
        for _ in 0..count {
            workers.push(DecodeWorker::new(ctx.clone())?);
        }
        log::debug!("WorkerPool spawned with {} worker(s)", workers.len());
        Ok(Self {
            workers,
            next_ingest: 0,
            next_render: 0,
        })
    }

    /// Number of workers in the pool.
    #[allow(dead_code)]
    pub fn size(&self) -> usize {
        self.workers.len()
    }

    fn next_ingest_index(&mut self) -> usize {
        let idx = self.next_ingest % self.workers.len();
        self.next_ingest = self.next_ingest.wrapping_add(1);
        idx
    }

    fn next_render_index(&mut self) -> usize {
        let idx = self.next_render % self.workers.len();
        self.next_render = self.next_render.wrapping_add(1);
        idx
    }

    /// Submit an archive ingest (full file) — round-robined across workers.
    pub fn ingest(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp_secs: i64,
        file_name: String,
        fetch_latency_ms: f64,
    ) {
        let idx = self.next_ingest_index();
        self.workers[idx].ingest(data, site_id, timestamp_secs, file_name, fetch_latency_ms);
    }

    /// Submit a per-chunk ingest — pinned to the live-worker slot so the
    /// accumulator thread-local stays consistent across chunks of the same
    /// volume.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_chunk(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp_secs: i64,
        chunk_index: u32,
        is_start: bool,
        is_end: bool,
        file_name: String,
        skip_overlap_delete: bool,
        is_last_in_sweep: bool,
    ) {
        self.workers[LIVE_WORKER_INDEX].ingest_chunk(
            data,
            site_id,
            timestamp_secs,
            chunk_index,
            is_start,
            is_end,
            file_name,
            skip_overlap_delete,
            is_last_in_sweep,
        );
    }

    /// Submit an archive render — round-robined across workers.
    pub fn render(&mut self, scan_key: String, elevation_number: u8, product: String) {
        let idx = self.next_render_index();
        self.workers[idx].render(scan_key, elevation_number, product);
    }

    /// Submit a live (partial) render — pinned to the live worker because it
    /// reads the in-memory accumulator populated by `ingest_chunk`.
    pub fn render_live(&mut self, elevation_number: u8, product: String) {
        self.workers[LIVE_WORKER_INDEX].render_live(elevation_number, product);
    }

    /// Submit a volume render — round-robined across workers.
    pub fn render_volume(&mut self, scan_key: String, product: String, elevation_numbers: Vec<u8>) {
        let idx = self.next_render_index();
        self.workers[idx].render_volume(scan_key, product, elevation_numbers);
    }

    /// Drain pending outcomes from every worker.
    pub fn try_recv(&mut self) -> Vec<WorkerOutcome> {
        let mut out = Vec::new();
        for worker in &mut self.workers {
            out.extend(worker.try_recv());
        }
        out
    }
}

/// Determine a sensible worker pool size from `navigator.hardwareConcurrency`.
///
/// We reserve one core for the UI thread and cap the pool at 4 workers to
/// avoid the diminishing returns of over-subscription on laptops. Fallback is
/// 2 workers when the browser doesn't expose hardware concurrency.
pub fn default_pool_size() -> usize {
    let hw = web_sys::window()
        .map(|w| w.navigator().hardware_concurrency() as usize)
        .unwrap_or(0);

    if hw == 0 {
        return 2;
    }
    hw.saturating_sub(1).clamp(1, 4)
}
