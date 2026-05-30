//! Render coordinator: owns the decode worker pool and render request deduplication.
//!
//! Consolidates the five tightly-coupled fields that were scattered between
//! WorkbenchApp and Renderers into a single owner.

use super::decode_worker::{default_pool_size, WorkerOutcome, WorkerPool};
use super::render_request::VolumeRenderRequest;
use crate::data::ScanKey;
use crate::state::SweepIdentity;

/// Coordinates render requests to a pool of decode workers, deduplicating
/// identical requests and owning the current scan/elevation state.
///
/// The pool is chosen to saturate the ingest pipeline with parallel
/// decompress/decode work while keeping the UI thread responsive — see
/// [`default_pool_size`] for the sizing heuristic.
pub struct RenderCoordinator {
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
    /// [`crate::state::VolumeElevationRoster::received`], which is the
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
    pub fn new(worker: Option<WorkerPool>) -> Self {
        Self {
            worker,
            current_scan_key: None,
            available_elevations: Vec::new(),
            last_render: None,
            last_volume_render: None,
        }
    }

    /// Whether a decode worker is available.
    pub fn has_worker(&self) -> bool {
        self.worker.is_some()
    }

    /// Current scan key, if any.
    pub fn scan_key(&self) -> Option<&ScanKey> {
        self.current_scan_key.as_ref()
    }

    /// Available elevation numbers for the current scan.
    pub fn available_elevations(&self) -> &[u8] {
        &self.available_elevations
    }

    /// Set the current scan key and available elevations (after ingest).
    pub fn set_scan(&mut self, key: ScanKey, elevations: Vec<u8>) {
        self.current_scan_key = Some(key);
        self.available_elevations = elevations;
    }

    /// Set just the scan key (e.g. during scrub or chunk ingest).
    pub fn set_scan_key(&mut self, key: ScanKey) {
        self.current_scan_key = Some(key);
    }

    /// Add newly-completed elevations (used during chunk ingest).
    pub fn add_elevations(&mut self, new: &[u8]) {
        for &elev in new {
            if !self.available_elevations.contains(&elev) {
                self.available_elevations.push(elev);
                self.available_elevations.sort_unstable();
            }
        }
    }

    /// Clear render state for a site change.
    pub fn clear_for_site_change(&mut self) {
        self.current_scan_key = None;
        self.available_elevations.clear();
        self.last_render = None;
        self.last_volume_render = None;
    }

    /// Force the next render request to go through (clears dedup cache).
    pub fn force_fresh_render(&mut self) {
        self.last_render = None;
        self.last_volume_render = None;
    }

    /// Clear only the scan key (e.g. when no scan is in range).
    pub fn clear_scan_key(&mut self) {
        self.current_scan_key = None;
        self.last_render = None;
    }

    /// Send a render request for an explicit sweep identity. Returns true
    /// if the request was actually sent (false if deduplicated or no
    /// worker).
    pub fn request_render_for(&mut self, identity: SweepIdentity) -> bool {
        let Some(ref mut worker) = self.worker else {
            return false;
        };

        if self.last_render.as_ref() == Some(&identity) {
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
    pub fn request_volume_render(&mut self, product: &str) -> bool {
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

        if self.last_volume_render.as_ref() == Some(&request) {
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
    pub fn render_live(&mut self, elevation_number: u8, product: String) {
        if let Some(ref mut worker) = self.worker {
            worker.render_live(elevation_number, product);
        }
    }

    /// Forward raw bytes to worker for ingest. When `wanted_elevations` is
    /// non-empty, the worker stores only those cuts (filter-scoped fetch).
    #[allow(clippy::too_many_arguments)]
    pub fn ingest(
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
    pub fn ingest_chunk(
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
    pub fn render_direct(&mut self, scan_key: &ScanKey, elevation_number: u8, product: String) {
        if let Some(ref mut worker) = self.worker {
            worker.render(scan_key.clone(), elevation_number, product);
        }
    }

    /// Drain all pending worker results.
    pub fn try_recv(&mut self) -> Vec<WorkerOutcome> {
        if let Some(ref mut worker) = self.worker {
            worker.try_recv()
        } else {
            Vec::new()
        }
    }

    /// Try to create a new decode worker pool (retry after failure).
    pub fn create_worker(&mut self, ctx: eframe::egui::Context) -> Result<(), String> {
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
    pub fn set_last_render(&mut self, identity: SweepIdentity) {
        self.last_render = Some(identity);
    }
}
