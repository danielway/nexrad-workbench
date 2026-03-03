//! IndexedDB storage for pre-computed radar sweep data.
//!
//! ## Object Stores
//!
//! 1. `sweeps` - Pre-computed sweep blobs (ArrayBuffer)
//!    - Key: "SITE|SCAN_MS|ELEV_NUM|PRODUCT"
//!
//! 2. `scan_index` - Per-scan metadata for timeline (JSON)
//!    - Key: "SITE|SCAN_START_MS"

use crate::data::keys::*;
use js_sys::{Array, ArrayBuffer, Uint8Array};
use serde::de::DeserializeOwned;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{IdbDatabase, IdbRequest, IdbTransaction, IdbTransactionMode};

/// Current database schema version.
const DATABASE_VERSION: u32 = 2;

/// Database name.
const DATABASE_NAME: &str = "nexrad-workbench";

/// Object store names.
const STORE_SWEEPS: &str = "sweeps";
const STORE_SCAN_INDEX: &str = "scan_index";

/// IndexedDB sweep store.
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

        let db = open_database().await?;
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
    // Sweep operations
    // ========================================================================

    /// Stores a pre-computed sweep blob.
    pub async fn put_sweep(&self, key: &str, data: &[u8]) -> Result<(), String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_SWEEPS, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SWEEPS)
            .map_err(|e| format!("Failed to get sweeps store: {:?}", e))?;

        let array = Uint8Array::from(data);
        let buffer = array.buffer();

        store
            .put_with_key(&buffer, &JsValue::from_str(key))
            .map_err(|e| format!("Failed to put sweep: {:?}", e))?;

        wait_for_transaction(&tx).await?;
        Ok(())
    }

    /// Stores multiple pre-computed sweep blobs in a single IDB transaction.
    ///
    /// Batches all writes into one readwrite transaction to avoid per-transaction
    /// disk-flush overhead. Critical: no await between puts — IDB transactions
    /// auto-commit when the event loop yields in WASM.
    pub async fn put_sweeps_batch(
        &self,
        items: &[(String, Vec<u8>)],
    ) -> Result<(), String> {
        if items.is_empty() {
            return Ok(());
        }
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_SWEEPS, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create batch transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SWEEPS)
            .map_err(|e| format!("Failed to get sweeps store: {:?}", e))?;

        // All puts synchronous — NO await between operations
        for (key, data) in items {
            let array = Uint8Array::from(data.as_slice());
            let buffer = array.buffer();
            store
                .put_with_key(&buffer, &JsValue::from_str(key))
                .map_err(|e| format!("Failed to put sweep '{}': {:?}", key, e))?;
        }

        // Single await — transaction commits atomically
        wait_for_transaction(&tx).await?;
        Ok(())
    }

    /// Gets a pre-computed sweep blob by key, returning the raw JS ArrayBuffer.
    /// Avoids the 5MB+ copy from JS to Rust that `get_sweep` performs.
    pub async fn get_sweep_as_js(
        &self,
        key: &str,
    ) -> Result<Option<ArrayBuffer>, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let tx = db
            .transaction_with_str_and_mode(STORE_SWEEPS, IdbTransactionMode::Readonly)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SWEEPS)
            .map_err(|e| format!("Failed to get sweeps store: {:?}", e))?;

        let request = store
            .get(&JsValue::from_str(key))
            .map_err(|e| format!("Failed to get sweep: {:?}", e))?;

        let result = wait_for_request(&request).await?;

        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }

        let buffer: ArrayBuffer = result
            .dyn_into()
            .map_err(|_| "Expected ArrayBuffer".to_string())?;
        Ok(Some(buffer))
    }

    // ========================================================================
    // Scan index operations
    // ========================================================================

    /// Writes or updates a scan index entry.
    pub async fn put_scan_index_entry(&self, entry: &ScanIndexEntry) -> Result<(), String> {
        self.ensure_open().await?;
        let db = self.get_db()?;
        let storage_key = entry.storage_key();

        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_INDEX, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get scan_index store: {:?}", e))?;

        let json =
            serde_json::to_string(entry).map_err(|e| format!("Serialization error: {}", e))?;
        let js = js_sys::JSON::parse(&json).map_err(|e| format!("JSON parse error: {:?}", e))?;

        store
            .put_with_key(&js, &JsValue::from_str(&storage_key))
            .map_err(|e| format!("Failed to put scan index: {:?}", e))?;

        wait_for_transaction(&tx).await?;
        Ok(())
    }

    /// Updates sweep metadata on an existing scan index entry.
    pub async fn update_scan_sweep_meta(
        &self,
        scan: &ScanKey,
        end_timestamp_secs: i64,
        sweeps: Vec<SweepMeta>,
    ) -> Result<bool, String> {
        self.ensure_open().await?;

        let existing = self.scan_availability(scan).await?;

        let Some(mut entry) = existing else {
            return Ok(false);
        };

        entry.end_timestamp_secs = Some(end_timestamp_secs);
        entry.sweeps = Some(sweeps);
        entry.updated_at = UnixMillis::now();

        self.put_scan_index_entry(&entry).await?;
        Ok(true)
    }

    /// Gets scan availability information.
    pub async fn scan_availability(
        &self,
        scan: &ScanKey,
    ) -> Result<Option<ScanIndexEntry>, String> {
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
                        if entry.scan.site.0 != site.0 {
                            continue;
                        }

                        let scan_time = entry.scan.scan_start;
                        if scan_time >= start && scan_time <= end {
                            let scan_end = UnixMillis(scan_time.0 + 5 * 60 * 1000);
                            ranges.push(TimeRange::new(scan_time, scan_end));
                        }
                    }
                }
            }
        }

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
                        if entry.scan.site.0 != site.0 {
                            continue;
                        }

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
    pub async fn set_expected_records(&self, scan: &ScanKey, expected: u32) -> Result<(), String> {
        self.ensure_open().await?;

        let existing = self.scan_availability(scan).await?;
        let mut entry = existing.unwrap_or_else(|| ScanIndexEntry::new(scan.clone()));

        entry.expected_records = Some(expected);
        entry.updated_at = UnixMillis::now();

        self.put_scan_index_entry(&entry).await?;
        Ok(())
    }

    /// Gets total cache size across all scans.
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

        let existing = self.scan_availability(scan).await?;

        let Some(mut entry) = existing else {
            return Ok(());
        };

        entry.last_accessed_at = UnixMillis::now();
        self.put_scan_index_entry(&entry).await?;
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

        scans.sort_by_key(|s| s.last_accessed_at.0);
        scans.truncate(limit as usize);
        Ok(scans)
    }

    /// Deletes a scan and all its sweep blobs.
    /// Returns the number of bytes freed.
    pub async fn delete_scan(&self, scan: &ScanKey) -> Result<u64, String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let scan_storage_key = scan.to_storage_key();

        // Get the scan entry to know its size and elevation structure
        let scan_entry = self.scan_availability(scan).await?;
        let bytes_freed = scan_entry.as_ref().map(|e| e.total_size_bytes).unwrap_or(0);

        // Build list of all possible sweep keys for this scan
        let sweep_keys: Vec<String> = if let Some(ref entry) = scan_entry {
            if let Some(ref sweeps) = entry.sweeps {
                let mut keys = Vec::new();
                for sweep in sweeps {
                    for product in ALL_PRODUCTS {
                        let key = SweepDataKey::new(
                            scan.clone(),
                            sweep.elevation_number,
                            *product,
                        );
                        keys.push(key.to_storage_key());
                    }
                }
                keys
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Delete all sweep blobs and scan index entry in one transaction
        let store_names = Array::new();
        store_names.push(&JsValue::from_str(STORE_SWEEPS));
        store_names.push(&JsValue::from_str(STORE_SCAN_INDEX));

        let tx = db
            .transaction_with_str_sequence_and_mode(&store_names, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        let sweeps_store = tx
            .object_store(STORE_SWEEPS)
            .map_err(|e| format!("Failed to get sweeps store: {:?}", e))?;

        let scan_store = tx
            .object_store(STORE_SCAN_INDEX)
            .map_err(|e| format!("Failed to get scan_index store: {:?}", e))?;

        for key in &sweep_keys {
            sweeps_store
                .delete(&JsValue::from_str(key))
                .map_err(|e| format!("Failed to delete sweep: {:?}", e))?;
        }

        scan_store
            .delete(&JsValue::from_str(&scan_storage_key))
            .map_err(|e| format!("Failed to delete scan index: {:?}", e))?;

        wait_for_transaction(&tx).await?;

        log::info!(
            "Deleted scan {} ({} sweep blobs, {} bytes freed)",
            scan,
            sweep_keys.len(),
            bytes_freed
        );
        Ok(bytes_freed)
    }

    /// Evicts scans until total cache size is below target_bytes.
    /// Returns the number of scans evicted.
    pub async fn evict_to_size(&self, target_bytes: u64) -> Result<u32, String> {
        let mut current_size = self.total_cache_size().await?;
        let mut evicted_count = 0u32;

        while current_size > target_bytes {
            let lru_scans = self.get_lru_scans(1).await?;

            if lru_scans.is_empty() {
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

    /// Clears all data from all stores.
    pub async fn clear_all(&self) -> Result<(), String> {
        self.ensure_open().await?;
        let db = self.get_db()?;

        let store_names = Array::new();
        store_names.push(&JsValue::from_str(STORE_SWEEPS));
        store_names.push(&JsValue::from_str(STORE_SCAN_INDEX));

        let tx = db
            .transaction_with_str_sequence_and_mode(&store_names, IdbTransactionMode::Readwrite)
            .map_err(|e| format!("Failed to create transaction: {:?}", e))?;

        for name in [STORE_SWEEPS, STORE_SCAN_INDEX] {
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

/// Gets the IdbFactory from the current global scope (works in both Window and Worker).
fn get_idb_factory() -> Result<web_sys::IdbFactory, String> {
    let global = js_sys::global();
    let idb = js_sys::Reflect::get(&global, &wasm_bindgen::JsValue::from_str("indexedDB"))
        .map_err(|e| format!("Failed to access indexedDB: {:?}", e))?;
    if idb.is_undefined() || idb.is_null() {
        return Err("IndexedDB not available in this context".to_string());
    }
    idb.dyn_into::<web_sys::IdbFactory>()
        .map_err(|_| "indexedDB is not an IdbFactory".to_string())
}

/// Opens the database, creating schema as needed.
async fn open_database() -> Result<IdbDatabase, String> {
    let idb_factory = get_idb_factory()?;

    let open_request = idb_factory
        .open_with_u32(DATABASE_NAME, DATABASE_VERSION)
        .map_err(|e| format!("Failed to open database: {:?}", e))?;

    // Set up upgrade handler
    let onupgradeneeded = Closure::wrap(Box::new(move |event: web_sys::IdbVersionChangeEvent| {
        let request: IdbRequest = event
            .target()
            .unwrap()
            .dyn_into()
            .expect("Expected IdbRequest");
        let db: IdbDatabase = request.result().unwrap().dyn_into().unwrap();

        // Delete old stores from v1 if they exist
        let store_names = db.object_store_names();
        for old_store in ["records", "record_index"] {
            if store_names.contains(old_store) {
                db.delete_object_store(old_store)
                    .expect("Failed to delete old object store");
                log::info!("Deleted old IndexedDB store: {}", old_store);
            }
        }

        // Create new stores if they don't exist
        for store_name in [STORE_SWEEPS, STORE_SCAN_INDEX] {
            if !store_names.contains(store_name) {
                db.create_object_store(store_name)
                    .expect("Failed to create object store");
                log::info!("Created IndexedDB store: {}", store_name);
            }
        }
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
