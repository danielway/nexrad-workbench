//! Data facade providing a unified interface for IndexedDB storage.
//!
//! Wraps `IndexedDbStore` with cache eviction logic.

use crate::data::indexeddb::{DataError, IndexedDbStore};
use crate::data::keys::*;
use crate::data::quota::{decide_eviction, QuotaPolicy};

/// Result type for cache operations.
pub type CacheResult<T> = Result<T, DataError>;

/// Data facade for accessing radar data in IndexedDB.
#[derive(Clone)]
pub struct DataFacade {
    store: IndexedDbStore,
}

impl Default for DataFacade {
    fn default() -> Self {
        Self::new()
    }
}

impl DataFacade {
    pub fn new() -> Self {
        Self {
            store: IndexedDbStore::new(),
        }
    }

    /// Build a facade over a specific store. Only used from `tests/idb.rs`
    /// (which links the lib target) to run against a throwaway database —
    /// hence dead in the bin.
    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn with_store(store: IndexedDbStore) -> Self {
        Self { store }
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
    /// The decision (app-level quota check + browser-level pressure check)
    /// is the pure [`decide_eviction`]; this method just gathers the sizes
    /// and executes the outcome.
    pub async fn check_and_evict(
        &self,
        quota_bytes: u64,
        target_bytes: u64,
    ) -> CacheResult<(bool, u32, Option<String>)> {
        let current_size = self.store.total_cache_size().await?;
        let estimate = IndexedDbStore::estimate_storage_quota().await;
        let decision = decide_eviction(
            current_size,
            quota_bytes,
            target_bytes,
            estimate,
            &QuotaPolicy::DEFAULT,
        );

        if let Some(warning) = &decision.warning {
            log::warn!(
                "Browser storage quota critically low: {:.1} MB remaining out of {:.1} MB",
                warning.remaining_bytes as f64 / (1024.0 * 1024.0),
                warning.browser_quota_bytes as f64 / (1024.0 * 1024.0),
            );
        }

        let mut total_evicted = 0u32;
        if let Some(evict_to) = decision.evict_to {
            log::info!(
                "Cache size {} (app quota {}) / browser pressure {} → evicting to {}",
                current_size,
                quota_bytes,
                decision.warning.is_some(),
                evict_to
            );
            total_evicted = self.store.evict_to_size(evict_to).await?;
        }

        Ok((
            total_evicted > 0,
            total_evicted,
            decision.warning.map(|w| w.message()),
        ))
    }
}
