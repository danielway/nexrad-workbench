//! Render coordinator: owns the decode worker and render request deduplication.
//!
//! Consolidates the five tightly-coupled fields that were scattered between
//! WorkbenchApp and Renderers into a single owner.

use super::decode_worker::{DecodeWorker, WorkerOutcome};
use super::render_request::{RenderRequest, VolumeRenderRequest};

/// Coordinates render requests to the decode worker, deduplicating
/// identical requests and owning the current scan/elevation state.
pub struct RenderCoordinator {
    /// Web Worker for offloading expensive NEXRAD operations.
    worker: Option<DecodeWorker>,
    /// Scan key of the currently displayed scan ("SITE|TIMESTAMP_MS").
    current_scan_key: Option<String>,
    /// Available elevation numbers for the current scan (from ingest).
    available_elevations: Vec<u8>,
    /// Previous render parameters for change detection.
    last_render: Option<RenderRequest>,
    /// Previous volume render parameters for change detection.
    last_volume_render: Option<VolumeRenderRequest>,
}

impl RenderCoordinator {
    pub fn new(worker: Option<DecodeWorker>) -> Self {
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
    pub fn scan_key(&self) -> Option<&str> {
        self.current_scan_key.as_deref()
    }

    /// Available elevation numbers for the current scan.
    pub fn available_elevations(&self) -> &[u8] {
        &self.available_elevations
    }

    /// Set the current scan key and available elevations (after ingest).
    pub fn set_scan(&mut self, key: String, elevations: Vec<u8>) {
        self.current_scan_key = Some(key);
        self.available_elevations = elevations;
    }

    /// Set just the scan key (e.g. during scrub or chunk ingest).
    pub fn set_scan_key(&mut self, key: String) {
        self.current_scan_key = Some(key);
    }

    /// Set the full elevation list (replacing, not merging).
    pub fn set_elevations(&mut self, elevations: Vec<u8>) {
        self.available_elevations = elevations;
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

    /// Pick the closest available elevation to the requested one.
    pub fn best_available_elevation(&self, requested: u8) -> u8 {
        self.available_elevations
            .iter()
            .copied()
            .min_by_key(|&e| (e as i16 - requested as i16).unsigned_abs())
            .unwrap_or(requested)
    }

    /// Send a render request to the worker. Returns true if the request was
    /// actually sent (false if deduplicated or no worker/scan key).
    pub fn request_render(&mut self, elevation_number: u8, product: &str, is_auto: bool) -> bool {
        let Some(ref scan_key) = self.current_scan_key else {
            return false;
        };
        let Some(ref mut worker) = self.worker else {
            return false;
        };

        let request = RenderRequest {
            scan_key: scan_key.clone(),
            elevation_number,
            product: product.to_string(),
            is_auto,
        };

        if self.last_render.as_ref() == Some(&request) {
            return false;
        }

        log::info!(
            "Requesting worker decode: {} elev={} product={}",
            scan_key,
            elevation_number,
            product,
        );

        let scan_key = scan_key.clone();
        self.last_render = Some(request);
        worker.render(scan_key, elevation_number, product.to_string());
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

        log::info!(
            "Requesting volume render: {} product={} elevations={:?}",
            scan_key,
            product,
            self.available_elevations,
        );

        let scan_key = scan_key.clone();
        let elev_nums = self.available_elevations.clone();
        self.last_volume_render = Some(request);
        worker.render_volume(scan_key, product.to_string(), elev_nums);
        true
    }

    /// Send a live render request (partial sweep, no dedup).
    pub fn render_live(&mut self, elevation_number: u8, product: String) {
        if let Some(ref mut worker) = self.worker {
            worker.render_live(elevation_number, product);
        }
    }

    /// Forward raw bytes to worker for ingest.
    pub fn ingest(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp: i64,
        file_name: String,
        fetch_latency: f64,
    ) {
        if let Some(ref mut worker) = self.worker {
            worker.ingest(data, site_id, timestamp, file_name, fetch_latency);
        }
    }

    /// Forward a chunk to worker for incremental ingest.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_chunk(
        &mut self,
        data: Vec<u8>,
        site_id: String,
        timestamp: i64,
        chunk_index: u32,
        is_start: bool,
        is_end: bool,
        file_name: String,
        skip_overlap_delete: bool,
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
                skip_overlap_delete,
                is_last_in_sweep,
            );
        }
    }

    /// Send a direct render request (used by prefetch/prev-sweep, bypasses dedup).
    pub fn render_direct(&mut self, scan_key: String, elevation_number: u8, product: String) {
        if let Some(ref mut worker) = self.worker {
            worker.render(scan_key, elevation_number, product);
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

    /// Try to create a new decode worker (retry after failure).
    pub fn create_worker(&mut self, ctx: eframe::egui::Context) -> Result<(), String> {
        match DecodeWorker::new(ctx) {
            Ok(w) => {
                log::info!("Decode worker created successfully");
                self.worker = Some(w);
                Ok(())
            }
            Err(e) => {
                log::warn!("Failed to create decode worker: {}", e);
                Err(format!("Decode worker failed to initialize: {}", e))
            }
        }
    }

    /// Store a prefetch render request in the dedup cache (to prevent
    /// re-sending the same prefetch request).
    pub fn set_last_render(&mut self, request: RenderRequest) {
        self.last_render = Some(request);
    }
}
