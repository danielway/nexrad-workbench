//! IndexedDB cache layer for NEXRAD scan data.
//!
//! Provides persistent caching of downloaded NEXRAD scans to avoid
//! repeated downloads of the same data.

use super::types::{CachedScan, ScanKey};
use crate::storage::{KeyValueStore, StorageConfig, StorageError};

#[cfg(target_arch = "wasm32")]
use crate::storage::IndexedDbStore;

/// Cache for NEXRAD scan data.
///
/// Uses IndexedDB on WASM targets to persist downloaded scans.
/// Scans are stored keyed by site_id + timestamp for efficient lookup.
#[derive(Clone)]
pub struct NexradCache {
    #[cfg(target_arch = "wasm32")]
    store: IndexedDbStore,
}

impl NexradCache {
    /// Creates a new NEXRAD cache instance.
    ///
    /// Uses the "nexrad-scans" object store in the nexrad-workbench database.
    pub fn new() -> Self {
        Self {
            #[cfg(target_arch = "wasm32")]
            store: IndexedDbStore::new(StorageConfig {
                database_name: "nexrad-workbench".to_string(),
                store_name: "nexrad-scans".to_string(),
                version: 2, // Bump version to create new object store
            }),
        }
    }

    /// Retrieves a cached scan by its key.
    ///
    /// Returns `Ok(None)` if the scan is not in cache.
    #[cfg(target_arch = "wasm32")]
    pub async fn get(&self, key: &ScanKey) -> Result<Option<CachedScan>, StorageError> {
        self.store.get(&key.to_storage_key()).await
    }

    /// Stores a scan in the cache.
    #[cfg(target_arch = "wasm32")]
    pub async fn put(&self, scan: &CachedScan) -> Result<(), StorageError> {
        self.store.put(&scan.key.to_storage_key(), scan).await
    }

    /// Lists all cached scan keys for a given site.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)]
    pub async fn list_keys_for_site(&self, site_id: &str) -> Result<Vec<ScanKey>, StorageError> {
        let all_keys = self.store.get_all_keys().await?;
        let prefix = format!("{}_", site_id);

        Ok(all_keys
            .iter()
            .filter(|k| k.starts_with(&prefix))
            .filter_map(|k| ScanKey::from_storage_key(k))
            .collect())
    }

    /// Deletes a cached scan by its key.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)]
    pub async fn delete(&self, key: &ScanKey) -> Result<(), StorageError> {
        self.store.delete(&key.to_storage_key()).await
    }

    /// Clears all cached scans.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)]
    pub async fn clear(&self) -> Result<(), StorageError> {
        self.store.clear().await
    }

    // Native stubs - these do nothing on native builds
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn get(&self, _key: &ScanKey) -> Result<Option<CachedScan>, StorageError> {
        Ok(None)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn put(&self, _scan: &CachedScan) -> Result<(), StorageError> {
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn list_keys_for_site(&self, _site_id: &str) -> Result<Vec<ScanKey>, StorageError> {
        Ok(Vec::new())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn delete(&self, _key: &ScanKey) -> Result<(), StorageError> {
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn clear(&self) -> Result<(), StorageError> {
        Ok(())
    }
}

impl Default for NexradCache {
    fn default() -> Self {
        Self::new()
    }
}
