//! Async channel for loading cache metadata without blocking the UI.
//!
//! This module provides a channel-based interface for loading scan metadata
//! from IndexedDB asynchronously. The UI can request a cache load and poll
//! for results each frame.

use super::cache::NexradCache;
use super::types::ScanMetadata;
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
    #[cfg(target_arch = "wasm32")]
    pub fn load_site_timeline(&self, ctx: Context, cache: NexradCache, site_id: String) {
        // Don't start a new load if one is in progress
        if *self.loading.borrow() {
            log::debug!("Cache load already in progress, ignoring request");
            return;
        }

        *self.loading.borrow_mut() = true;
        let receiver = self.receiver.clone();
        let loading = self.loading.clone();

        wasm_bindgen_futures::spawn_local(async move {
            log::info!("Loading cache metadata for site: {}", site_id);

            let result = match cache.list_metadata_for_site(&site_id).await {
                Ok(metadata) => {
                    log::info!(
                        "Loaded {} cached scan(s) for site {}",
                        metadata.len(),
                        site_id
                    );

                    // Also calculate total cache size across all sites
                    let total_cache_size = cache.total_cache_size().await.unwrap_or(0);
                    log::info!("Total cache size: {} bytes", total_cache_size);

                    CacheLoadResult::Success {
                        site_id,
                        metadata,
                        total_cache_size,
                    }
                }
                Err(e) => {
                    log::error!("Failed to load cache metadata: {}", e);
                    CacheLoadResult::Error(e.to_string())
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

    /// Initiates migration of existing scans to the metadata store.
    ///
    /// Call this once on app startup to ensure metadata exists for all cached scans.
    #[cfg(target_arch = "wasm32")]
    pub fn run_migration(&self, ctx: Context, cache: NexradCache) {
        let loading = self.loading.clone();

        // Don't block on migration, just run it in background
        wasm_bindgen_futures::spawn_local(async move {
            match cache.migrate_existing_scans().await {
                Ok(count) => {
                    if count > 0 {
                        log::info!("Migration complete: {} scans migrated", count);
                        ctx.request_repaint();
                    }
                }
                Err(e) => {
                    log::error!("Migration failed: {}", e);
                }
            }
            // Note: We don't set loading = false here since migration runs independently
            drop(loading);
        });
    }

    /// Clears all cached data.
    ///
    /// After clearing, the cache size will be 0 and timeline will be empty.
    #[cfg(target_arch = "wasm32")]
    pub fn clear_cache(&self, ctx: Context, cache: NexradCache) {
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

            let result = match cache.clear().await {
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
                    CacheLoadResult::Error(e.to_string())
                }
            };

            *receiver.borrow_mut() = Some(result);
            *loading.borrow_mut() = false;

            ctx.request_repaint();
        });
    }

    // Native stubs
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_site_timeline(&self, _ctx: Context, _cache: NexradCache, _site_id: String) {
        // No-op on native
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn run_migration(&self, _ctx: Context, _cache: NexradCache) {
        // No-op on native
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn clear_cache(&self, _ctx: Context, _cache: NexradCache) {
        // No-op on native
    }
}

impl Default for CacheLoadChannel {
    fn default() -> Self {
        Self::new()
    }
}
