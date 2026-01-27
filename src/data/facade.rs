//! Data facade that coordinates cache, archive, and realtime sources.
//!
//! The facade provides a unified interface for accessing radar data,
//! transparently handling caching and source selection.
//!
//! ## Access Policies
//!
//! - `PreferCache`: Use cache if available, fallback to network
//! - `CacheThenNetwork`: Always check cache first, refresh from network if stale
//! - `NetworkOnly`: Bypass cache, always fetch from network
//! - `CacheOnly`: Only use cache, never fetch from network
//!
//! ## Data Flow
//!
//! Archive download:
//! 1. Check cache for existing records
//! 2. If missing and policy allows, download from AWS
//! 3. Split downloaded file into records
//! 4. Store records in cache with metadata
//! 5. Return assembled volume data
//!
//! Realtime streaming:
//! 1. Receive record from stream
//! 2. Compute record key (scan_start + record_id)
//! 3. Store record in cache
//! 4. Notify subscribers of new data

use crate::data::keys::*;
use crate::data::record_cache::*;
use ::nexrad::prelude::{load, Volume};
use std::cell::RefCell;
use std::rc::Rc;

/// Data access policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessPolicy {
    /// Use cache if available, fallback to network.
    #[default]
    PreferCache,
    /// Always check cache first, refresh from network if stale.
    CacheThenNetwork,
    /// Bypass cache, always fetch from network.
    NetworkOnly,
    /// Only use cache, never fetch from network.
    CacheOnly,
}

/// Result of attempting to decode available records.
#[derive(Debug)]
pub enum DecodeStatus {
    /// Successfully decoded a volume.
    Success(Volume),
    /// Not enough records to decode (have, estimated need).
    Incomplete {
        records_have: u32,
        estimated_need: Option<u32>,
    },
    /// Decode failed with error.
    Error(String),
}

/// Result of a data fetch operation.
#[derive(Debug, Clone)]
pub enum FetchResult {
    /// Data retrieved from cache.
    CacheHit {
        scan: ScanKey,
        data: Vec<u8>,
        completeness: ScanCompleteness,
    },
    /// Data downloaded from network and cached.
    Downloaded {
        scan: ScanKey,
        data: Vec<u8>,
        records_stored: u32,
    },
    /// Partial data available (some records missing).
    Partial {
        scan: ScanKey,
        data: Vec<u8>,
        present_records: u32,
        expected_records: Option<u32>,
    },
    /// Data not available.
    NotFound { scan: ScanKey },
    /// Error occurred.
    Error(String),
}

/// Event emitted when new data is available.
#[derive(Debug, Clone)]
pub enum DataEvent {
    /// New record stored in cache.
    RecordStored {
        key: RecordKey,
        size_bytes: u32,
        is_new_scan: bool,
    },
    /// Scan completeness changed.
    ScanUpdated {
        scan: ScanKey,
        completeness: ScanCompleteness,
        present_records: u32,
    },
    /// Timeline data changed (need to refresh UI).
    TimelineChanged { site: SiteId },
}

/// Callback for data events.
pub type EventCallback = Box<dyn Fn(DataEvent)>;

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
    pub fn policy(&self) -> AccessPolicy {
        *self.policy.borrow()
    }

    /// Sets the access policy.
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

    /// Gets scan data, using cache and/or network based on policy.
    ///
    /// Returns the assembled volume data (all records concatenated).
    pub async fn get_scan(&self, scan: &ScanKey) -> FetchResult {
        let policy = self.policy();

        // Check cache first (unless NetworkOnly)
        if policy != AccessPolicy::NetworkOnly {
            if let Ok(Some(entry)) = self.cache.scan_availability(scan).await {
                if entry.completeness() == ScanCompleteness::Complete {
                    // Fetch all records and assemble
                    match self.assemble_scan_from_cache(scan).await {
                        Ok(data) => {
                            return FetchResult::CacheHit {
                                scan: scan.clone(),
                                data,
                                completeness: entry.completeness(),
                            };
                        }
                        Err(e) => {
                            log::warn!("Failed to assemble scan from cache: {}", e);
                        }
                    }
                } else if policy == AccessPolicy::CacheOnly {
                    // Return partial data from cache
                    match self.assemble_scan_from_cache(scan).await {
                        Ok(data) => {
                            return FetchResult::Partial {
                                scan: scan.clone(),
                                data,
                                present_records: entry.present_records,
                                expected_records: entry.expected_records,
                            };
                        }
                        Err(e) => {
                            return FetchResult::Error(e);
                        }
                    }
                }
            } else if policy == AccessPolicy::CacheOnly {
                return FetchResult::NotFound { scan: scan.clone() };
            }
        }

        // If we get here and policy is CacheOnly, return not found
        if policy == AccessPolicy::CacheOnly {
            return FetchResult::NotFound { scan: scan.clone() };
        }

        // Network fetch would go here, but for now return not found
        // The actual network fetch is handled by the download channel
        FetchResult::NotFound { scan: scan.clone() }
    }

    /// Assembles a complete scan from cached records.
    async fn assemble_scan_from_cache(&self, scan: &ScanKey) -> CacheResult<Vec<u8>> {
        let record_keys = self.cache.list_records_for_scan(scan).await?;

        let mut records = Vec::with_capacity(record_keys.len());
        for key in record_keys {
            if let Some(record) = self.cache.get_record(&key).await? {
                records.push(record);
            }
        }

        // Sort by record_id and reassemble
        records.sort_by_key(|r| r.key.record_id);
        Ok(reassemble_records(&records))
    }

    /// Attempts to decode all available records for a scan.
    ///
    /// This method fetches all cached records, reassembles them, and attempts
    /// to decode the volume. It may succeed with partial data if enough sweeps
    /// are present.
    ///
    /// Returns `Ok(volume)` on successful decode, or `Err(DecodeStatus)` if
    /// incomplete or failed.
    pub async fn decode_available_records(&self, scan: &ScanKey) -> Result<Volume, DecodeStatus> {
        // List all available records for this scan
        let record_keys = match self.cache.list_records_for_scan(scan).await {
            Ok(keys) => keys,
            Err(e) => return Err(DecodeStatus::Error(e)),
        };

        if record_keys.is_empty() {
            return Err(DecodeStatus::Incomplete {
                records_have: 0,
                estimated_need: None,
            });
        }

        // Fetch all record blobs
        let mut records = Vec::with_capacity(record_keys.len());
        for key in &record_keys {
            match self.cache.get_record(key).await {
                Ok(Some(blob)) => records.push(blob),
                Ok(None) => {
                    log::warn!("Record {} listed but not found", key);
                }
                Err(e) => {
                    log::warn!("Failed to fetch record {}: {}", key, e);
                }
            }
        }

        if records.is_empty() {
            return Err(DecodeStatus::Incomplete {
                records_have: 0,
                estimated_need: None,
            });
        }

        // Sort by record_id and reassemble
        records.sort_by_key(|r| r.key.record_id);
        let data = reassemble_records(&records);

        // Attempt decode
        match load(&data) {
            Ok(volume) => {
                log::debug!(
                    "decode_available_records: success with {} records, {} sweeps",
                    records.len(),
                    volume.sweeps().len()
                );
                Ok(volume)
            }
            Err(e) => {
                log::debug!(
                    "decode_available_records: failed with {} records: {}",
                    records.len(),
                    e
                );
                Err(DecodeStatus::Incomplete {
                    records_have: records.len() as u32,
                    estimated_need: None,
                })
            }
        }
    }

    // ========================================================================
    // Record operations
    // ========================================================================

    /// Stores a record in the cache.
    ///
    /// This is called by the download channel after splitting an archive file,
    /// or by the realtime channel for each incoming chunk.
    pub async fn store_record(
        &self,
        record: &RecordBlob,
        meta: RecordIndexEntry,
    ) -> CacheResult<bool> {
        self.cache.put_record(record, meta).await
    }

    /// Stores multiple records in a batch (more efficient for archive downloads).
    pub async fn store_records_batch(
        &self,
        records: Vec<(RecordBlob, RecordIndexEntry)>,
    ) -> CacheResult<u32> {
        let mut stored = 0;
        for (record, meta) in records {
            if self.cache.put_record(&record, meta).await? {
                stored += 1;
            }
        }
        Ok(stored)
    }

    /// Gets a single record from cache.
    pub async fn get_record(&self, key: &RecordKey) -> CacheResult<Option<RecordBlob>> {
        self.cache.get_record(key).await
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

    /// Queries records by time range.
    pub async fn query_records_by_time(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
        limit: u32,
        include_bytes: bool,
    ) -> CacheResult<Vec<(RecordKey, Option<RecordBlob>)>> {
        self.cache
            .query_records_by_time(site, start, end, limit, include_bytes)
            .await
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

    /// Deletes a scan and all its records. Returns bytes freed.
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
    pub async fn check_and_evict(&self, quota_bytes: u64, target_bytes: u64) -> CacheResult<(bool, u32)> {
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

/// Helper to process an archive download and store records.
///
/// This function:
/// 1. Splits the archive file into individual records
/// 2. Computes record keys and metadata
/// 3. Stores each record in the cache
/// 4. Returns the scan key and number of records stored
pub async fn process_archive_download(
    facade: &DataFacade,
    site_id: &str,
    file_name: &str,
    timestamp_secs: i64,
    data: &[u8],
) -> CacheResult<(ScanKey, u32)> {
    let scan_start = UnixMillis::from_secs(timestamp_secs);
    let scan_key = ScanKey::new(site_id, scan_start);

    // Split into records
    let record_parts = split_archive2_into_records(data);

    if record_parts.is_empty() {
        return Err("No records found in archive file".to_string());
    }

    // Store each record
    let mut stored = 0;
    for (record_id, record_data) in record_parts {
        let record_key = RecordKey::new(scan_key.clone(), record_id);
        let record = RecordBlob::new(record_key.clone(), record_data.clone());

        // First record typically contains VCP metadata
        let has_vcp = record_id == 0;

        let meta = RecordIndexEntry::new(record_key, record_data.len() as u32)
            .with_vcp(has_vcp);

        if facade.store_record(&record, meta).await? {
            stored += 1;
        }
    }

    // Update scan index with file name
    // (This would require extending the cache API, for now we skip it)

    log::info!(
        "Processed archive {}: {} records stored for scan {}",
        file_name,
        stored,
        scan_key
    );

    Ok((scan_key, stored))
}

/// Helper to process a realtime chunk and store it.
///
/// This function:
/// 1. Computes the record key from scan start and chunk sequence
/// 2. Creates record metadata
/// 3. Stores the record in the cache
pub async fn process_realtime_chunk(
    facade: &DataFacade,
    site_id: &str,
    scan_start_secs: i64,
    chunk_seq: u32,
    data: &[u8],
    is_first_chunk: bool,
) -> CacheResult<RecordKey> {
    let scan_start = UnixMillis::from_secs(scan_start_secs);
    let scan_key = ScanKey::new(site_id, scan_start);
    let record_key = RecordKey::new(scan_key, chunk_seq);

    let record = RecordBlob::new(record_key.clone(), data.to_vec());
    let meta = RecordIndexEntry::new(record_key.clone(), data.len() as u32)
        .with_vcp(is_first_chunk)
        .with_time(UnixMillis::now());

    facade.store_record(&record, meta).await?;

    Ok(record_key)
}
