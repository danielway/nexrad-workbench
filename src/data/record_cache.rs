//! Record cache abstraction layer.
//!
//! Provides a high-level interface for storing and querying radar records.
//! The actual storage is implemented by `IndexedDbRecordStore` backed by IndexedDB.

use crate::data::keys::*;

use crate::data::indexeddb::IndexedDbRecordStore;

/// Result type for cache operations.
pub type CacheResult<T> = Result<T, String>;

/// Record cache interface.
pub trait RecordCache {
    /// Stores a record blob and updates indexes.
    ///
    /// Idempotent: if record already exists, does not overwrite blob.
    fn put_record(
        &self,
        record: &RecordBlob,
        meta: RecordIndexEntry,
    ) -> impl std::future::Future<Output = CacheResult<bool>> + Send;

    /// Gets a record blob by key.
    fn get_record(
        &self,
        key: &RecordKey,
    ) -> impl std::future::Future<Output = CacheResult<Option<RecordBlob>>> + Send;

    /// Checks if a record exists.
    fn has_record(
        &self,
        key: &RecordKey,
    ) -> impl std::future::Future<Output = CacheResult<bool>> + Send;

    /// Lists all record keys for a scan.
    fn list_records_for_scan(
        &self,
        scan: &ScanKey,
    ) -> impl std::future::Future<Output = CacheResult<Vec<RecordKey>>> + Send;

    /// Queries record keys by time range.
    fn query_record_keys_by_time(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
        limit: u32,
    ) -> impl std::future::Future<Output = CacheResult<Vec<RecordKey>>> + Send;

    /// Gets scan availability information.
    fn scan_availability(
        &self,
        scan: &ScanKey,
    ) -> impl std::future::Future<Output = CacheResult<Option<ScanIndexEntry>>> + Send;

    /// Gets availability ranges for a site within a time window.
    fn availability_ranges(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> impl std::future::Future<Output = CacheResult<Vec<TimeRange>>> + Send;
}

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

    /// Stores a record blob and updates indexes.
    pub async fn put_record(
        &self,
        record: &RecordBlob,
        meta: RecordIndexEntry,
    ) -> CacheResult<bool> {
        let outcome = self.store.put_record(record, meta).await?;
        Ok(outcome.inserted)
    }

    /// Gets a record blob by key.
    pub async fn get_record(&self, key: &RecordKey) -> CacheResult<Option<RecordBlob>> {
        self.store.get_record(key).await
    }

    /// Checks if a record exists.
    pub async fn has_record(&self, key: &RecordKey) -> CacheResult<bool> {
        self.store.has_record(key).await
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

    /// Lists all record keys for a scan.
    pub async fn list_records_for_scan(&self, scan: &ScanKey) -> CacheResult<Vec<RecordKey>> {
        self.store.list_records_for_scan(scan).await
    }

    /// Queries record keys by time range.
    pub async fn query_record_keys_by_time(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
        limit: u32,
    ) -> CacheResult<Vec<RecordKey>> {
        self.store
            .query_record_keys_by_time(site, start, end, limit)
            .await
    }

    /// Queries records by time range, optionally including blob data.
    pub async fn query_records_by_time(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
        limit: u32,
        include_bytes: bool,
    ) -> CacheResult<Vec<(RecordKey, Option<RecordBlob>)>> {
        self.store
            .query_records_by_time(site, start, end, limit, include_bytes)
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

    /// Deletes a scan and all its records. Returns bytes freed.
    pub async fn delete_scan(&self, scan: &ScanKey) -> CacheResult<u64> {
        self.store.delete_scan(scan).await
    }

    /// Evicts scans until total cache size is below target_bytes.
    /// Returns the number of scans evicted.
    pub async fn evict_to_size(&self, target_bytes: u64) -> CacheResult<u32> {
        self.store.evict_to_size(target_bytes).await
    }
}

/// Splits an Archive2 volume file into individual bzip2-compressed records.
///
/// Archive2 files consist of multiple bzip2-compressed blocks concatenated together.
/// Each block starts with the bzip2 magic bytes "BZh" followed by compression level.
///
/// Returns a vector of (record_id, record_bytes) tuples.
pub fn split_archive2_into_records(data: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut records = Vec::new();
    let mut pos = 0;
    let mut record_id = 0u32;

    // BZip2 magic: "BZ" followed by 'h' and compression level ('1'-'9')
    const BZIP2_MAGIC: &[u8] = b"BZh";

    while pos < data.len() {
        // Find next bzip2 block
        let start = pos;

        // Look for next bzip2 magic after current position
        let mut end = data.len();
        for i in (pos + 4)..data.len().saturating_sub(3) {
            if &data[i..i + 3] == BZIP2_MAGIC {
                // Verify it's a valid compression level
                if data.len() > i + 3 {
                    let level = data[i + 3];
                    if (b'1'..=b'9').contains(&level) {
                        end = i;
                        break;
                    }
                }
            }
        }

        if end > start {
            records.push((record_id, data[start..end].to_vec()));
            record_id += 1;
        }

        pos = end;
    }

    records
}

/// Reassembles records into a complete Archive2 volume.
///
/// Records must be in order by record_id.
pub fn reassemble_records(records: &[RecordBlob]) -> Vec<u8> {
    let total_size: usize = records.iter().map(|r| r.data.len()).sum();
    let mut data = Vec::with_capacity(total_size);

    for record in records {
        data.extend_from_slice(&record.data);
    }

    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_archive2_simple() {
        // Create mock data with two bzip2 blocks
        let mut data = Vec::new();
        // Block 1
        data.extend_from_slice(b"BZh9");
        data.extend_from_slice(&[0u8; 100]); // Fake compressed data
                                             // Block 2
        data.extend_from_slice(b"BZh9");
        data.extend_from_slice(&[1u8; 50]); // Fake compressed data

        let records = split_archive2_into_records(&data);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, 0);
        assert_eq!(records[1].0, 1);
        assert_eq!(records[0].1.len(), 104); // "BZh9" + 100 bytes
        assert_eq!(records[1].1.len(), 54); // "BZh9" + 50 bytes
    }

    #[test]
    fn test_reassemble_records() {
        let scan = ScanKey::new("KDMX", UnixMillis(1700000000000));
        let records = vec![
            RecordBlob::new(RecordKey::new(scan.clone(), 0), vec![1, 2, 3]),
            RecordBlob::new(RecordKey::new(scan.clone(), 1), vec![4, 5, 6]),
            RecordBlob::new(RecordKey::new(scan, 2), vec![7, 8, 9]),
        ];

        let data = reassemble_records(&records);
        assert_eq!(data, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}
