//! IndexedDB cache layer for NEXRAD scan data.
//!
//! Provides persistent caching of downloaded NEXRAD scans to avoid
//! repeated downloads of the same data. Uses a dual-store architecture:
//! - "nexrad-scans": Full scan data (1-5MB each)
//! - "scan-metadata": Lightweight metadata for timeline display (~100 bytes)

use super::types::{CachedScan, ScanKey, ScanMetadata};
use crate::storage::{KeyValueStore, StorageConfig, StorageError};

#[cfg(target_arch = "wasm32")]
use crate::storage::IndexedDbStore;

/// Cache for NEXRAD scan data.
///
/// Uses IndexedDB on WASM targets to persist downloaded scans.
/// Maintains two object stores:
/// - `nexrad-scans`: Full scan data for rendering
/// - `scan-metadata`: Lightweight metadata for timeline queries
#[derive(Clone)]
pub struct NexradCache {
    /// Store for full scan data
    #[cfg(target_arch = "wasm32")]
    scan_store: IndexedDbStore,
    /// Store for lightweight metadata (timeline display)
    #[cfg(target_arch = "wasm32")]
    metadata_store: IndexedDbStore,
}

impl NexradCache {
    /// Creates a new NEXRAD cache instance.
    ///
    /// Initializes both the scan store and metadata store.
    pub fn new() -> Self {
        Self {
            #[cfg(target_arch = "wasm32")]
            scan_store: IndexedDbStore::new(StorageConfig {
                database_name: "nexrad-workbench".to_string(),
                store_name: "nexrad-scans".to_string(),
                version: 3,
            }),
            #[cfg(target_arch = "wasm32")]
            metadata_store: IndexedDbStore::new(StorageConfig {
                database_name: "nexrad-workbench".to_string(),
                store_name: "scan-metadata".to_string(),
                version: 3,
            }),
        }
    }

    /// Retrieves a cached scan by its key.
    ///
    /// Returns `Ok(None)` if the scan is not in cache.
    #[cfg(target_arch = "wasm32")]
    pub async fn get(&self, key: &ScanKey) -> Result<Option<CachedScan>, StorageError> {
        self.scan_store.get(&key.to_storage_key()).await
    }

    /// Stores a scan in the cache along with its metadata.
    ///
    /// Writes to both the scan store and metadata store.
    #[cfg(target_arch = "wasm32")]
    pub async fn put(&self, scan: &CachedScan) -> Result<(), StorageError> {
        let storage_key = scan.key.to_storage_key();

        // Write full scan data
        self.scan_store.put(&storage_key, scan).await?;

        // Write lightweight metadata
        let metadata = ScanMetadata::from_cached_scan(scan);
        self.metadata_store.put(&storage_key, &metadata).await?;

        Ok(())
    }

    /// Stores a scan with decoded metadata (end timestamp, VCP).
    ///
    /// Use this when the scan has been decoded and additional info is available.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)] // API method for future use when decoding VCP/end time is implemented
    pub async fn put_with_metadata(
        &self,
        scan: &CachedScan,
        end_timestamp: Option<i64>,
        vcp: Option<u16>,
    ) -> Result<(), StorageError> {
        let storage_key = scan.key.to_storage_key();

        // Write full scan data
        self.scan_store.put(&storage_key, scan).await?;

        // Write metadata with decoded info
        let metadata = ScanMetadata::from_cached_scan_with_info(scan, end_timestamp, vcp);
        self.metadata_store.put(&storage_key, &metadata).await?;

        Ok(())
    }

    /// Lists all cached scan metadata for a given site.
    ///
    /// This is the fast path for timeline loading - only fetches lightweight
    /// metadata, not the full scan data.
    #[cfg(target_arch = "wasm32")]
    pub async fn list_metadata_for_site(
        &self,
        site_id: &str,
    ) -> Result<Vec<ScanMetadata>, StorageError> {
        let all_keys = self.metadata_store.get_all_keys().await?;
        let prefix = format!("{}_", site_id);

        let mut metadata_list = Vec::new();
        for key in all_keys.iter().filter(|k| k.starts_with(&prefix)) {
            if let Some(metadata) = self.metadata_store.get::<ScanMetadata>(key).await? {
                metadata_list.push(metadata);
            }
        }

        // Sort by timestamp (ascending)
        metadata_list.sort_by_key(|m| m.key.timestamp);

        Ok(metadata_list)
    }

    /// Lists all cached scan keys for a given site.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)]
    pub async fn list_keys_for_site(&self, site_id: &str) -> Result<Vec<ScanKey>, StorageError> {
        let all_keys = self.scan_store.get_all_keys().await?;
        let prefix = format!("{}_", site_id);

        Ok(all_keys
            .iter()
            .filter(|k| k.starts_with(&prefix))
            .filter_map(|k| ScanKey::from_storage_key(k))
            .collect())
    }

    /// Deletes a cached scan and its metadata by key.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)]
    pub async fn delete(&self, key: &ScanKey) -> Result<(), StorageError> {
        let storage_key = key.to_storage_key();
        self.scan_store.delete(&storage_key).await?;
        self.metadata_store.delete(&storage_key).await?;
        Ok(())
    }

    /// Clears all cached scans and metadata.
    #[cfg(target_arch = "wasm32")]
    #[allow(dead_code)]
    pub async fn clear(&self) -> Result<(), StorageError> {
        self.scan_store.clear().await?;
        self.metadata_store.clear().await?;
        Ok(())
    }

    /// Migrates existing scans that don't have metadata entries.
    ///
    /// Call this on startup to backfill metadata for scans cached before
    /// the metadata store was added.
    #[cfg(target_arch = "wasm32")]
    pub async fn migrate_existing_scans(&self) -> Result<usize, StorageError> {
        use std::collections::HashSet;

        let scan_keys: HashSet<String> =
            self.scan_store.get_all_keys().await?.into_iter().collect();
        let metadata_keys: HashSet<String> = self
            .metadata_store
            .get_all_keys()
            .await?
            .into_iter()
            .collect();

        let missing_keys: Vec<_> = scan_keys.difference(&metadata_keys).collect();
        let count = missing_keys.len();

        for key in missing_keys {
            if let Some(scan) = self.scan_store.get::<CachedScan>(key).await? {
                let metadata = ScanMetadata::from_cached_scan(&scan);
                self.metadata_store.put(key, &metadata).await?;
                log::info!("Migrated metadata for scan: {}", key);
            }
        }

        if count > 0 {
            log::info!("Migrated {} scan(s) to metadata store", count);
        }

        Ok(count)
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
    pub async fn put_with_metadata(
        &self,
        _scan: &CachedScan,
        _end_timestamp: Option<i64>,
        _vcp: Option<u16>,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn list_metadata_for_site(
        &self,
        _site_id: &str,
    ) -> Result<Vec<ScanMetadata>, StorageError> {
        Ok(Vec::new())
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

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn migrate_existing_scans(&self) -> Result<usize, StorageError> {
        Ok(0)
    }
}

impl Default for NexradCache {
    fn default() -> Self {
        Self::new()
    }
}
