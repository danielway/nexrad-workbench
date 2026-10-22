//! Render coordinator: owns the decode worker pool and sends render requests.
//!
//! The pool is shared by every site. What each site is rendering, and the
//! dedup inputs for its requests, is that site's
//! [`RenderTrack`](crate::core::render::RenderTrack); the request methods take
//! the track they are sending for.

use crate::core::render::{RenderTrack, VolumeRenderRequest};
use crate::core::SweepIdentity;
use crate::data::ScanKey;
use crate::nexrad::decode::decode_worker::{default_pool_size, WorkerOutcome, WorkerPool};

/// Sends render requests to a pool of decode workers, deduplicating each
/// against the requesting site's [`RenderTrack`].
///
/// The pool is chosen to saturate the ingest pipeline with parallel
/// decompress/decode work while keeping the UI thread responsive — see
/// [`default_pool_size`] for the sizing heuristic.
pub(crate) struct RenderCoordinator {
    /// Pool of Web Workers for offloading expensive NEXRAD operations.
    worker: Option<WorkerPool>,
}

impl RenderCoordinator {
    pub(crate) fn new(worker: Option<WorkerPool>) -> Self {
        Self { worker }
    }

    /// Whether a decode worker is available.
    pub(crate) fn has_worker(&self) -> bool {
        self.worker.is_some()
    }

    /// Send a render request for an explicit sweep identity, unless `track`
    /// sent the same one last. Returns true if the request was actually sent
    /// (false if deduplicated or no worker).
    pub(crate) fn request_render_for(
        &mut self,
        track: &mut RenderTrack,
        identity: SweepIdentity,
    ) -> bool {
        let Some(ref mut worker) = self.worker else {
            return false;
        };

        if !track.admits_sweep(&identity) {
            return false;
        }

        log::debug!(
            "Requesting worker decode: {} elev={} product={}",
            identity.scan_key,
            identity.elevation_number,
            identity.product,
        );

        let scan_key = identity.scan_key.clone();
        let elevation_number = identity.elevation_number;
        let product = identity.product.clone();
        track.set_last_render(identity);
        worker.render(scan_key, elevation_number, product);
        true
    }

    /// Send a volume render request for `track`'s scan. Returns true if
    /// actually sent.
    pub(crate) fn request_volume_render(&mut self, track: &mut RenderTrack, product: &str) -> bool {
        let Some(scan_key) = track.scan_key().cloned() else {
            log::debug!("Volume render skipped: no scan key");
            return false;
        };
        let Some(ref mut worker) = self.worker else {
            log::debug!("Volume render skipped: no worker");
            return false;
        };
        if track.available_elevations().is_empty() {
            log::warn!("Volume render skipped: no elevation numbers available");
            return false;
        }

        let request = VolumeRenderRequest {
            scan_key: scan_key.clone(),
            product: product.to_string(),
        };

        if !track.admits_volume(&request) {
            return false;
        }

        log::debug!(
            "Requesting volume render: {} product={} elevations={:?}",
            scan_key,
            product,
            track.available_elevations(),
        );

        let elev_nums = track.available_elevations().to_vec();
        track.set_last_volume_render(request);
        worker.render_volume(scan_key, product.to_string(), elev_nums);
        true
    }

    /// Send a live render request (partial sweep, no dedup) for `site`'s
    /// stream.
    pub(crate) fn render_live(
        &mut self,
        site: crate::data::SiteId,
        elevation_number: u8,
        product: String,
    ) {
        if let Some(ref mut worker) = self.worker {
            worker.render_live(site, elevation_number, product);
        }
    }

    /// Tell the worker to forget `site`'s live accumulator; the site left
    /// the map, or its element now points at another radar.
    pub(crate) fn drop_site(&mut self, site: crate::data::SiteId) {
        if let Some(ref mut worker) = self.worker {
            worker.drop_site(site);
        }
    }

    /// Post a storm cell detection. Returns `false` when there is no worker
    /// to run it.
    pub(crate) fn detect(&mut self, request: crate::core::storm_cells::DetectRequest) -> bool {
        match self.worker {
            Some(ref mut worker) => {
                worker.detect(request);
                true
            }
            None => false,
        }
    }

    /// Submit a composite storm cell detection. Returns false when there is
    /// no worker pool to take it.
    pub(crate) fn detect_composite(
        &mut self,
        request: crate::core::storm_cells::SceneDetectRequest,
    ) -> bool {
        match self.worker {
            Some(ref mut worker) => {
                worker.detect_composite(request);
                true
            }
            None => false,
        }
    }

    /// Forward raw bytes to worker for ingest. When `wanted_elevations` is
    /// non-empty, the worker stores only those cuts (filter-scoped fetch).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ingest(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp: f64,
        file_name: String,
        fetch_latency: f64,
        wanted_elevations: Vec<u8>,
    ) {
        if let Some(ref mut worker) = self.worker {
            worker.ingest(
                data,
                site_id,
                timestamp,
                file_name,
                fetch_latency,
                wanted_elevations,
            );
        }
    }

    /// Forward a chunk to worker for incremental ingest.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ingest_chunk(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp: f64,
        chunk_index: u32,
        source_sequence: u32,
        elevation_number: Option<u8>,
        chunk_index_in_sweep: Option<u8>,
        chunks_in_sweep: Option<u8>,
        is_start: bool,
        is_end: bool,
        file_name: String,
        is_last_in_sweep: bool,
    ) {
        if let Some(ref mut worker) = self.worker {
            worker.ingest_chunk(
                data,
                site_id,
                timestamp,
                chunk_index,
                source_sequence,
                elevation_number,
                chunk_index_in_sweep,
                chunks_in_sweep,
                is_start,
                is_end,
                file_name,
                is_last_in_sweep,
            );
        }
    }

    /// Send a direct render request (used by prefetch/prev-sweep, bypasses dedup).
    pub(crate) fn render_direct(
        &mut self,
        scan_key: &ScanKey,
        elevation_number: u8,
        product: String,
    ) {
        if let Some(ref mut worker) = self.worker {
            worker.render(scan_key.clone(), elevation_number, product);
        }
    }

    /// Drain all pending worker results.
    pub(crate) fn try_recv(&mut self) -> Vec<WorkerOutcome> {
        if let Some(ref mut worker) = self.worker {
            worker.try_recv()
        } else {
            Vec::new()
        }
    }

    /// Outstanding worker jobs, or an empty load when no pool exists (worker
    /// creation failed, or hasn't been attempted yet).
    pub(crate) fn worker_load(&self) -> crate::core::WorkerLoad {
        self.worker
            .as_ref()
            .map(WorkerPool::load)
            .unwrap_or_default()
    }

    /// Try to create a new decode worker pool (retry after failure).
    pub(crate) fn create_worker(&mut self, ctx: eframe::egui::Context) -> Result<(), String> {
        match WorkerPool::new(ctx, default_pool_size()) {
            Ok(pool) => {
                self.worker = Some(pool);
                Ok(())
            }
            Err(e) => {
                log::warn!("Failed to create decode worker pool: {}", e);
                Err(format!("Decode worker failed to initialize: {}", e))
            }
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn key(site: &str, ms: i64) -> ScanKey {
        ScanKey::new(site, crate::data::keys::UnixMillis(ms))
    }

    fn identity(site: &str, ms: i64, elev: u8, product: &str) -> SweepIdentity {
        SweepIdentity::new(key(site, ms), elev, product)
    }

    #[wasm_bindgen_test]
    fn new_has_no_worker() {
        let c = RenderCoordinator::new(None);
        assert!(!c.has_worker());
    }

    #[wasm_bindgen_test]
    fn request_render_for_returns_false_without_worker() {
        let mut c = RenderCoordinator::new(None);
        let mut track = RenderTrack::default();
        let sent = c.request_render_for(&mut track, identity("KDMX", 100, 1, "ref"));
        assert!(!sent);
        // Nothing was sent, so nothing is recorded for dedup.
        assert!(track.last_render().is_none());
    }

    #[wasm_bindgen_test]
    fn request_volume_render_false_without_scan_key() {
        let mut c = RenderCoordinator::new(None);
        let mut track = RenderTrack::default();
        // no scan key set -> short-circuits to false
        assert!(!c.request_volume_render(&mut track, "ref"));
    }

    #[wasm_bindgen_test]
    fn request_volume_render_false_without_worker() {
        let mut c = RenderCoordinator::new(None);
        let mut track = RenderTrack::default();
        track.set_scan(key("KDMX", 100), vec![1, 2]);
        // scan key present + elevations present, but no worker -> false
        assert!(!c.request_volume_render(&mut track, "ref"));
        assert!(track.last_volume_render().is_none());
    }

    #[wasm_bindgen_test]
    fn try_recv_empty_without_worker() {
        let mut c = RenderCoordinator::new(None);
        assert!(c.try_recv().is_empty());
    }

    #[wasm_bindgen_test]
    fn primed_dedup_does_not_send_without_worker() {
        let mut c = RenderCoordinator::new(None);
        let mut track = RenderTrack::default();
        // priming the dedup cache keeps request_render_for returning false
        // (still no worker).
        track.set_last_render(identity("KDMX", 1, 1, "ref"));
        assert!(!c.request_render_for(&mut track, identity("KDMX", 1, 1, "ref")));
    }
}
