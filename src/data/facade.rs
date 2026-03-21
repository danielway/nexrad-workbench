//! Data facade providing a unified interface for IndexedDB storage.
//!
//! Wraps `IndexedDbRecordStore` with cache eviction logic.

use crate::data::indexeddb::{DataError, IndexedDbRecordStore};
use crate::data::keys::*;

/// Result type for cache operations.
pub type CacheResult<T> = Result<T, DataError>;

/// Data facade for accessing radar data in IndexedDB.
#[derive(Clone)]
pub struct DataFacade {
    store: IndexedDbRecordStore,
}

impl Default for DataFacade {
    fn default() -> Self {
        Self::new()
    }
}

impl DataFacade {
    pub fn new() -> Self {
        Self {
            store: IndexedDbRecordStore::new(),
        }
    }

    /// Opens the cache database.
    pub async fn open(&self) -> CacheResult<()> {
        self.store.open().await
    }

    /// Gets scan availability information.
    pub async fn scan_availability(&self, scan: &ScanKey) -> CacheResult<Option<ScanIndexEntry>> {
        self.store.scan_availability(scan).await
    }

    /// Queries available scans for a site within a time window.
    pub async fn list_scans(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> CacheResult<Vec<ScanIndexEntry>> {
        self.store.list_scans(site, start, end).await
    }

    /// Gets total cache size.
    pub async fn total_cache_size(&self) -> CacheResult<u64> {
        self.store.total_cache_size().await
    }

    /// Clears all cached data.
    pub async fn clear_all(&self) -> CacheResult<()> {
        self.store.clear_all().await
    }

    /// Checks if eviction is needed and performs it.
    /// Returns `(evicted, scans_evicted, quota_warning)`.
    ///
    /// Checks both the app-level quota and the browser-level storage quota.
    /// If browser quota is critically low (less than 10% remaining), triggers
    /// proactive eviction and returns a warning message.
    pub async fn check_and_evict(
        &self,
        quota_bytes: u64,
        target_bytes: u64,
    ) -> CacheResult<(bool, u32, Option<String>)> {
        let current_size = self.store.total_cache_size().await?;
        let mut total_evicted = 0u32;
        let mut did_evict = false;

        // App-level quota check
        if current_size > quota_bytes {
            log::info!(
                "Cache size {} exceeds quota {}, starting eviction to {}",
                current_size,
                quota_bytes,
                target_bytes
            );
            let evicted = self.store.evict_to_size(target_bytes).await?;
            total_evicted += evicted;
            did_evict = true;
        }

        // Browser-level quota check via navigator.storage.estimate()
        let quota_warning = if let Some(estimate) =
            IndexedDbRecordStore::estimate_storage_quota().await
        {
            let remaining = estimate.remaining();
            let threshold = estimate.quota / 10; // 10% of browser quota

            if remaining < threshold {
                log::warn!(
                    "Browser storage quota critically low: {:.1} MB remaining out of {:.1} MB ({:.0}% used)",
                    remaining as f64 / (1024.0 * 1024.0),
                    estimate.quota as f64 / (1024.0 * 1024.0),
                    (estimate.usage as f64 / estimate.quota as f64) * 100.0,
                );

                // Proactive eviction to free browser storage
                if !did_evict {
                    let evicted = self.store.evict_to_size(target_bytes).await?;
                    if evicted > 0 {
                        total_evicted += evicted;
                        did_evict = true;
                        log::info!(
                            "Proactive eviction: removed {} scans due to low browser quota",
                            evicted
                        );
                    }
                }

                Some(format!(
                    "Storage nearly full: {:.0} MB remaining of {:.0} MB browser quota",
                    remaining as f64 / (1024.0 * 1024.0),
                    estimate.quota as f64 / (1024.0 * 1024.0),
                ))
            } else {
                None
            }
        } else {
            None
        };

        Ok((did_evict, total_evicted, quota_warning))
    }
}
