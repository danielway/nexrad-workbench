//! IndexedDB v4 schema implementation for record-based storage.
//!
//! ## Object Stores
//!
//! 1. `records` - Stores raw bzip2-compressed record blobs
//!    - Key: "SITE|SCAN_START_MS|RECORD_ID" (e.g., "KDMX|1700000000000|12")
//!    - Value: ArrayBuffer (raw bytes, NOT JSON)
//!
//! 2. `record_index` - Stores per-record metadata for fast queries
//!    - Key: Same as records
//!    - Value: RecordIndexEntry (JSON)
//!    - Indexes:
//!      - `by_scan`: [site, scan_start] for fetching all records in a scan
//!      - `by_time`: [site, effective_time] for time-range queries
//!
//! 3. `scan_index` - Stores per-scan metadata for timeline
//!    - Key: "SITE|SCAN_START_MS"
//!    - Value: ScanIndexEntry (JSON)
//!    - Indexes:
//!      - `by_site_time`: [site, scan_start] for timeline queries
//!
//! ## Migration from v3
//!
//! On upgrade from v3, existing stores are preserved:
//! - `nexrad-scans`: Lazily migrated when accessed
//! - `scan-metadata`: Used to populate scan_index on first access
//! - `file-cache`: Unchanged
//!
//! The `_legacy_migrated` store tracks which old entries have been migrated.

use crate::data::keys::*;
use js_sys::{Array, ArrayBuffer, Uint8Array};
use serde::de::DeserializeOwned;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{IdbDatabase, IdbRequest, IdbTransaction, IdbTransactionMode};

/// Current database schema version.
pub const DATABASE_VERSION: u32 = 4;

/// Database name.
pub const DATABASE_NAME: &str = "nexrad-workbench";

/// Object store names.
pub const STORE_RECORDS: &str = "records_v4";
pub const STORE_RECORD_INDEX: &str = "record_index_v4";
pub const STORE_SCAN_INDEX: &str = "scan_index_v4";

// Legacy stores (v3)
pub const LEGACY_STORE_SCANS: &str = "nexrad-scans";
pub const LEGACY_STORE_METADATA: &str = "scan-metadata";

/// Result of a put operation.
#[derive(Debug, Clone)]
pub struct PutOutcome {
    /// Whether the record blob was inserted (false if already existed).
    pub inserted: bool,
    /// Whether the scan index was updated.
    pub updated_scan_index: bool,
}

/// IndexedDB v4 record store.
#[derive(Clone)]
pub struct IndexedDbRecordStore {
    db: Rc<RefCell<Option<IdbDatabase>>>,
}

impl Default for IndexedDbRecordStore {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexedDbRecordStore {
    pub fn new() -> Self {
        Self {
            db: Rc::new(RefCell::new(None)),
        }
    }

    /// Opens the database, creating/upgrading schema as needed.
    pub async fn open(&self) -> Result<(), String> {
        if self.db.borrow().is_some() {
            return Ok(());
        }

        let db = open_database_v4().await?;
        *self.db.borrow_mut() = Some(db);
        Ok(())
    }

    /// Ensures the database is open.
    async fn ensure_open(&self) -> Result<(), String> {
        if self.db.borrow().is_none() {
            self.open().await?;
        }
        Ok(())
    }

    /// Gets the database reference.
    fn get_db(&self) -> Result<IdbDatabase, String> {
        self.db
            .borrow()
            .clone()
            .ok_or_else(|| "Database not open".to_string())
    }

    // ========================================================================
    // Record operations
    // ========================================================================

    /// Stores a record blob and updates indexes.
    ///
    /// Idempotent: if record already exists, does not overwrite blob.
    pub async fn put_record(
        &self,
        record: &RecordBlob,
        meta: RecordIndexEntry,
    ) -> Result<PutOutcome, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let record_key = record.key.to_storage_key();
        let scan_key = record.key.scan.to_storage_key();

        // Check if record already exists
        let exists = self.has_record(&record.key).await?;

        // Start transaction with all stores we need
        let store_names = Array::new();
        store_names.push(&JsValue::from_str(STORE_RECORDS));
        store_names.push(&JsValue::from_str(STORE_RECORD_INDEX));
        store_names.push(&JsValue::from_str(STORE_SCAN_INDEX));

        let tx = db
            .transaction_with_str_sequence_and_mode(&store_names, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        // Write record blob if not exists
        if !exists {
            let records_store = tx
                .object_store(STORE_RECORDS)
                .map_err(|e| format!("Failed to get records store: {:?}", e))?;

            // Store as ArrayBuffer directly
            let array = Uint8Array::from(record.data.as_slice());
            let buffer = array.buffer();

            records_store
                .put_with_key(&buffer, &JsValue::from_str(&record_key))
                .map_err(|e| format!("Failed to put record: {:?}", e))?;
        }

        // Write record index entry
        let index_store = tx
            .object_store(STORE_RECORD_INDEX)
            .map_err(|e| format!("Failed to get record_index store: {:?}", e))?;

        let meta_json =
            serde_json::to_string(&meta).map_err(|e| format!("Serialization error: {}", e))?;
        let meta_js = js_sys::JSON::parse(&meta_json)
            .map_err(|e| format!("JSON parse error: {:?}", e))?;

        index_store
            .put_with_key(&meta_js, &JsValue::from_str(&record_key))
            .map_err(|e| format!("Failed to put record index: {:?}", e))?;

        // Update scan index
        let scan_store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get scan_index store: {:?}", e))?;

        // Get existing scan entry or create new one
        // We do a synchronous get within the transaction
        let get_req = scan_store
            .get(&JsValue::from_str(&scan_key))
            .map_err(|e| format!("Failed to get scan: {:?}", e))?;
        let existing_result = wait_for_request(&get_req).await?;
        let existing_scan: Option<ScanIndexEntry> = deserialize_js_value(&existing_result);

        let mut scan_entry = existing_scan.unwrap_or_else(|| ScanIndexEntry::new(record.key.scan.clone()));

        // Update scan entry
        if !exists {
            scan_entry.present_records += 1;
            scan_entry.total_size_bytes += record.data.len() as u64;
        }
        if meta.has_vcp {
            scan_entry.has_vcp = true;
        }
        scan_entry.updated_at = UnixMillis::now();

        let scan_json = serde_json::to_string(&scan_entry)
            .map_err(|e| format!("Serialization error: {}", e))?;
        let scan_js =
            js_sys::JSON::parse(&scan_json).map_err(|e| format!("JSON parse error: {:?}", e))?;

        scan_store
            .put_with_key(&scan_js, &JsValue::from_str(&scan_key))
            .map_err(|e| format!("Failed to put scan index: {:?}", e))?;

        // Wait for transaction to complete
        wait_for_transaction(&tx).await?;

        Ok(PutOutcome {
            inserted: !exists,
            updated_scan_index: true,
        })
    }

    /// Updates sweep metadata on an existing scan index entry.
    ///
    /// Called after a volume is decoded to persist the actual end timestamp
    /// and per-sweep timing so subsequent timeline loads don't need to decode.
    pub async fn update_scan_sweep_meta(
        &self,
        scan: &ScanKey,
        end_timestamp_secs: i64,
        sweeps: Vec<SweepMeta>,
    ) -> Result<bool, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let scan_key = scan.to_storage_key();

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get scan_index store: {:?}", e))?;

        let get_req = store
            .get(&JsValue::from_str(&scan_key))
            .map_err(|e| format!("Failed to get scan: {:?}", e))?;
        let existing_result = wait_for_request(&get_req).await?;
        let existing: Option<ScanIndexEntry> = deserialize_js_value(&existing_result);

        let Some(mut entry) = existing else {
            return Ok(false);
        };

        entry.end_timestamp_secs = Some(end_timestamp_secs);
        entry.sweeps = Some(sweeps);
        entry.updated_at = UnixMillis::now();

        let json = serde_json::to_string(&entry)
            .map_err(|e| format!("Serialization error: {}", e))?;
        let js = js_sys::JSON::parse(&json)
            .map_err(|e| format!("JSON parse error: {:?}", e))?;

        store
            .put_with_key(&js, &JsValue::from_str(&scan_key))
            .map_err(|e| format!("Failed to put scan index: {:?}", e))?;

        wait_for_transaction(&tx).await?;
        Ok(true)
    }

    /// Gets a record blob by key.
    pub async fn get_record(&self, key: &RecordKey) -> Result<Option<RecordBlob>, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let storage_key = key.to_storage_key();

        let tx = db
            .transaction_with_str_and_mode(STORE_RECORDS, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_RECORDS)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let request = store
            .get(&JsValue::from_str(&storage_key))
            .map_err(|e| format!("Failed to get record: {:?}", e))?;

        let result = wait_for_request(&request).await?;

        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }

        // Result is an ArrayBuffer
        let buffer: ArrayBuffer = result
            .dyn_into()
            .map_err(|_| "Expected ArrayBuffer".to_string())?;
        let array = Uint8Array::new(&buffer);
        let data = array.to_vec();

        // Touch the scan for LRU tracking (fire and forget)
        let _ = self.touch_scan(&key.scan).await;

        Ok(Some(RecordBlob::new(key.clone(), data)))
    }

    /// Checks if a record exists.
    pub async fn has_record(&self, key: &RecordKey) -> Result<bool, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let storage_key = key.to_storage_key();

        let tx = db
            .transaction_with_str_and_mode(STORE_RECORD_INDEX, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_RECORD_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let request = store
            .count_with_key(&JsValue::from_str(&storage_key))
            .map_err(|e| format!("Failed to count: {:?}", e))?;

        let result = wait_for_request(&request).await?;
        let count = result.as_f64().unwrap_or(0.0) as u32;

        Ok(count > 0)
    }

    /// Lists all record keys for a scan.
    pub async fn list_records_for_scan(&self, scan: &ScanKey) -> Result<Vec<RecordKey>, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_RECORD_INDEX, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_RECORD_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        // Use key prefix to find all records for this scan
        let prefix = format!("{}|{}|", scan.site.0, scan.scan_start.0);

        let request = store
            .get_all_keys()
            .map_err(|e| format!("Failed to get keys: {:?}", e))?;

        let result = wait_for_request(&request).await?;
        let array = Array::from(&result);

        let mut keys = Vec::new();
        for i in 0..array.length() {
            if let Some(key_str) = array.get(i).as_string() {
                if key_str.starts_with(&prefix) {
                    if let Some(key) = RecordKey::from_storage_key(&key_str) {
                        keys.push(key);
                    }
                }
            }
        }

        keys.sort_by_key(|k| k.record_id);

        // Touch the scan for LRU tracking if we found records
        if !keys.is_empty() {
            let _ = self.touch_scan(scan).await;
        }

        Ok(keys)
    }

    // ========================================================================
    // Time-based queries
    // ========================================================================

    /// Queries record keys by time range.
    pub async fn query_record_keys_by_time(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
        limit: u32,
    ) -> Result<Vec<RecordKey>, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_RECORD_INDEX, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_RECORD_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        // Get all keys and filter by site and time
        // Note: In production, we'd use an index, but for simplicity we filter in Rust
        let request = store
            .get_all()
            .map_err(|e| format!("Failed to get all: {:?}", e))?;

        let result = wait_for_request(&request).await?;
        let array = Array::from(&result);

        let mut keys = Vec::new();
        for i in 0..array.length() {
            if keys.len() >= limit as usize {
                break;
            }

            let value = array.get(i);
            if let Ok(json_str) = js_sys::JSON::stringify(&value) {
                if let Some(s) = json_str.as_string() {
                    if let Ok(entry) = serde_json::from_str::<RecordIndexEntry>(&s) {
                        // Filter by site
                        if entry.key.scan.site.0 != site.0 {
                            continue;
                        }

                        // Filter by time
                        let time = entry.effective_time();
                        if time >= start && time <= end {
                            keys.push(entry.key);
                        }
                    }
                }
            }
        }

        keys.sort_by_key(|k| (k.scan.scan_start.0, k.record_id));
        Ok(keys)
    }

    /// Queries records by time range, optionally including blob data.
    pub async fn query_records_by_time(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
        limit: u32,
        include_bytes: bool,
    ) -> Result<Vec<(RecordKey, Option<RecordBlob>)>, String> {
        let keys = self.query_record_keys_by_time(site, start, end, limit).await?;

        if !include_bytes {
            return Ok(keys.into_iter().map(|k| (k, None)).collect());
        }

        let mut results = Vec::with_capacity(keys.len());
        for key in keys {
            let blob = self.get_record(&key).await?;
            results.push((key, blob));
        }

        Ok(results)
    }

    // ========================================================================
    // Scan index operations
    // ========================================================================

    /// Gets scan availability information.
    pub async fn scan_availability(&self, scan: &ScanKey) -> Result<Option<ScanIndexEntry>, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let storage_key = scan.to_storage_key();

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let request = store
            .get(&JsValue::from_str(&storage_key))
            .map_err(|e| format!("Failed to get: {:?}", e))?;

        let result = wait_for_request(&request).await?;
        Ok(deserialize_js_value(&result))
    }

    /// Gets availability ranges for a site within a time window.
    pub async fn availability_ranges(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> Result<Vec<TimeRange>, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let request = store
            .get_all()
            .map_err(|e| format!("Failed to get all: {:?}", e))?;

        let result = wait_for_request(&request).await?;
        let array = Array::from(&result);

        let mut ranges = Vec::new();
        for i in 0..array.length() {
            let value = array.get(i);
            if let Ok(json_str) = js_sys::JSON::stringify(&value) {
                if let Some(s) = json_str.as_string() {
                    if let Ok(entry) = serde_json::from_str::<ScanIndexEntry>(&s) {
                        // Filter by site
                        if entry.scan.site.0 != site.0 {
                            continue;
                        }

                        // Filter by time
                        let scan_time = entry.scan.scan_start;
                        if scan_time >= start && scan_time <= end {
                            // Estimate scan duration as 5 minutes
                            let scan_end = UnixMillis(scan_time.0 + 5 * 60 * 1000);
                            ranges.push(TimeRange::new(scan_time, scan_end));
                        }
                    }
                }
            }
        }

        // Merge adjacent ranges with 15-minute gap threshold
        Ok(merge_time_ranges(ranges, 15 * 60 * 1000))
    }

    /// Lists all scans for a site within a time window.
    pub async fn list_scans(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> Result<Vec<ScanIndexEntry>, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let request = store
            .get_all()
            .map_err(|e| format!("Failed to get all: {:?}", e))?;

        let result = wait_for_request(&request).await?;
        let array = Array::from(&result);

        let mut scans = Vec::new();
        for i in 0..array.length() {
            let value = array.get(i);
            if let Ok(json_str) = js_sys::JSON::stringify(&value) {
                if let Some(s) = json_str.as_string() {
                    if let Ok(entry) = serde_json::from_str::<ScanIndexEntry>(&s) {
                        // Filter by site
                        if entry.scan.site.0 != site.0 {
                            continue;
                        }

                        // Filter by time
                        let scan_time = entry.scan.scan_start;
                        if scan_time >= start && scan_time <= end {
                            scans.push(entry);
                        }
                    }
                }
            }
        }

        scans.sort_by_key(|s| s.scan.scan_start.0);
        Ok(scans)
    }

    /// Updates scan index with expected record count (from VCP).
    pub async fn set_expected_records(
        &self,
        scan: &ScanKey,
        expected: u32,
    ) -> Result<(), String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let storage_key = scan.to_storage_key();

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let get_req = store
            .get(&JsValue::from_str(&storage_key))
            .map_err(|e| format!("Failed to get: {:?}", e))?;
        let existing_result = wait_for_request(&get_req).await?;
        let existing: Option<ScanIndexEntry> = deserialize_js_value(&existing_result);
        let mut entry = existing.unwrap_or_else(|| ScanIndexEntry::new(scan.clone()));

        entry.expected_records = Some(expected);
        entry.updated_at = UnixMillis::now();

        let json = serde_json::to_string(&entry)
            .map_err(|e| format!("Serialization error: {}", e))?;
        let js = js_sys::JSON::parse(&json)
            .map_err(|e| format!("JSON parse error: {:?}", e))?;

        store
            .put_with_key(&js, &JsValue::from_str(&storage_key))
            .map_err(|e| format!("Failed to put: {:?}", e))?;

        wait_for_transaction(&tx).await?;
        Ok(())
    }

    /// Gets total cache size across all records.
    pub async fn total_cache_size(&self) -> Result<u64, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let request = store
            .get_all()
            .map_err(|e| format!("Failed to get all: {:?}", e))?;

        let result = wait_for_request(&request).await?;
        let array = Array::from(&result);

        let mut total: u64 = 0;
        for i in 0..array.length() {
            let value = array.get(i);
            if let Ok(json_str) = js_sys::JSON::stringify(&value) {
                if let Some(s) = json_str.as_string() {
                    if let Ok(entry) = serde_json::from_str::<ScanIndexEntry>(&s) {
                        total += entry.total_size_bytes;
                    }
                }
            }
        }

        Ok(total)
    }

    /// Updates the last_accessed_at timestamp for a scan (LRU tracking).
    pub async fn touch_scan(&self, scan: &ScanKey) -> Result<(), String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let storage_key = scan.to_storage_key();

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let get_req = store
            .get(&JsValue::from_str(&storage_key))
            .map_err(|e| format!("Failed to get: {:?}", e))?;
        let existing_result = wait_for_request(&get_req).await?;

        if let Some(mut entry) = deserialize_js_value::<ScanIndexEntry>(&existing_result) {
            entry.last_accessed_at = UnixMillis::now();

            let json = serde_json::to_string(&entry)
                .map_err(|e| format!("Serialization error: {}", e))?;
            let js = js_sys::JSON::parse(&json)
                .map_err(|e| format!("JSON parse error: {:?}", e))?;

            store
                .put_with_key(&js, &JsValue::from_str(&storage_key))
                .map_err(|e| format!("Failed to put: {:?}", e))?;

            wait_for_transaction(&tx).await?;
        }

        Ok(())
    }

    /// Gets scans sorted by last_accessed_at (oldest first) for LRU eviction.
    pub async fn get_lru_scans(&self, limit: u32) -> Result<Vec<ScanIndexEntry>, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get store: {:?}", e))?;

        let request = store
            .get_all()
            .map_err(|e| format!("Failed to get all: {:?}", e))?;

        let result = wait_for_request(&request).await?;
        let array = Array::from(&result);

        let mut scans: Vec<ScanIndexEntry> = Vec::new();
        for i in 0..array.length() {
            let value = array.get(i);
            if let Ok(json_str) = js_sys::JSON::stringify(&value) {
                if let Some(s) = json_str.as_string() {
                    if let Ok(entry) = serde_json::from_str::<ScanIndexEntry>(&s) {
                        scans.push(entry);
                    }
                }
            }
        }

        // Sort by last_accessed_at ascending (oldest first)
        scans.sort_by_key(|s| s.last_accessed_at.0);

        // Return only the requested limit
        scans.truncate(limit as usize);
        Ok(scans)
    }

    /// Deletes a scan and all its records.
    /// Returns the number of bytes freed.
    pub async fn delete_scan(&self, scan: &ScanKey) -> Result<u64, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let scan_storage_key = scan.to_storage_key();

        // First, get the scan entry to know its size
        let scan_entry = self.scan_availability(scan).await?;
        let bytes_freed = scan_entry.as_ref().map(|e| e.total_size_bytes).unwrap_or(0);

        // Get all record keys for this scan
        let record_keys = self.list_records_for_scan(scan).await?;

        // Delete all records and indexes in a transaction
        let store_names = Array::new();
        store_names.push(&JsValue::from_str(STORE_RECORDS));
        store_names.push(&JsValue::from_str(STORE_RECORD_INDEX));
        store_names.push(&JsValue::from_str(STORE_SCAN_INDEX));

        let tx = db
            .transaction_with_str_sequence_and_mode(&store_names, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let records_store = tx
            .object_store(STORE_RECORDS)
            .map_err(|e| format!("Failed to get records store: {:?}", e))?;

        let index_store = tx
            .object_store(STORE_RECORD_INDEX)
            .map_err(|e| format!("Failed to get record_index store: {:?}", e))?;

        let scan_store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get scan_index store: {:?}", e))?;

        // Delete all records and record index entries
        for key in record_keys {
            let record_storage_key = key.to_storage_key();
            records_store
                .delete(&JsValue::from_str(&record_storage_key))
                .map_err(|e| format!("Failed to delete record: {:?}", e))?;
            index_store
                .delete(&JsValue::from_str(&record_storage_key))
                .map_err(|e| format!("Failed to delete record index: {:?}", e))?;
        }

        // Delete scan index entry
        scan_store
            .delete(&JsValue::from_str(&scan_storage_key))
            .map_err(|e| format!("Failed to delete scan index: {:?}", e))?;

        wait_for_transaction(&tx).await?;

        log::info!("Deleted scan {} ({} bytes freed)", scan, bytes_freed);
        Ok(bytes_freed)
    }

    /// Evicts scans until total cache size is below target_bytes.
    /// Returns the number of scans evicted.
    pub async fn evict_to_size(&self, target_bytes: u64) -> Result<u32, String> {
        let mut current_size = self.total_cache_size().await?;
        let mut evicted_count = 0u32;

        while current_size > target_bytes {
            // Get the oldest scan
            let lru_scans = self.get_lru_scans(1).await?;

            if lru_scans.is_empty() {
                // No more scans to evict
                break;
            }

            let oldest = &lru_scans[0];
            let bytes_freed = self.delete_scan(&oldest.scan).await?;

            current_size = current_size.saturating_sub(bytes_freed);
            evicted_count += 1;

            log::info!(
                "Evicted scan {} (freed {} bytes, {} remaining)",
                oldest.scan,
                bytes_freed,
                current_size
            );
        }

        if evicted_count > 0 {
            log::info!(
                "LRU eviction complete: evicted {} scans, cache now {} bytes",
                evicted_count,
                current_size
            );
        }

        Ok(evicted_count)
    }

    /// Clears all data from v4 stores.
    pub async fn clear_all(&self) -> Result<(), String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let store_names = Array::new();
        store_names.push(&JsValue::from_str(STORE_RECORDS));
        store_names.push(&JsValue::from_str(STORE_RECORD_INDEX));
        store_names.push(&JsValue::from_str(STORE_SCAN_INDEX));

        let tx = db
            .transaction_with_str_sequence_and_mode(&store_names, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        for name in [STORE_RECORDS, STORE_RECORD_INDEX, STORE_SCAN_INDEX] {
            let store = tx
                .object_store(name)
                .map_err(|e| format!("Failed to get store: {:?}", e))?;
            store
                .clear()
                .map_err(|e| format!("Failed to clear store: {:?}", e))?;
        }

        wait_for_transaction(&tx).await?;
        Ok(())
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Opens the v4 database, creating/upgrading schema as needed.
async fn open_database_v4() -> Result<IdbDatabase, String> {
    let window =
        web_sys::window().ok_or_else(|| "No window object".to_string())?;

    let idb_factory = window
        .indexed_db()
        .map_err(|e| format!("IndexedDB error: {:?}", e))?
        .ok_or_else(|| "IndexedDB not available".to_string())?;

    let open_request = idb_factory
        .open_with_u32(DATABASE_NAME, DATABASE_VERSION)
        .map_err(|e| format!("Failed to open database: {:?}", e))?;

    // Set up upgrade handler
    let onupgradeneeded = Closure::wrap(Box::new(move |event: web_sys::IdbVersionChangeEvent| {
        let old_version = event.old_version() as u32;
        let request: IdbRequest = event
            .target()
            .unwrap()
            .dyn_into()
            .expect("Expected IdbRequest");
        let db: IdbDatabase = request.result().unwrap().dyn_into().unwrap();

        log::info!(
            "Upgrading IndexedDB from v{} to v{}",
            old_version,
            DATABASE_VERSION
        );

        // Create v4 stores if they don't exist
        if !db.object_store_names().contains(STORE_RECORDS) {
            db.create_object_store(STORE_RECORDS)
                .expect("Failed to create records store");
            log::info!("Created {} store", STORE_RECORDS);
        }

        if !db.object_store_names().contains(STORE_RECORD_INDEX) {
            db.create_object_store(STORE_RECORD_INDEX)
                .expect("Failed to create record_index store");
            log::info!("Created {} store", STORE_RECORD_INDEX);
        }

        if !db.object_store_names().contains(STORE_SCAN_INDEX) {
            db.create_object_store(STORE_SCAN_INDEX)
                .expect("Failed to create scan_index store");
            log::info!("Created {} store", STORE_SCAN_INDEX);
        }

        // Keep legacy stores for migration (don't delete them)
        // They will be lazily migrated when accessed
    }) as Box<dyn FnMut(_)>);

    open_request.set_onupgradeneeded(Some(onupgradeneeded.as_ref().unchecked_ref()));
    onupgradeneeded.forget();

    // Wait for database to open
    let db_result = wait_for_request(&open_request).await?;
    let db: IdbDatabase = db_result
        .dyn_into()
        .map_err(|_| "Failed to cast to IdbDatabase".to_string())?;

    log::info!("Opened IndexedDB {} v{}", DATABASE_NAME, DATABASE_VERSION);

    Ok(db)
}

/// Waits for an IDB request to complete.
async fn wait_for_request(request: &IdbRequest) -> Result<JsValue, String> {
    let (tx, rx) = futures_channel::oneshot::channel::<Result<JsValue, String>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let tx_success = tx.clone();
    let onsuccess = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let request: IdbRequest = event
            .target()
            .unwrap()
            .dyn_into()
            .expect("Expected IdbRequest");
        let result = request.result().unwrap_or(JsValue::UNDEFINED);
        if let Some(tx) = tx_success.borrow_mut().take() {
            let _ = tx.send(Ok(result));
        }
    }) as Box<dyn FnMut(_)>);

    let tx_error = tx;
    let onerror = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let request: IdbRequest = event
            .target()
            .unwrap()
            .dyn_into()
            .expect("Expected IdbRequest");
        let error_msg = request
            .error()
            .ok()
            .flatten()
            .map(|e| e.message())
            .unwrap_or_else(|| "Unknown error".to_string());
        if let Some(tx) = tx_error.borrow_mut().take() {
            let _ = tx.send(Err(error_msg));
        }
    }) as Box<dyn FnMut(_)>);

    request.set_onsuccess(Some(onsuccess.as_ref().unchecked_ref()));
    request.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let result = rx.await.map_err(|_| "Channel closed".to_string())?;

    request.set_onsuccess(None);
    request.set_onerror(None);

    drop(onsuccess);
    drop(onerror);

    result
}

/// Waits for an IDB transaction to complete.
async fn wait_for_transaction(tx: &IdbTransaction) -> Result<(), String> {
    let (sender, rx) = futures_channel::oneshot::channel::<Result<(), String>>();
    let sender = Rc::new(RefCell::new(Some(sender)));

    let tx_complete = sender.clone();
    let oncomplete = Closure::wrap(Box::new(move |_: web_sys::Event| {
        if let Some(tx) = tx_complete.borrow_mut().take() {
            let _ = tx.send(Ok(()));
        }
    }) as Box<dyn FnMut(_)>);

    let tx_error = sender;
    let onerror = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        // Getting the specific error is complex with web-sys, use a generic message
        let error_msg = "Transaction error".to_string();
        if let Some(tx) = tx_error.borrow_mut().take() {
            let _ = tx.send(Err(error_msg));
        }
    }) as Box<dyn FnMut(_)>);

    tx.set_oncomplete(Some(oncomplete.as_ref().unchecked_ref()));
    tx.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let result = rx.await.map_err(|_| "Channel closed".to_string())?;

    tx.set_oncomplete(None);
    tx.set_onerror(None);

    drop(oncomplete);
    drop(onerror);

    result
}

/// Deserializes a JsValue to a Rust type via JSON.
fn deserialize_js_value<T: DeserializeOwned>(value: &JsValue) -> Option<T> {
    if value.is_undefined() || value.is_null() {
        return None;
    }
    let json_str = js_sys::JSON::stringify(value).ok()?;
    let s = json_str.as_string()?;
    serde_json::from_str(&s).ok()
}
