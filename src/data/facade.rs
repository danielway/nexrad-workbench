//! Data facade providing a unified interface for IndexedDB storage.
//!
//! Wraps `IndexedDbRecordStore` with cache eviction logic.

use crate::data::indexeddb::IndexedDbRecordStore;
use crate::data::keys::*;

/// Result type for cache operations.
pub type CacheResult<T> = Result<T, String>;

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
    /// Returns (should_evict, scans_evicted) tuple.
    pub async fn check_and_evict(
        &self,
        quota_bytes: u64,
        target_bytes: u64,
    ) -> CacheResult<(bool, u32)> {
        let current_size = self.store.total_cache_size().await?;

        if current_size > quota_bytes {
            log::info!(
                "Cache size {} exceeds quota {}, starting eviction to {}",
                current_size,
                quota_bytes,
                target_bytes
            );
            let evicted = self.store.evict_to_size(target_bytes).await?;
            Ok((true, evicted))
        } else {
            Ok((false, 0))
        }
    }
}
