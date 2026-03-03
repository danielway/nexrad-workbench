//! Data facade that coordinates cache and sweep-based storage.
//!
//! The facade provides a unified interface for accessing radar data,
//! transparently handling caching and source selection.

use crate::data::keys::*;
use crate::data::record_cache::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Data access policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessPolicy {
    /// Use cache if available, fallback to network.
    #[default]
    PreferCache,
    /// Always check cache first, refresh from network if stale.
    #[allow(dead_code)]
    CacheThenNetwork,
    /// Bypass cache, always fetch from network.
    #[allow(dead_code)]
    NetworkOnly,
    /// Only use cache, never fetch from network.
    #[allow(dead_code)]
    CacheOnly,
}

/// Data facade that coordinates cache and network sources.
#[derive(Clone)]
pub struct DataFacade {
    cache: WasmRecordCache,
    policy: Rc<RefCell<AccessPolicy>>,
}

impl Default for DataFacade {
    fn default() -> Self {
        Self::new()
    }
}

impl DataFacade {
    pub fn new() -> Self {
        Self {
            cache: WasmRecordCache::new(),
            policy: Rc::new(RefCell::new(AccessPolicy::default())),
        }
    }

    /// Opens the cache database.
    pub async fn open(&self) -> CacheResult<()> {
        self.cache.open().await
    }

    /// Gets the current access policy.
    #[allow(dead_code)]
    pub fn policy(&self) -> AccessPolicy {
        *self.policy.borrow()
    }

    /// Sets the access policy.
    #[allow(dead_code)]
    pub fn set_policy(&self, policy: AccessPolicy) {
        *self.policy.borrow_mut() = policy;
    }

    /// Gets the underlying cache for direct operations.
    pub fn cache(&self) -> &WasmRecordCache {
        &self.cache
    }

    // ========================================================================
    // Scan operations
    // ========================================================================

    /// Updates sweep metadata on a scan index entry after decode.
    pub async fn update_scan_sweep_meta(
        &self,
        scan: &ScanKey,
        end_timestamp_secs: i64,
        sweeps: Vec<SweepMeta>,
    ) -> CacheResult<bool> {
        self.cache
            .update_scan_sweep_meta(scan, end_timestamp_secs, sweeps)
            .await
    }

    // ========================================================================
    // Query operations
    // ========================================================================

    /// Queries available scans for a site within a time window.
    pub async fn list_scans(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> CacheResult<Vec<ScanIndexEntry>> {
        self.cache.list_scans(site, start, end).await
    }

    /// Gets availability ranges for timeline display.
    pub async fn availability_ranges(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> CacheResult<Vec<TimeRange>> {
        self.cache.availability_ranges(site, start, end).await
    }

    // ========================================================================
    // Utility operations
    // ========================================================================

    /// Gets total cache size.
    pub async fn total_cache_size(&self) -> CacheResult<u64> {
        self.cache.total_cache_size().await
    }

    /// Clears all cached data.
    pub async fn clear_all(&self) -> CacheResult<()> {
        self.cache.clear_all().await
    }

    /// Gets scans sorted by LRU (oldest first) for eviction.
    pub async fn get_lru_scans(&self, limit: u32) -> CacheResult<Vec<ScanIndexEntry>> {
        self.cache.get_lru_scans(limit).await
    }

    /// Deletes a scan and all its sweeps. Returns bytes freed.
    pub async fn delete_scan(&self, scan: &ScanKey) -> CacheResult<u64> {
        self.cache.delete_scan(scan).await
    }

    /// Evicts scans until total cache size is below target_bytes.
    /// Returns the number of scans evicted.
    pub async fn evict_to_size(&self, target_bytes: u64) -> CacheResult<u32> {
        self.cache.evict_to_size(target_bytes).await
    }

    /// Checks if eviction is needed and performs it.
    /// Returns (should_evict, scans_evicted) tuple.
    pub async fn check_and_evict(
        &self,
        quota_bytes: u64,
        target_bytes: u64,
    ) -> CacheResult<(bool, u32)> {
        let current_size = self.cache.total_cache_size().await?;

        if current_size > quota_bytes {
            log::info!(
                "Cache size {} exceeds quota {}, starting eviction to {}",
                current_size,
                quota_bytes,
                target_bytes
            );
            let evicted = self.cache.evict_to_size(target_bytes).await?;
            Ok((true, evicted))
        } else {
            Ok((false, 0))
        }
    }
}
