//! Async channel for loading cache metadata without blocking the UI.
//!
//! This module provides a channel-based interface for loading scan metadata
//! from IndexedDB asynchronously. The UI can request a cache load and poll
//! for results each frame.

use super::types::ScanMetadata;
use crate::data::{DataFacade, SiteId, UnixMillis};
use eframe::egui::Context;
use std::cell::RefCell;
use std::rc::Rc;

/// Result of a cache load operation.
#[derive(Debug, Clone)]
pub enum CacheLoadResult {
    /// Successfully loaded metadata for a site
    Success {
        site_id: String,
        metadata: Vec<ScanMetadata>,
        /// Total cache size across all sites (in bytes)
        total_cache_size: u64,
    },
    /// Cache load failed with an error
    Error(String),
}

/// Channel for async cache loading operations.
///
/// Allows the UI to request metadata loading from IndexedDB without blocking.
/// The sender/receiver pattern enables the async operation to complete
/// while the UI continues rendering.
pub struct CacheLoadChannel {
    /// Receiver for completed cache loads
    receiver: Rc<RefCell<Option<CacheLoadResult>>>,
    /// Flag indicating a load is in progress
    loading: Rc<RefCell<bool>>,
}

impl CacheLoadChannel {
    /// Creates a new cache load channel.
    pub fn new() -> Self {
        Self {
            receiver: Rc::new(RefCell::new(None)),
            loading: Rc::new(RefCell::new(false)),
        }
    }

    /// Returns true if a cache load is currently in progress.
    pub fn is_loading(&self) -> bool {
        *self.loading.borrow()
    }

    /// Initiates an async load of timeline metadata for a site.
    ///
    /// If a load is already in progress, this call is ignored.
    /// Results can be retrieved via `try_recv()`.
    pub fn load_site_timeline(&self, ctx: Context, facade: DataFacade, site_id: String) {
        // Don't start a new load if one is in progress
        if *self.loading.borrow() {
            log::debug!("Cache load already in progress, ignoring request");
            return;
        }

        *self.loading.borrow_mut() = true;
        let receiver = self.receiver.clone();
        let loading = self.loading.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let t_total = web_time::Instant::now();
            log::info!("Loading cache metadata for site: {}", site_id);

            // Query scan index
            let site = SiteId::new(&site_id);
            let start = UnixMillis(0);
            let end = UnixMillis::now();

            let result = match facade.list_scans(&site, start, end).await {
                Ok(scan_entries) => {
                    let list_ms = t_total.elapsed().as_secs_f64() * 1000.0;

                    // Convert ScanIndexEntry to ScanMetadata for UI compatibility
                    let metadata: Vec<ScanMetadata> = scan_entries
                        .iter()
                        .map(|entry| {
                            use super::types::ScanKey;
                            ScanMetadata {
                                key: ScanKey::new(
                                    &entry.scan.site.0,
                                    entry.scan.scan_start.as_secs(),
                                ),
                                file_name: entry.file_name.clone().unwrap_or_default(),
                                file_size: entry.total_size_bytes,
                                end_timestamp: entry.end_timestamp_secs,
                                vcp: None,
                                completeness: Some(entry.completeness()),
                                present_records: Some(entry.present_records),
                                expected_records: entry.expected_records,
                                sweeps: entry.sweeps.clone(),
                            }
                        })
                        .collect();

                    // Also calculate total cache size across all sites
                    let total_cache_size = facade.total_cache_size().await.unwrap_or(0);

                    let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
                    log::info!(
                        "Timeline loaded: {} scan(s) for {} in {:.0}ms (list_scans: {:.0}ms)",
                        metadata.len(),
                        site_id,
                        total_ms,
                        list_ms,
                    );

                    CacheLoadResult::Success {
                        site_id,
                        metadata,
                        total_cache_size,
                    }
                }
                Err(e) => {
                    log::error!("Failed to load cache metadata: {}", e);
                    CacheLoadResult::Error(e)
                }
            };

            *receiver.borrow_mut() = Some(result);
            *loading.borrow_mut() = false;

            // Request a repaint to process the result
            ctx.request_repaint();
        });
    }

    /// Non-blocking receive for cache load results.
    ///
    /// Returns `Some(result)` if a cache load has completed, `None` otherwise.
    pub fn try_recv(&self) -> Option<CacheLoadResult> {
        self.receiver.borrow_mut().take()
    }

    /// Clears all cached data.
    ///
    /// After clearing, the cache size will be 0 and timeline will be empty.
    pub fn clear_cache(&self, ctx: Context, facade: DataFacade) {
        // Don't start if a load is in progress
        if *self.loading.borrow() {
            log::debug!("Cache operation in progress, ignoring clear request");
            return;
        }

        *self.loading.borrow_mut() = true;
        let receiver = self.receiver.clone();
        let loading = self.loading.clone();

        wasm_bindgen_futures::spawn_local(async move {
            log::info!("Clearing cache...");

            let result = match facade.clear_all().await {
                Ok(()) => {
                    log::info!("Cache cleared successfully");
                    CacheLoadResult::Success {
                        site_id: String::new(),
                        metadata: Vec::new(),
                        total_cache_size: 0,
                    }
                }
                Err(e) => {
                    log::error!("Failed to clear cache: {}", e);
                    CacheLoadResult::Error(e)
                }
            };

            *receiver.borrow_mut() = Some(result);
            *loading.borrow_mut() = false;

            ctx.request_repaint();
        });
    }
}

impl Default for CacheLoadChannel {
    fn default() -> Self {
        Self::new()
    }
}
