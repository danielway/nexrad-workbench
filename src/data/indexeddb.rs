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
use std::marker::PhantomData;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{IdbDatabase, IdbObjectStore, IdbRequest, IdbTransaction, IdbTransactionMode};

/// Structured error type for IndexedDB operations.
#[derive(Debug)]
#[allow(dead_code)]
pub enum DataError {
    /// The database has not been opened yet.
    NotOpen,
    /// An IDB transaction failed.
    TransactionFailed(String),
    /// An IDB request failed.
    RequestFailed(String),
    /// Browser storage quota exceeded.
    QuotaExceeded { available_mb: f64, required_mb: f64 },
    /// The requested key was not found.
    NotFound,
    /// Deserialization of stored data failed.
    DeserializationError(String),
}

impl std::fmt::Display for DataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataError::NotOpen => write!(f, "Database not open"),
            DataError::TransactionFailed(msg) => write!(f, "Transaction failed: {}", msg),
            DataError::RequestFailed(msg) => write!(f, "Request failed: {}", msg),
            DataError::QuotaExceeded {
                available_mb,
                required_mb,
            } => write!(
                f,
                "Insufficient storage quota: {:.1} MB available, {:.1} MB required",
                available_mb, required_mb
            ),
            DataError::NotFound => write!(f, "Not found"),
            DataError::DeserializationError(msg) => write!(f, "Deserialization error: {}", msg),
        }
    }
}

/// Browser storage quota estimate from `navigator.storage.estimate()`.
#[derive(Debug, Clone, Copy)]
pub struct StorageQuotaEstimate {
    /// Total quota granted by the browser (bytes).
    pub quota: u64,
    /// Current usage across all storage mechanisms (bytes).
    pub usage: u64,
}

impl StorageQuotaEstimate {
    /// Remaining bytes available.
    pub fn remaining(&self) -> u64 {
        self.quota.saturating_sub(self.usage)
    }
}

/// Current database schema version.
const DATABASE_VERSION: u32 = 3;

/// Database name.
const DATABASE_NAME: &str = "nexrad-workbench";

/// Object store names.
const STORE_SWEEPS: &str = "sweeps";
const STORE_SCAN_INDEX: &str = "scan_index";

/// Open-state machine that coalesces concurrent `open()` calls into a single
/// underlying `indexedDB.open(...)`. Without this, multiple `spawn_local`
/// tasks racing on a fresh store would each run their own open (each logging
/// "Opened IndexedDB …") because the database handle is only stored after
/// the initial `.await` resumes.
enum OpenState {
    Closed,
    Opening(Vec<futures_channel::oneshot::Sender<Result<(), String>>>),
    Open(IdbDatabase),
}

/// IndexedDB sweep store.
#[derive(Clone)]
pub struct IndexedDbRecordStore {
    state: Rc<RefCell<OpenState>>,
}

impl Default for IndexedDbRecordStore {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexedDbRecordStore {
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(OpenState::Closed)),
        }
    }

    /// Opens the database, creating/upgrading schema as needed.
    ///
    /// Safe to call concurrently: the first caller drives `open_database()`,
    /// and any callers that arrive while it is in flight await the same
    /// completion via a oneshot channel rather than starting their own open.
    pub async fn open(&self) -> Result<(), DataError> {
        enum Action {
            AlreadyOpen,
            Wait(futures_channel::oneshot::Receiver<Result<(), String>>),
            Drive,
        }

        let action = {
            let mut state = self.state.borrow_mut();
            match &mut *state {
                OpenState::Open(_) => Action::AlreadyOpen,
                OpenState::Opening(waiters) => {
                    let (tx, rx) = futures_channel::oneshot::channel();
                    waiters.push(tx);
                    Action::Wait(rx)
                }
                OpenState::Closed => {
                    *state = OpenState::Opening(Vec::new());
                    Action::Drive
                }
            }
        };

        match action {
            Action::AlreadyOpen => Ok(()),
            Action::Wait(rx) => rx
                .await
                .map_err(|_| DataError::TransactionFailed("open canceled".to_string()))
                .and_then(|r| r.map_err(DataError::TransactionFailed)),
            Action::Drive => {
                let result = open_database().await;
                // Concurrent callers may have pushed into the Opening vec while
                // we were awaiting; take them here and notify.
                let waiters = {
                    let mut state = self.state.borrow_mut();
                    let next = match &result {
                        Ok(db) => OpenState::Open(db.clone()),
                        // Stay Closed so a later call can retry.
                        Err(_) => OpenState::Closed,
                    };
                    match std::mem::replace(&mut *state, next) {
                        OpenState::Opening(waiters) => waiters,
                        // The Drive caller set state to Opening and nothing
                        // else transitions out of it.
                        _ => unreachable!(),
                    }
                };

                let notification: Result<(), String> =
                    result.as_ref().map(|_| ()).map_err(|e| e.to_string());
                for tx in waiters {
                    let _ = tx.send(notification.clone());
                }
                result.map(|_| ())
            }
        }
    }

    /// Ensures the database is open.
    async fn ensure_open(&self) -> Result<(), DataError> {
        if matches!(&*self.state.borrow(), OpenState::Open(_)) {
            return Ok(());
        }
        self.open().await
    }

    /// Gets the database reference.
    fn get_db(&self) -> Result<IdbDatabase, DataError> {
        match &*self.state.borrow() {
            OpenState::Open(db) => Ok(db.clone()),
            _ => Err(DataError::NotOpen),
        }
    }

    /// Executes a readwrite transaction on a single object store.
    ///
    /// The closure receives a [`WriteTransaction`] and runs synchronously — no
    /// `.await` is possible inside it, which enforces the IDB rule that
    /// readwrite transactions must not yield to the event loop.
    async fn write_tx<F, T>(&self, store_name: &str, f: F) -> Result<T, DataError>
    where
        F: FnOnce(&WriteTransaction) -> Result<T, DataError>,
    {
        let db = self.get_db()?;
        let tx = db
            .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readwrite)
            .map_err(|e| DataError::TransactionFailed(format!("{:?}", e)))?;
        let result = f(&WriteTransaction::new(&tx))?;
        wait_for_transaction(&tx).await?;
        Ok(result)
    }

    /// Executes a readwrite transaction spanning multiple object stores.
    ///
    /// Same safety guarantee as [`write_tx`]: the closure is synchronous,
    /// preventing any `.await` inside the transaction scope.
    async fn write_tx_multi<F, T>(&self, store_names: &[&str], f: F) -> Result<T, DataError>
    where
        F: FnOnce(&WriteTransaction) -> Result<T, DataError>,
    {
        let db = self.get_db()?;
        let names = Array::new();
        for name in store_names {
            names.push(&JsValue::from_str(name));
        }
        let tx = db
            .transaction_with_str_sequence_and_mode(&names, IdbTransactionMode::Readwrite)
            .map_err(|e| DataError::TransactionFailed(format!("{:?}", e)))?;
        let result = f(&WriteTransaction::new(&tx))?;
        wait_for_transaction(&tx).await?;
        Ok(result)
    }

    /// Executes a readonly single-key get on an object store.
    async fn read_one(&self, store_name: &str, key: &str) -> Result<JsValue, DataError> {
        let db = self.get_db()?;
        let tx = db
            .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readonly)
            .map_err(|e| DataError::TransactionFailed(format!("{:?}", e)))?;
        let store = tx
            .object_store(store_name)
            .map_err(|e| DataError::TransactionFailed(format!("{:?}", e)))?;
        let request = store
            .get(&JsValue::from_str(key))
            .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
        wait_for_request(&request).await
    }

    /// Executes a readonly get_all and deserializes results into a Vec.
    async fn read_all<T: DeserializeOwned>(&self, store_name: &str) -> Result<Vec<T>, DataError> {
        let db = self.get_db()?;
        let tx = db
            .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readonly)
            .map_err(|e| DataError::TransactionFailed(format!("{:?}", e)))?;
        let store = tx
            .object_store(store_name)
            .map_err(|e| DataError::TransactionFailed(format!("{:?}", e)))?;
        let request = store
            .get_all()
            .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
        let result = wait_for_request(&request).await?;
        let array = Array::from(&result);
        Ok(deserialize_js_array(&array))
    }

    // ========================================================================
    // Sweep operations
    // ========================================================================

    /// Stores multiple pre-computed sweep blobs in a single IDB transaction.
    ///
    /// Batches all writes into one readwrite transaction to avoid per-transaction
    /// disk-flush overhead. The [`WriteTransaction`] closure guarantees no
    /// `.await` between puts — IDB transactions auto-commit when the event
    /// loop yields in WASM.
    ///
    /// Checks browser storage quota before writing. If remaining quota is
    /// insufficient for the batch, returns an error instead of silently failing.
    pub async fn put_sweeps_batch(&self, items: &[(String, Vec<u8>)]) -> Result<(), DataError> {
        if items.is_empty() {
            return Ok(());
        }

        // Pre-write quota check: verify browser has enough storage remaining
        let batch_size: u64 = items.iter().map(|(_, data)| data.len() as u64).sum();
        if let Some(estimate) = estimate_browser_quota().await {
            let remaining = estimate.remaining();
            // Require the write size plus 5 MB headroom for IDB overhead/metadata
            let required = batch_size + 5 * 1024 * 1024;
            if remaining < required {
                return Err(DataError::QuotaExceeded {
                    available_mb: remaining as f64 / (1024.0 * 1024.0),
                    required_mb: required as f64 / (1024.0 * 1024.0),
                });
            }
        }

        self.ensure_open().await?;

        self.write_tx(STORE_SWEEPS, |wtx| {
            let store = wtx.object_store(STORE_SWEEPS)?;
            for (key, data) in items {
                let array = Uint8Array::from(data.as_slice());
                let buffer = array.buffer();
                store
                    .put_with_key(&buffer, &JsValue::from_str(key))
                    .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
            }
            Ok(())
        })
        .await
    }

    /// Gets a pre-computed sweep blob by key, returning the raw JS ArrayBuffer.
    /// Avoids the 5MB+ copy from JS to Rust that `get_sweep` performs.
    pub async fn get_sweep_as_js(&self, key: &str) -> Result<Option<ArrayBuffer>, DataError> {
        self.ensure_open().await?;
        let result = self.read_one(STORE_SWEEPS, key).await?;

        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }

        let buffer: ArrayBuffer = result
            .dyn_into()
            .map_err(|_| DataError::DeserializationError("Expected ArrayBuffer".to_string()))?;
        Ok(Some(buffer))
    }

    // ========================================================================
    // Scan index operations
    // ========================================================================

    /// Writes or updates a scan index entry.
    pub async fn put_scan_index_entry(&self, entry: &ScanIndexEntry) -> Result<(), DataError> {
        self.ensure_open().await?;
        let storage_key = entry.storage_key();

        // Serialize before entering the transaction scope to keep the closure
        // as lean as possible.
        let json = serde_json::to_string(entry)
            .map_err(|e| DataError::DeserializationError(format!("{}", e)))?;
        let js = js_sys::JSON::parse(&json)
            .map_err(|e| DataError::DeserializationError(format!("{:?}", e)))?;

        self.write_tx(STORE_SCAN_INDEX, |wtx| {
            let store = wtx.object_store(STORE_SCAN_INDEX)?;
            store
                .put_with_key(&js, &JsValue::from_str(&storage_key))
                .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
            Ok(())
        })
        .await
    }

    /// Gets scan availability information.
    pub async fn scan_availability(
        &self,
        scan: &ScanKey,
    ) -> Result<Option<ScanIndexEntry>, DataError> {
        self.ensure_open().await?;
        let storage_key = scan.to_storage_key();
        let result = self.read_one(STORE_SCAN_INDEX, &storage_key).await?;
        Ok(deserialize_js_value(&result))
    }

    /// Lists all scans for a site within a time window.
    pub async fn list_scans(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> Result<Vec<ScanIndexEntry>, DataError> {
        self.ensure_open().await?;
        let entries: Vec<ScanIndexEntry> = self.read_all(STORE_SCAN_INDEX).await?;

        let mut scans: Vec<ScanIndexEntry> = entries
            .into_iter()
            .filter(|entry| {
                entry.scan.site.0 == site.0
                    && entry.scan.scan_start >= start
                    && entry.scan.scan_start <= end
            })
            .collect();

        scans.sort_by_key(|s| s.scan.scan_start.0);
        Ok(scans)
    }

    /// Gets total cache size across all scans.
    pub async fn total_cache_size(&self) -> Result<u64, DataError> {
        self.ensure_open().await?;
        let entries: Vec<ScanIndexEntry> = self.read_all(STORE_SCAN_INDEX).await?;
        let total: u64 = entries.iter().map(|e| e.total_size_bytes).sum();
        Ok(total)
    }

    /// Gets scans sorted by last_accessed_at (oldest first) for LRU eviction.
    pub async fn get_lru_scans(&self, limit: u32) -> Result<Vec<ScanIndexEntry>, DataError> {
        self.ensure_open().await?;
        let mut scans: Vec<ScanIndexEntry> = self.read_all(STORE_SCAN_INDEX).await?;
        scans.sort_by_key(|s| s.last_accessed_at.0);
        scans.truncate(limit as usize);
        Ok(scans)
    }

    /// Deletes a scan and all its sweep blobs.
    /// Returns the number of bytes freed.
    pub async fn delete_scan(&self, scan: &ScanKey) -> Result<u64, DataError> {
        self.ensure_open().await?;

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
                        let key = SweepDataKey::new(scan.clone(), sweep.elevation_number, *product);
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
        self.write_tx_multi(&[STORE_SWEEPS, STORE_SCAN_INDEX], |wtx| {
            let sweeps_store = wtx.object_store(STORE_SWEEPS)?;
            let scan_store = wtx.object_store(STORE_SCAN_INDEX)?;

            for key in &sweep_keys {
                sweeps_store
                    .delete(&JsValue::from_str(key))
                    .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
            }

            scan_store
                .delete(&JsValue::from_str(&scan_storage_key))
                .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
            Ok(())
        })
        .await?;

        log::debug!(
            "Deleted scan {} ({} sweep blobs, {} bytes freed)",
            scan,
            sweep_keys.len(),
            bytes_freed
        );
        Ok(bytes_freed)
    }

    /// Evicts scans until total cache size is below target_bytes.
    /// Returns the number of scans evicted.
    pub async fn evict_to_size(&self, target_bytes: u64) -> Result<u32, DataError> {
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

            log::debug!(
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

    /// Merges incremental data into an existing scan index entry, or creates one
    /// if it doesn't exist yet. Used by per-chunk ingest to build up scan metadata
    /// incrementally as elevations complete.
    ///
    /// Two-transaction pattern: read in readonly first, then write the merged
    /// result in a separate readwrite transaction (no await inside readwrite).
    pub async fn merge_scan_index_entry(
        &self,
        partial: &ScanIndexEntry,
        new_records: u32,
        new_size_bytes: u64,
        new_sweeps: &[SweepMeta],
    ) -> Result<(), DataError> {
        self.ensure_open().await?;
        let storage_key = partial.storage_key();

        // --- Readonly tx: read existing entry ---
        let existing: Option<ScanIndexEntry> = {
            let result = self.read_one(STORE_SCAN_INDEX, &storage_key).await?;
            deserialize_js_value(&result)
        };

        // --- Merge in memory ---
        let merged = if let Some(mut entry) = existing {
            entry.present_records += new_records;
            entry.total_size_bytes += new_size_bytes;
            entry.updated_at = UnixMillis::now();
            entry.has_precomputed_sweeps = true;

            // Merge VCP if newly available
            if !entry.has_vcp && partial.has_vcp {
                entry.has_vcp = true;
                entry.vcp = partial.vcp.clone();
                if let Some(ref vcp) = entry.vcp {
                    entry.expected_records = Some(vcp.elevations.len() as u32);
                }
            }

            // Merge file_name if not set
            if entry.file_name.is_none() {
                entry.file_name = partial.file_name.clone();
            }

            // Append new sweeps
            if !new_sweeps.is_empty() {
                let sweeps = entry.sweeps.get_or_insert_with(Vec::new);
                sweeps.extend_from_slice(new_sweeps);
            }

            // Update end timestamp to max
            if let Some(new_end) = partial.end_timestamp_secs {
                entry.end_timestamp_secs = Some(
                    entry
                        .end_timestamp_secs
                        .map(|old| old.max(new_end))
                        .unwrap_or(new_end),
                );
            }

            entry
        } else {
            // No existing entry — create from partial
            let mut entry = partial.clone();
            entry.present_records = new_records;
            entry.total_size_bytes = new_size_bytes;
            entry.has_precomputed_sweeps = true;
            if !new_sweeps.is_empty() {
                entry.sweeps = Some(new_sweeps.to_vec());
            }
            entry
        };

        // --- Readwrite tx: write merged entry ---
        let json = serde_json::to_string(&merged)
            .map_err(|e| DataError::DeserializationError(format!("{}", e)))?;
        let js = js_sys::JSON::parse(&json)
            .map_err(|e| DataError::DeserializationError(format!("{:?}", e)))?;

        self.write_tx(STORE_SCAN_INDEX, |wtx| {
            let store = wtx.object_store(STORE_SCAN_INDEX)?;
            store
                .put_with_key(&js, &JsValue::from_str(&storage_key))
                .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
            Ok(())
        })
        .await
    }

    /// Deletes all existing scans for a site whose time range overlaps with the
    /// given archive scan. Returns the number of scans deleted.
    ///
    /// A scan overlaps if its [start, end] range intersects the archive scan's
    /// range. Scans without an end timestamp use start as the end.
    /// The scan matching `exclude_key` (the archive scan itself) is skipped.
    pub async fn delete_overlapping_scans(
        &self,
        site: &SiteId,
        archive_start: UnixMillis,
        archive_end_ms: i64,
        exclude_key: &ScanKey,
    ) -> Result<u32, DataError> {
        self.ensure_open().await?;

        // Read all scan index entries
        let all_entries: Vec<ScanIndexEntry> = self.read_all(STORE_SCAN_INDEX).await?;

        // Find overlapping scans for this site
        let mut to_delete: Vec<ScanKey> = Vec::new();
        for entry in &all_entries {
            if entry.scan.site.0 != site.0 {
                continue;
            }
            if entry.scan == *exclude_key {
                continue;
            }
            let existing_start = entry.scan.scan_start.0;
            let existing_end = entry
                .end_timestamp_secs
                .map(|s| s * 1000)
                .unwrap_or(existing_start);

            // Two ranges overlap if start_a <= end_b AND start_b <= end_a
            if archive_start.0 <= existing_end && existing_start <= archive_end_ms {
                to_delete.push(entry.scan.clone());
            }
        }

        if to_delete.is_empty() {
            return Ok(0);
        }

        let count = to_delete.len() as u32;
        for scan in &to_delete {
            log::debug!("Deleting overlapping scan {} (replaced by archive)", scan);
            self.delete_scan(scan).await?;
        }

        Ok(count)
    }

    /// Queries the browser's storage quota via `navigator.storage.estimate()`.
    ///
    /// Works in both Window and Worker contexts. Returns `None` if the
    /// Storage API is unavailable (e.g. older browsers, opaque origins).
    pub async fn estimate_storage_quota() -> Option<StorageQuotaEstimate> {
        estimate_browser_quota().await
    }

    /// Clears all data from all stores.
    pub async fn clear_all(&self) -> Result<(), DataError> {
        // Clear each object store rather than deleting the database.
        // deleteDatabase would hang if any other connection (e.g. the worker)
        // is still open, because the delete is blocked until ALL connections close.
        self.ensure_open().await?;

        self.write_tx_multi(&[STORE_SWEEPS, STORE_SCAN_INDEX], |wtx| {
            wtx.object_store(STORE_SWEEPS)?
                .clear()
                .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
            wtx.object_store(STORE_SCAN_INDEX)?
                .clear()
                .map_err(|e| DataError::RequestFailed(format!("{:?}", e)))?;
            Ok(())
        })
        .await?;

        log::info!("Cleared all IndexedDB stores");
        Ok(())
    }
}

// ============================================================================
// WriteTransaction — enforces "no await inside readwrite" at the type level
// ============================================================================

/// A synchronous handle to an IDB readwrite transaction.
///
/// `WriteTransaction` is the sole way to perform write operations. It is
/// handed to a closure by [`IndexedDbRecordStore::write_tx`] /
/// [`write_tx_multi`], and because the closure is `FnOnce` (not
/// `async FnOnce`), the compiler rejects any `.await` inside it.
///
/// The `PhantomData<*const ()>` marker makes the type `!Send`, which
/// provides an additional safety net against accidental moves across
/// threads or await points in non-WASM contexts.
pub struct WriteTransaction<'a> {
    tx: &'a IdbTransaction,
    /// Prevents `Send` — extra guard against cross-await usage.
    _not_send: PhantomData<*const ()>,
}

impl<'a> WriteTransaction<'a> {
    fn new(tx: &'a IdbTransaction) -> Self {
        Self {
            tx,
            _not_send: PhantomData,
        }
    }

    /// Gets an object store from this transaction.
    pub fn object_store(&self, name: &str) -> Result<IdbObjectStore, DataError> {
        self.tx
            .object_store(name)
            .map_err(|e| DataError::TransactionFailed(format!("{:?}", e)))
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Gets the IdbFactory from the current global scope (works in both Window and Worker).
fn get_idb_factory() -> Result<web_sys::IdbFactory, DataError> {
    let global = js_sys::global();
    let idb = js_sys::Reflect::get(&global, &wasm_bindgen::JsValue::from_str("indexedDB"))
        .map_err(|e| {
            DataError::TransactionFailed(format!("Failed to access indexedDB: {:?}", e))
        })?;
    if idb.is_undefined() || idb.is_null() {
        return Err(DataError::TransactionFailed(
            "IndexedDB not available in this context".to_string(),
        ));
    }
    idb.dyn_into::<web_sys::IdbFactory>()
        .map_err(|_| DataError::TransactionFailed("indexedDB is not an IdbFactory".to_string()))
}

/// Opens the database, creating schema as needed.
async fn open_database() -> Result<IdbDatabase, DataError> {
    let idb_factory = get_idb_factory()?;

    let open_request = idb_factory
        .open_with_u32(DATABASE_NAME, DATABASE_VERSION)
        .map_err(|e| DataError::TransactionFailed(format!("Failed to open database: {:?}", e)))?;

    // Set up upgrade handler
    let onupgradeneeded = Closure::wrap(Box::new(move |event: web_sys::IdbVersionChangeEvent| {
        let request: IdbRequest = event
            .target()
            .unwrap()
            .dyn_into()
            .expect("Expected IdbRequest");
        let db: IdbDatabase = request.result().unwrap().dyn_into().unwrap();

        // Delete all existing stores and recreate — breaking schema change
        let store_names = db.object_store_names();
        for i in 0..store_names.length() {
            if let Some(name) = store_names.get(i) {
                db.delete_object_store(&name)
                    .expect("Failed to delete object store");
                log::info!("Deleted IndexedDB store: {}", name);
            }
        }

        // Create fresh stores
        for store_name in [STORE_SWEEPS, STORE_SCAN_INDEX] {
            db.create_object_store(store_name)
                .expect("Failed to create object store");
            log::info!("Created IndexedDB store: {}", store_name);
        }
    }) as Box<dyn FnMut(_)>);

    open_request.set_onupgradeneeded(Some(onupgradeneeded.as_ref().unchecked_ref()));
    onupgradeneeded.forget();

    // Wait for database to open
    let db_result = wait_for_request(&open_request).await?;
    let db: IdbDatabase = db_result
        .dyn_into()
        .map_err(|_| DataError::TransactionFailed("Failed to cast to IdbDatabase".to_string()))?;

    log::info!("Opened IndexedDB {} v{}", DATABASE_NAME, DATABASE_VERSION);

    Ok(db)
}

/// Waits for an IDB request to complete.
async fn wait_for_request(request: &IdbRequest) -> Result<JsValue, DataError> {
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

    let result = rx
        .await
        .map_err(|_| DataError::RequestFailed("Channel closed".to_string()))?;

    request.set_onsuccess(None);
    request.set_onerror(None);

    drop(onsuccess);
    drop(onerror);

    result.map_err(DataError::RequestFailed)
}

/// Waits for an IDB transaction to complete.
async fn wait_for_transaction(tx: &IdbTransaction) -> Result<(), DataError> {
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

    let result = rx
        .await
        .map_err(|_| DataError::TransactionFailed("Channel closed".to_string()))?;

    tx.set_oncomplete(None);
    tx.set_onerror(None);

    drop(oncomplete);
    drop(onerror);

    result.map_err(DataError::TransactionFailed)
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

fn deserialize_js_array<T: DeserializeOwned>(array: &Array) -> Vec<T> {
    let mut items = Vec::new();
    for i in 0..array.length() {
        let value = array.get(i);
        if let Some(item) = deserialize_js_value(&value) {
            items.push(item);
        }
    }
    items
}

/// Queries `navigator.storage.estimate()` from either Window or Worker context.
///
/// Returns `None` if the Storage Manager API is unavailable.
async fn estimate_browser_quota() -> Option<StorageQuotaEstimate> {
    let global = js_sys::global();

    // Try Window context first, then Worker context
    let storage_manager = {
        // Window context
        let window: Result<web_sys::Window, _> = global.clone().dyn_into();
        if let Ok(win) = window {
            web_sys::Navigator::storage(&win.navigator())
        } else {
            // Worker context
            let worker: Result<web_sys::WorkerGlobalScope, _> = global.dyn_into();
            if let Ok(ws) = worker {
                web_sys::WorkerNavigator::storage(&ws.navigator())
            } else {
                log::debug!("Storage API: not in Window or Worker context");
                return None;
            }
        }
    };

    let promise = web_sys::StorageManager::estimate(&storage_manager).ok()?;
    let result = wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    let estimate: web_sys::StorageEstimate = result.dyn_into().ok()?;

    let quota = web_sys::StorageEstimate::get_quota(&estimate).unwrap_or(0.0) as u64;
    let usage = web_sys::StorageEstimate::get_usage(&estimate).unwrap_or(0.0) as u64;

    Some(StorageQuotaEstimate { quota, usage })
}
