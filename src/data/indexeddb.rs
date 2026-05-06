//! IndexedDB storage for pre-computed radar sweep data.
//!
//! ## Object Stores
//!
//! 1. `sweeps` - Pre-computed sweep blobs (ArrayBuffer)
//!    - Key: "SITE|SCAN_MS|ELEV_NUM|PRODUCT"
//!
//! 2. `scan_index` - Per-scan metadata (`ScanIndexEntry`, structured-cloned)
//!    - Key: "SITE|SCAN_START_MS"
//!
//! ## Concurrency
//!
//! - `open()` coalesces concurrent calls behind a single `indexedDB.open` via
//!   an `OpenState` machine.
//! - All writes go through `write_tx`, which hands the closure a synchronous
//!   `WriteTransaction`. The closure is `FnOnce` (not `async`), so the
//!   compiler rejects `.await` inside it — enforcing the WASM IDB rule that
//!   readwrite transactions auto-commit when the event loop yields.
//! - Cross-store transactions are supported by passing a multi-store slice.
//! - Read-modify-write (e.g. merging into an existing scan-index entry) is
//!   *not* atomic at the IDB layer: callers must ensure single-writer
//!   serialization for a given key. The ingest pipeline does this via the
//!   per-worker `CHUNK_ACCUM` thread-local.

use crate::data::keys::*;
use js_sys::{Array, ArrayBuffer, Uint8Array};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    IdbDatabase, IdbKeyRange, IdbObjectStore, IdbRequest, IdbTransaction, IdbTransactionMode,
};

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
    /// (De)serialization of stored data failed.
    SerdeError(String),
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
            DataError::SerdeError(msg) => write!(f, "Serde error: {}", msg),
        }
    }
}

/// Format a `JsValue` error into a string. Tries to extract `name`/`message`
/// when the JsValue is a DOMException-like object; falls back to `{:?}`.
fn js_err(e: JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    let name = js_sys::Reflect::get(&e, &JsValue::from_str("name"))
        .ok()
        .and_then(|v| v.as_string());
    let msg = js_sys::Reflect::get(&e, &JsValue::from_str("message"))
        .ok()
        .and_then(|v| v.as_string());
    match (name, msg) {
        (Some(n), Some(m)) => format!("{}: {}", n, m),
        (Some(n), None) => n,
        (None, Some(m)) => m,
        (None, None) => format!("{:?}", e),
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
const DATABASE_VERSION: u32 = 5;

/// Database name.
const DATABASE_NAME: &str = "nexrad-workbench";

/// Object store names.
const STORE_SWEEPS: &str = "sweeps";
const STORE_SCAN_INDEX: &str = "scan_index";
/// Per-scan last-access timestamps for LRU eviction. Lives in its own
/// store so that fire-and-forget access bumps from `get_sweep` don't
/// race with chunk-ingest's read-modify-write of the scan-index entry.
const STORE_SCAN_TOUCHES: &str = "scan_touches";

/// Sentinel character used as the upper bound of prefix key ranges.
/// `\u{FFFF}` sorts after any reasonable storage-key character.
const PREFIX_RANGE_UPPER: char = '\u{FFFF}';

/// Minimum interval between in-memory touch deduplications. Limits how often
/// a single scan's `last_accessed_at` is rewritten to IDB during fast scrub.
const TOUCH_THROTTLE_MS: i64 = 60_000;

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

/// IndexedDB store for sweep blobs and scan-index metadata.
#[derive(Clone)]
pub struct IndexedDbStore {
    state: Rc<RefCell<OpenState>>,
    /// Per-scan in-memory throttle for touch writes. Maps scan key to the
    /// last time `touch_scan` enqueued an IDB write for it. A second touch
    /// inside `TOUCH_THROTTLE_MS` is a no-op.
    recent_touches: Rc<RefCell<HashMap<ScanKey, UnixMillis>>>,
}

impl Default for IndexedDbStore {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexedDbStore {
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(OpenState::Closed)),
            recent_touches: Rc::new(RefCell::new(HashMap::new())),
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

    /// Executes a readwrite transaction over one or more object stores.
    ///
    /// The closure receives a [`WriteTransaction`] and runs synchronously — no
    /// `.await` is possible inside it, which enforces the IDB rule that
    /// readwrite transactions must not yield to the event loop.
    async fn write_tx<F, T>(&self, store_names: &[&str], f: F) -> Result<T, DataError>
    where
        F: FnOnce(&WriteTransaction) -> Result<T, DataError>,
    {
        let db = self.get_db()?;
        let tx = match store_names {
            [] => {
                return Err(DataError::TransactionFailed(
                    "write_tx requires at least one store".to_string(),
                ))
            }
            [single] => db
                .transaction_with_str_and_mode(single, IdbTransactionMode::Readwrite)
                .map_err(|e| DataError::TransactionFailed(js_err(e)))?,
            many => {
                let names = Array::new();
                for name in many {
                    names.push(&JsValue::from_str(name));
                }
                db.transaction_with_str_sequence_and_mode(&names, IdbTransactionMode::Readwrite)
                    .map_err(|e| DataError::TransactionFailed(js_err(e)))?
            }
        };
        let result = f(&WriteTransaction::new(&tx))?;
        wait_for_transaction(&tx).await?;
        Ok(result)
    }

    /// Executes a readonly request on a single store and returns the raw JS result.
    async fn read<F>(&self, store_name: &str, build_request: F) -> Result<JsValue, DataError>
    where
        F: FnOnce(&IdbObjectStore) -> Result<IdbRequest, JsValue>,
    {
        let db = self.get_db()?;
        let tx = db
            .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readonly)
            .map_err(|e| DataError::TransactionFailed(js_err(e)))?;
        let store = tx
            .object_store(store_name)
            .map_err(|e| DataError::TransactionFailed(js_err(e)))?;
        let request = build_request(&store).map_err(|e| DataError::RequestFailed(js_err(e)))?;
        wait_for_request(&request).await
    }

    // ========================================================================
    // Sweep operations
    // ========================================================================

    /// Pre-write browser-quota check for a sweep-blob batch. Returns
    /// `QuotaExceeded` when remaining quota is below `batch + 5 MB` headroom.
    async fn check_quota(sweep_blobs: &[(String, Vec<u8>)]) -> Result<(), DataError> {
        let batch_size: u64 = sweep_blobs.iter().map(|(_, data)| data.len() as u64).sum();
        if batch_size == 0 {
            return Ok(());
        }
        let Some(estimate) = estimate_browser_quota().await else {
            return Ok(());
        };
        let remaining = estimate.remaining();
        let required = batch_size + 5 * 1024 * 1024;
        if remaining < required {
            return Err(DataError::QuotaExceeded {
                available_mb: remaining as f64 / (1024.0 * 1024.0),
                required_mb: required as f64 / (1024.0 * 1024.0),
            });
        }
        Ok(())
    }

    /// Atomically writes a *new* scan: blobs, scan-index entry, and an
    /// initial `scan_touches` timestamp, all in one cross-store readwrite
    /// transaction.
    ///
    /// The initial touch is what gives a fresh scan its place in the LRU
    /// order. If you call this for a scan key that already exists, the
    /// existing `scan_touches` value is overwritten with `now` — fine when
    /// the scan is genuinely being replaced (e.g. archive supersedes a prior
    /// real-time entry under the same key), but the wrong choice for an
    /// in-progress chunk-ingest merge: use [`put_scan`] for those.
    pub async fn create_scan(
        &self,
        entry: &ScanIndexEntry,
        sweep_blobs: &[(String, Vec<u8>)],
    ) -> Result<(), DataError> {
        Self::check_quota(sweep_blobs).await?;

        self.ensure_open().await?;
        let entry_key = entry.storage_key();
        let entry_value = to_js_value(entry)?;
        let touch_value = JsValue::from_f64(UnixMillis::now().0 as f64);

        self.write_tx(
            &[STORE_SWEEPS, STORE_SCAN_INDEX, STORE_SCAN_TOUCHES],
            |wtx| {
                let sweeps = wtx.object_store(STORE_SWEEPS)?;
                for (key, data) in sweep_blobs {
                    let array = Uint8Array::from(data.as_slice());
                    let buffer = array.buffer();
                    sweeps
                        .put_with_key(&buffer, &JsValue::from_str(key))
                        .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                }
                wtx.object_store(STORE_SCAN_INDEX)?
                    .put_with_key(&entry_value, &JsValue::from_str(&entry_key))
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                wtx.object_store(STORE_SCAN_TOUCHES)?
                    .put_with_key(&touch_value, &JsValue::from_str(&entry_key))
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                Ok(())
            },
        )
        .await
    }

    /// Atomically updates an existing scan: blobs + scan-index entry, in one
    /// cross-store readwrite transaction. Leaves `scan_touches` alone so a
    /// chunk-ingest flush doesn't refresh the access timestamp on every
    /// partial write.
    ///
    /// Empty `sweep_blobs` writes the entry alone (e.g. a chunk-ingest flush
    /// that updates merge state without producing new blobs).
    ///
    /// Use [`create_scan`] for first-time writes — calling `put_scan` for a
    /// key that has no `scan_touches` entry leaves the scan with no LRU
    /// placement and it'll be evicted on the next pass.
    pub async fn put_scan(
        &self,
        entry: &ScanIndexEntry,
        sweep_blobs: &[(String, Vec<u8>)],
    ) -> Result<(), DataError> {
        Self::check_quota(sweep_blobs).await?;

        self.ensure_open().await?;
        let entry_key = entry.storage_key();
        let entry_value = to_js_value(entry)?;

        self.write_tx(&[STORE_SWEEPS, STORE_SCAN_INDEX], |wtx| {
            let sweeps = wtx.object_store(STORE_SWEEPS)?;
            for (key, data) in sweep_blobs {
                let array = Uint8Array::from(data.as_slice());
                let buffer = array.buffer();
                sweeps
                    .put_with_key(&buffer, &JsValue::from_str(key))
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
            }
            wtx.object_store(STORE_SCAN_INDEX)?
                .put_with_key(&entry_value, &JsValue::from_str(&entry_key))
                .map_err(|e| DataError::RequestFailed(js_err(e)))?;
            Ok(())
        })
        .await
    }

    /// Gets a pre-computed sweep blob, returning the raw JS ArrayBuffer.
    /// Returning the JS-side buffer avoids copying the (potentially several-MB)
    /// blob through Rust memory before it is uploaded to the GPU.
    ///
    /// Fires a fire-and-forget `touch_scan` for the scan-key portion after
    /// returning the buffer, so LRU eviction prefers recently-rendered scans.
    pub async fn get_sweep(&self, key: &SweepDataKey) -> Result<Option<ArrayBuffer>, DataError> {
        self.ensure_open().await?;
        let storage_key = key.to_storage_key();
        let result = self
            .read(STORE_SWEEPS, |store| {
                store.get(&JsValue::from_str(&storage_key))
            })
            .await?;

        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }

        let buffer: ArrayBuffer = result
            .dyn_into()
            .map_err(|_| DataError::SerdeError("Expected ArrayBuffer".to_string()))?;

        self.touch_scan(&key.scan);
        Ok(Some(buffer))
    }

    /// Bumps `last_accessed_at` for `scan` to now in the `scan_touches`
    /// store. Fire-and-forget — the IDB write is spawned and not awaited so
    /// the render hot path is unaffected. Throttled in-memory: a second
    /// touch for the same scan within `TOUCH_THROTTLE_MS` is a no-op.
    /// Errors are logged at debug level and otherwise swallowed.
    ///
    /// Lives in its own object store rather than mutating
    /// `ScanIndexEntry.last_accessed_at` because chunk-ingest runs an
    /// (intentionally non-atomic) read-modify-write on the entry to merge
    /// in new chunk state. A racing touch RMW would clobber merge results.
    /// A dedicated single-field store sidesteps that race.
    fn touch_scan(&self, scan: &ScanKey) {
        let now = UnixMillis::now();
        {
            let mut touches = self.recent_touches.borrow_mut();
            if let Some(last) = touches.get(scan) {
                if now.0 - last.0 < TOUCH_THROTTLE_MS {
                    return;
                }
            }
            touches.insert(scan.clone(), now);
        }

        let store = self.clone();
        let scan = scan.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = store.write_touch(&scan, now).await {
                log::debug!("touch_scan {} failed: {}", scan, e);
            }
        });
    }

    async fn write_touch(&self, scan: &ScanKey, time: UnixMillis) -> Result<(), DataError> {
        self.ensure_open().await?;
        let key = scan.to_storage_key();
        let value = JsValue::from_f64(time.0 as f64);
        self.write_tx(&[STORE_SCAN_TOUCHES], |wtx| {
            wtx.object_store(STORE_SCAN_TOUCHES)?
                .put_with_key(&value, &JsValue::from_str(&key))
                .map_err(|e| DataError::RequestFailed(js_err(e)))?;
            Ok(())
        })
        .await
    }

    /// Reads every entry in the `scan_touches` store as a map. Used by
    /// LRU eviction to join with `scan_index`.
    async fn read_all_touches(&self) -> Result<HashMap<ScanKey, UnixMillis>, DataError> {
        let db = self.get_db()?;
        let tx = db
            .transaction_with_str_and_mode(STORE_SCAN_TOUCHES, IdbTransactionMode::Readonly)
            .map_err(|e| DataError::TransactionFailed(js_err(e)))?;
        let store = tx
            .object_store(STORE_SCAN_TOUCHES)
            .map_err(|e| DataError::TransactionFailed(js_err(e)))?;

        // Issue both requests synchronously so the transaction stays active
        // across the awaits; they queue into the tx and resolve in order.
        let keys_req = store
            .get_all_keys()
            .map_err(|e| DataError::RequestFailed(js_err(e)))?;
        let vals_req = store
            .get_all()
            .map_err(|e| DataError::RequestFailed(js_err(e)))?;

        let keys_result = wait_for_request(&keys_req).await?;
        let vals_result = wait_for_request(&vals_req).await?;

        let keys = Array::from(&keys_result);
        let vals = Array::from(&vals_result);
        let len = keys.length().min(vals.length());

        let mut map = HashMap::with_capacity(len as usize);
        for i in 0..len {
            let Some(key_str) = keys.get(i).as_string() else {
                continue;
            };
            let Some(scan) = ScanKey::from_storage_key(&key_str) else {
                continue;
            };
            let Some(ms) = vals.get(i).as_f64() else {
                continue;
            };
            map.insert(scan, UnixMillis(ms as i64));
        }
        Ok(map)
    }

    // ========================================================================
    // Scan index operations
    // ========================================================================

    /// Gets scan availability information.
    pub async fn scan_availability(
        &self,
        scan: &ScanKey,
    ) -> Result<Option<ScanIndexEntry>, DataError> {
        self.ensure_open().await?;
        let storage_key = scan.to_storage_key();
        let result = self
            .read(STORE_SCAN_INDEX, |store| {
                store.get(&JsValue::from_str(&storage_key))
            })
            .await?;
        from_js_value_opt(&result)
    }

    /// Lists all scans for a site within a time window.
    ///
    /// Uses an IDB key range bounded by the site prefix (`"SITE|"`..),
    /// reading only that site's entries rather than the entire store. The
    /// time-window filter runs in Rust afterward.
    pub async fn list_scans(
        &self,
        site: &SiteId,
        start: UnixMillis,
        end: UnixMillis,
    ) -> Result<Vec<ScanIndexEntry>, DataError> {
        self.ensure_open().await?;
        let range = site_prefix_range(site)?;
        let result = self
            .read(STORE_SCAN_INDEX, |store| {
                store.get_all_with_key(&range.into())
            })
            .await?;
        let entries: Vec<ScanIndexEntry> = deserialize_js_array(&Array::from(&result));

        let mut scans: Vec<ScanIndexEntry> = entries
            .into_iter()
            .filter(|entry| entry.scan.scan_start >= start && entry.scan.scan_start <= end)
            .collect();

        scans.sort_by_key(|s| s.scan.scan_start.0);
        Ok(scans)
    }

    /// Returns all scan-index entries (across all sites). Used by cache-wide
    /// operations like total-size accounting and LRU eviction.
    async fn read_all_scan_entries(&self) -> Result<Vec<ScanIndexEntry>, DataError> {
        let result = self.read(STORE_SCAN_INDEX, |store| store.get_all()).await?;
        Ok(deserialize_js_array(&Array::from(&result)))
    }

    /// Gets total cache size across all scans.
    pub async fn total_cache_size(&self) -> Result<u64, DataError> {
        self.ensure_open().await?;
        let entries = self.read_all_scan_entries().await?;
        Ok(entries.iter().map(|e| e.total_size_bytes).sum())
    }

    /// Deletes a scan and all its sweep blobs in one cross-store transaction.
    /// Returns the number of bytes freed.
    ///
    /// Sweeps are deleted via an IDB key range covering the prefix
    /// `"SITE|SCAN_MS|"`, so this works regardless of which products were
    /// stored. The scan's `scan_touches` entry is also dropped.
    ///
    /// Crate-private: external callers should evict via `evict_to_size`.
    pub(crate) async fn delete_scan(&self, scan: &ScanKey) -> Result<u64, DataError> {
        self.ensure_open().await?;

        let scan_storage_key = scan.to_storage_key();
        let bytes_freed = self
            .scan_availability(scan)
            .await?
            .map(|e| e.total_size_bytes)
            .unwrap_or(0);

        let sweeps_range = scan_prefix_range(scan)?;

        self.write_tx(
            &[STORE_SWEEPS, STORE_SCAN_INDEX, STORE_SCAN_TOUCHES],
            |wtx| {
                wtx.object_store(STORE_SWEEPS)?
                    .delete(&sweeps_range.into())
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                wtx.object_store(STORE_SCAN_INDEX)?
                    .delete(&JsValue::from_str(&scan_storage_key))
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                wtx.object_store(STORE_SCAN_TOUCHES)?
                    .delete(&JsValue::from_str(&scan_storage_key))
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                Ok(())
            },
        )
        .await?;

        // Drop the in-memory throttle entry so a subsequent re-ingest of the
        // same scan key isn't suppressed.
        self.recent_touches.borrow_mut().remove(scan);

        log::debug!("Deleted scan {} ({} bytes freed)", scan, bytes_freed);
        Ok(bytes_freed)
    }

    /// Evicts scans (oldest access first) until total cache size is at or
    /// below `target_bytes`. Returns the number of scans evicted.
    ///
    /// Sort key is the `scan_touches` timestamp. `create_scan` writes an
    /// initial touch at ingest time, and `touch_scan` (fired by `get_sweep`)
    /// refreshes it on render. Entries with no touch — anomalous, but
    /// possible if a scan was written via `put_scan` without a prior
    /// `create_scan` — sort to position 0 and are evicted first, which
    /// reclaims any stranded data.
    ///
    /// Reads all entries + touches once, sorts, and deletes one scan at a
    /// time until the running size estimate drops under target. Each delete
    /// is its own transaction (so a failure mid-eviction still leaves a
    /// consistent store).
    pub async fn evict_to_size(&self, target_bytes: u64) -> Result<u32, DataError> {
        self.ensure_open().await?;

        let mut entries = self.read_all_scan_entries().await?;
        let mut current_size: u64 = entries.iter().map(|e| e.total_size_bytes).sum();
        if current_size <= target_bytes {
            return Ok(0);
        }

        let touches = self.read_all_touches().await?;
        entries.sort_by_key(|e| touches.get(&e.scan).copied().unwrap_or(UnixMillis(0)).0);

        let mut evicted_count = 0u32;
        for entry in &entries {
            if current_size <= target_bytes {
                break;
            }
            let bytes_freed = self.delete_scan(&entry.scan).await?;
            current_size = current_size.saturating_sub(bytes_freed);
            evicted_count += 1;
            log::debug!(
                "Evicted scan {} (freed {} bytes, {} remaining)",
                entry.scan,
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

    /// Queries the browser's storage quota via `navigator.storage.estimate()`.
    ///
    /// Works in both Window and Worker contexts. Returns `None` if the
    /// Storage API is unavailable (e.g. older browsers, opaque origins).
    pub async fn estimate_storage_quota() -> Option<StorageQuotaEstimate> {
        estimate_browser_quota().await
    }

    /// Clears all data from all stores.
    ///
    /// Preserves schema and version — does not call `deleteDatabase`, which
    /// would block until every other connection (e.g. the worker's) closed.
    pub async fn clear_all(&self) -> Result<(), DataError> {
        self.ensure_open().await?;

        self.write_tx(
            &[STORE_SWEEPS, STORE_SCAN_INDEX, STORE_SCAN_TOUCHES],
            |wtx| {
                wtx.object_store(STORE_SWEEPS)?
                    .clear()
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                wtx.object_store(STORE_SCAN_INDEX)?
                    .clear()
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                wtx.object_store(STORE_SCAN_TOUCHES)?
                    .clear()
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
                Ok(())
            },
        )
        .await?;

        self.recent_touches.borrow_mut().clear();

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
/// handed to a closure by [`IndexedDbStore::write_tx`], and because the
/// closure is `FnOnce` (not `async FnOnce`), the compiler rejects any
/// `.await` inside it.
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
            .map_err(|e| DataError::TransactionFailed(js_err(e)))
    }
}

// ============================================================================
// Key range helpers
// ============================================================================

/// Range covering all keys starting with `"SITE|"`. Used to scope scan-index
/// queries to a single site without scanning the whole store.
fn site_prefix_range(site: &SiteId) -> Result<IdbKeyRange, DataError> {
    let lower = format!("{}|", site.0);
    let upper = format!("{}|{}", site.0, PREFIX_RANGE_UPPER);
    IdbKeyRange::bound(&JsValue::from_str(&lower), &JsValue::from_str(&upper))
        .map_err(|e| DataError::RequestFailed(js_err(e)))
}

/// Range covering all sweep keys belonging to a single scan
/// (`"SITE|SCAN_MS|"...`). Used for prefix-deleting every elevation × product
/// blob without enumerating product names.
fn scan_prefix_range(scan: &ScanKey) -> Result<IdbKeyRange, DataError> {
    let prefix = format!("{}|", scan.to_storage_key());
    let upper = format!("{}{}", prefix, PREFIX_RANGE_UPPER);
    IdbKeyRange::bound(&JsValue::from_str(&prefix), &JsValue::from_str(&upper))
        .map_err(|e| DataError::RequestFailed(js_err(e)))
}

// ============================================================================
// Helper functions
// ============================================================================

/// Gets the IdbFactory from the current global scope (works in both Window and Worker).
fn get_idb_factory() -> Result<web_sys::IdbFactory, DataError> {
    let global = js_sys::global();
    let idb = js_sys::Reflect::get(&global, &wasm_bindgen::JsValue::from_str("indexedDB"))
        .map_err(|e| {
            DataError::TransactionFailed(format!("Failed to access indexedDB: {}", js_err(e)))
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
        .map_err(|e| {
            DataError::TransactionFailed(format!("Failed to open database: {}", js_err(e)))
        })?;

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
        for store_name in [STORE_SWEEPS, STORE_SCAN_INDEX, STORE_SCAN_TOUCHES] {
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

/// Convert a serializable Rust value to a `JsValue` for IDB storage via
/// `serde-wasm-bindgen` — IDB stores it via the structured-clone algorithm.
fn to_js_value<T: Serialize>(value: &T) -> Result<JsValue, DataError> {
    serde_wasm_bindgen::to_value(value).map_err(|e| DataError::SerdeError(e.to_string()))
}

/// Convert a `JsValue` returned from IDB to a Rust value, treating
/// undefined/null as `None`.
fn from_js_value_opt<T: DeserializeOwned>(value: &JsValue) -> Result<Option<T>, DataError> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    serde_wasm_bindgen::from_value(value.clone())
        .map(Some)
        .map_err(|e| DataError::SerdeError(e.to_string()))
}

fn deserialize_js_array<T: DeserializeOwned>(array: &Array) -> Vec<T> {
    let mut items = Vec::with_capacity(array.length() as usize);
    for i in 0..array.length() {
        let value = array.get(i);
        if value.is_undefined() || value.is_null() {
            continue;
        }
        match serde_wasm_bindgen::from_value::<T>(value) {
            Ok(item) => items.push(item),
            Err(e) => log::warn!("Skipped malformed scan-index entry: {}", e),
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
