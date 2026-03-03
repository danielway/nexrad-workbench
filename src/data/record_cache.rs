//! Record cache abstraction layer.
//!
//! Provides a high-level interface for querying scan metadata and managing
//! the sweep-based cache. The actual storage is implemented by
//! `IndexedDbRecordStore` backed by IndexedDB.

use crate::data::keys::*;

use crate::data::indexeddb::IndexedDbRecordStore;

/// Result type for cache operations.
pub type CacheResult<T> = Result<T, String>;

/// Record cache implementation using IndexedDB.
#[derive(Clone, Default)]
pub struct WasmRecordCache {
    store: IndexedDbRecordStore,
}

impl WasmRecordCache {
    pub fn new() -> Self {
        Self {
            store: IndexedDbRecordStore::new(),
        }
    }

    /// Opens the database.
    pub async fn open(&self) -> CacheResult<()> {
        self.store.open().await
    }

    /// Gets the underlying store for advanced operations.
    pub fn store(&self) -> &IndexedDbRecordStore {
        &self.store
    }

    /// Updates sweep metadata on a scan index entry after decode.
    pub async fn update_scan_sweep_meta(
        &self,
        scan: &ScanKey,
        end_timestamp_secs: i64,
        sweeps: Vec<SweepMeta>,
    ) -> CacheResult<bool> {
        self.store
            .update_scan_sweep_meta(scan, end_timestamp_secs, sweeps)
            .await
    }

    /// Gets scan availability information.
    pub async fn scan_availability(&self, scan: &ScanKey) -> CacheResult<Option<ScanIndexEntry>> {
        self.store.scan_availability(scan).await
    }

    /// Gets availability ranges for a site within a time window.
    pub async fn availability_ranges(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> CacheResult<Vec<TimeRange>> {
        self.store.availability_ranges(site, start, end).await
    }

    /// Lists all scans for a site within a time window.
    pub async fn list_scans(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> CacheResult<Vec<ScanIndexEntry>> {
        self.store.list_scans(site, start, end).await
    }

    /// Updates scan index with expected record count.
    pub async fn set_expected_records(&self, scan: &ScanKey, expected: u32) -> CacheResult<()> {
        self.store.set_expected_records(scan, expected).await
    }

    /// Gets total cache size.
    pub async fn total_cache_size(&self) -> CacheResult<u64> {
        self.store.total_cache_size().await
    }

    /// Clears all cached data.
    pub async fn clear_all(&self) -> CacheResult<()> {
        self.store.clear_all().await
    }

    /// Gets scans sorted by last_accessed_at (oldest first) for LRU eviction.
    pub async fn get_lru_scans(&self, limit: u32) -> CacheResult<Vec<ScanIndexEntry>> {
        self.store.get_lru_scans(limit).await
    }

    /// Deletes a scan and all its sweeps. Returns bytes freed.
    pub async fn delete_scan(&self, scan: &ScanKey) -> CacheResult<u64> {
        self.store.delete_scan(scan).await
    }

    /// Evicts scans until total cache size is below target_bytes.
    /// Returns the number of scans evicted.
    pub async fn evict_to_size(&self, target_bytes: u64) -> CacheResult<u32> {
        self.store.evict_to_size(target_bytes).await
    }
}
