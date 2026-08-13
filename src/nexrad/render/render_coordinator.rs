//! Render coordinator: owns the decode worker pool and render request deduplication.
//!
//! Consolidates the five tightly-coupled fields that were scattered between
//! WorkbenchApp and Renderers into a single owner.

use super::render_request::VolumeRenderRequest;
use crate::core::SweepIdentity;
use crate::data::ScanKey;
use crate::nexrad::decode::decode_worker::{default_pool_size, WorkerOutcome, WorkerPool};

/// Coordinates render requests to a pool of decode workers, deduplicating
/// identical requests and owning the current scan/elevation state.
///
/// The pool is chosen to saturate the ingest pipeline with parallel
/// decompress/decode work while keeping the UI thread responsive — see
/// [`default_pool_size`] for the sizing heuristic.
pub(crate) struct RenderCoordinator {
    /// Pool of Web Workers for offloading expensive NEXRAD operations.
    worker: Option<WorkerPool>,
    /// Identity of the currently displayed scan. Stored as the typed
    /// [`ScanKey`] rather than its serialized form so callers compare by
    /// value, not by string formatting; the storage-key string is built at
    /// the worker boundary via [`ScanKey::to_storage_key`].
    current_scan_key: Option<ScanKey>,
    /// Elevations the GPU can render right now for the *currently
    /// displayed* scan, populated from worker ingest results in either the
    /// live or archive flow. Distinct from
    /// [`crate::core::VolumeElevationRoster::received`], which is the
    /// in-progress *live* volume's observation roster — different scope,
    /// different lifecycle. Don't mix the two.
    available_elevations: Vec<u8>,
    /// Identity of the last single-elevation render request, for dedup.
    /// Compared structurally — equal identities mean the same on-disk sweep
    /// and the request is suppressed.
    last_render: Option<SweepIdentity>,
    /// Previous volume render parameters for change detection.
    last_volume_render: Option<VolumeRenderRequest>,
}

impl RenderCoordinator {
    pub(crate) fn new(worker: Option<WorkerPool>) -> Self {
        Self {
            worker,
            current_scan_key: None,
            available_elevations: Vec::new(),
            last_render: None,
            last_volume_render: None,
        }
    }

    /// Whether a decode worker is available.
    pub(crate) fn has_worker(&self) -> bool {
        self.worker.is_some()
    }

    /// Current scan key, if any.
    pub(crate) fn scan_key(&self) -> Option<&ScanKey> {
        self.current_scan_key.as_ref()
    }

    /// Available elevation numbers for the current scan.
    pub(crate) fn available_elevations(&self) -> &[u8] {
        &self.available_elevations
    }

    /// Set the current scan key and available elevations (after ingest).
    pub(crate) fn set_scan(&mut self, key: ScanKey, elevations: Vec<u8>) {
        self.current_scan_key = Some(key);
        self.available_elevations = elevations;
    }

    /// Set just the scan key (e.g. during scrub or chunk ingest).
    pub(crate) fn set_scan_key(&mut self, key: ScanKey) {
        if self.current_scan_key.as_ref() != Some(&key) {
            self.available_elevations.clear();
            self.last_render = None;
        }
        self.current_scan_key = Some(key);
    }

    /// Add newly-completed elevations (used during chunk ingest).
    pub(crate) fn add_elevations(&mut self, new: &[u8]) {
        for &elev in new {
            if !self.available_elevations.contains(&elev) {
                self.available_elevations.push(elev);
                self.available_elevations.sort_unstable();
            }
        }
    }

    /// Clear render state for a site change.
    pub(crate) fn clear_for_site_change(&mut self) {
        self.current_scan_key = None;
        self.available_elevations.clear();
        self.last_render = None;
        self.last_volume_render = None;
    }

    /// Force the next render request to go through (clears dedup cache).
    pub(crate) fn force_fresh_render(&mut self) {
        self.last_render = None;
        self.last_volume_render = None;
    }

    /// Clear only the scan key (e.g. when no scan is in range).
    pub(crate) fn clear_scan_key(&mut self) {
        self.current_scan_key = None;
        self.last_render = None;
    }

    /// Send a render request for an explicit sweep identity. Returns true
    /// if the request was actually sent (false if deduplicated or no
    /// worker).
    pub(crate) fn request_render_for(&mut self, identity: SweepIdentity) -> bool {
        let Some(ref mut worker) = self.worker else {
            return false;
        };

        if !crate::core::render::should_dispatch(&identity, self.last_render.as_ref()) {
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
        self.last_render = Some(identity);
        worker.render(scan_key, elevation_number, product);
        true
    }

    /// Send a volume render request. Returns true if actually sent.
    pub(crate) fn request_volume_render(&mut self, product: &str) -> bool {
        let Some(ref scan_key) = self.current_scan_key else {
            log::debug!("Volume render skipped: no scan key");
            return false;
        };
        let Some(ref mut worker) = self.worker else {
            log::debug!("Volume render skipped: no worker");
            return false;
        };
        if self.available_elevations.is_empty() {
            log::warn!("Volume render skipped: no elevation numbers available");
            return false;
        }

        let request = VolumeRenderRequest {
            scan_key: scan_key.clone(),
            product: product.to_string(),
        };

        if !crate::core::render::should_dispatch(&request, self.last_volume_render.as_ref()) {
            return false;
        }

        log::debug!(
            "Requesting volume render: {} product={} elevations={:?}",
            scan_key,
            product,
            self.available_elevations,
        );

        let scan_key_typed = scan_key.clone();
        let elev_nums = self.available_elevations.clone();
        self.last_volume_render = Some(request);
        worker.render_volume(scan_key_typed, product.to_string(), elev_nums);
        true
    }

    /// Send a live render request (partial sweep, no dedup).
    pub(crate) fn render_live(&mut self, elevation_number: u8, product: String) {
        if let Some(ref mut worker) = self.worker {
            worker.render_live(elevation_number, product);
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

    /// Store a prefetch render identity in the dedup cache (to prevent
    /// re-sending the same prefetch request).
    pub(crate) fn set_last_render(&mut self, identity: SweepIdentity) {
        self.last_render = Some(identity);
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
    fn new_has_no_worker_and_empty_state() {
        let c = RenderCoordinator::new(None);
        assert!(!c.has_worker());
        assert!(c.scan_key().is_none());
        assert!(c.available_elevations().is_empty());
    }

    #[wasm_bindgen_test]
    fn set_scan_sets_key_and_elevations() {
        let mut c = RenderCoordinator::new(None);
        c.set_scan(key("KDMX", 1_700_000_000_000), vec![1, 2, 3]);
        assert_eq!(c.scan_key(), Some(&key("KDMX", 1_700_000_000_000)));
        assert_eq!(c.available_elevations(), &[1, 2, 3]);
    }

    #[wasm_bindgen_test]
    fn set_scan_key_clears_elevations_for_new_scan() {
        let mut c = RenderCoordinator::new(None);
        c.set_scan(key("KDMX", 100), vec![4, 5]);
        c.set_scan_key(key("KFTG", 200));
        assert_eq!(c.scan_key(), Some(&key("KFTG", 200)));
        assert!(c.available_elevations().is_empty());
    }

    #[wasm_bindgen_test]
    fn set_same_scan_key_preserves_incremental_elevations() {
        let mut c = RenderCoordinator::new(None);
        let scan = key("KDMX", 100);
        c.set_scan(scan.clone(), vec![4, 5]);
        c.set_scan_key(scan);
        assert_eq!(c.available_elevations(), &[4, 5]);
    }

    #[wasm_bindgen_test]
    fn add_elevations_keeps_sorted_unique() {
        let mut c = RenderCoordinator::new(None);
        c.add_elevations(&[5, 3, 9]);
        assert_eq!(c.available_elevations(), &[3, 5, 9]);
    }

    #[wasm_bindgen_test]
    fn add_elevations_dedups_existing() {
        let mut c = RenderCoordinator::new(None);
        c.add_elevations(&[1, 2]);
        c.add_elevations(&[2, 1, 4]);
        assert_eq!(c.available_elevations(), &[1, 2, 4]);
    }

    #[wasm_bindgen_test]
    fn add_elevations_no_duplicate_when_re_added() {
        let mut c = RenderCoordinator::new(None);
        c.add_elevations(&[7]);
        c.add_elevations(&[7]);
        assert_eq!(c.available_elevations(), &[7]);
    }

    #[wasm_bindgen_test]
    fn add_elevations_merges_into_existing_sorted() {
        let mut c = RenderCoordinator::new(None);
        c.set_scan(key("KABC", 1), vec![2, 6]);
        c.add_elevations(&[4]);
        assert_eq!(c.available_elevations(), &[2, 4, 6]);
    }

    #[wasm_bindgen_test]
    fn clear_for_site_change_resets_everything() {
        let mut c = RenderCoordinator::new(None);
        c.set_scan(key("KDMX", 5), vec![1, 2]);
        c.clear_for_site_change();
        assert!(c.scan_key().is_none());
        assert!(c.available_elevations().is_empty());
    }

    #[wasm_bindgen_test]
    fn clear_scan_key_clears_key_but_keeps_elevations() {
        let mut c = RenderCoordinator::new(None);
        c.set_scan(key("KDMX", 5), vec![1, 2]);
        c.clear_scan_key();
        assert!(c.scan_key().is_none());
        assert_eq!(c.available_elevations(), &[1, 2]);
    }

    #[wasm_bindgen_test]
    fn request_render_for_returns_false_without_worker() {
        let mut c = RenderCoordinator::new(None);
        let sent = c.request_render_for(identity("KDMX", 100, 1, "ref"));
        assert!(!sent);
    }

    #[wasm_bindgen_test]
    fn request_volume_render_false_without_scan_key() {
        let mut c = RenderCoordinator::new(None);
        // no scan key set -> short-circuits to false
        assert!(!c.request_volume_render("ref"));
    }

    #[wasm_bindgen_test]
    fn request_volume_render_false_without_worker() {
        let mut c = RenderCoordinator::new(None);
        c.set_scan(key("KDMX", 100), vec![1, 2]);
        // scan key present + elevations present, but no worker -> false
        assert!(!c.request_volume_render("ref"));
    }

    #[wasm_bindgen_test]
    fn try_recv_empty_without_worker() {
        let mut c = RenderCoordinator::new(None);
        assert!(c.try_recv().is_empty());
    }

    #[wasm_bindgen_test]
    fn force_fresh_render_is_safe_without_worker() {
        let mut c = RenderCoordinator::new(None);
        // exercises the dedup-cache clear path; no observable getter, just
        // assert it does not affect scan state.
        c.set_scan(key("KDMX", 1), vec![3]);
        c.force_fresh_render();
        assert_eq!(c.scan_key(), Some(&key("KDMX", 1)));
        assert_eq!(c.available_elevations(), &[3]);
    }

    #[wasm_bindgen_test]
    fn set_last_render_does_not_send_without_worker() {
        let mut c = RenderCoordinator::new(None);
        // priming the dedup cache should not affect public state and should
        // keep request_render_for returning false (still no worker).
        c.set_last_render(identity("KDMX", 1, 1, "ref"));
        assert!(!c.request_render_for(identity("KDMX", 1, 1, "ref")));
    }

    #[wasm_bindgen_test]
    fn set_scan_overwrites_prior_elevations() {
        let mut c = RenderCoordinator::new(None);
        c.set_scan(key("KDMX", 1), vec![1, 2, 3]);
        c.set_scan(key("KDMX", 2), vec![9]);
        assert_eq!(c.scan_key(), Some(&key("KDMX", 2)));
        assert_eq!(c.available_elevations(), &[9]);
    }
}
