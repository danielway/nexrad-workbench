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
//! 3. `scan_touches` - Per-scan last-access timestamp (f64 unix ms) for LRU
//!    eviction; separate store so touch bumps don't race ingest's
//!    read-modify-write of the index entry
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
//! - `upsert_scan` does a readonly `scan_availability` lookup before its
//!   readwrite transaction to decide create-vs-merge. The split is *not*
//!   atomic across async awaits, so the function acquires an
//!   [`UpsertScanGuard`] at entry and returns
//!   [`DataError::ConcurrentUpsert`] when a second task tries to upsert
//!   the same scan in parallel. The ingest pipeline already serializes
//!   via the per-worker `CHUNK_ACCUM` thread-local; the guard is the
//!   type-system backstop.

use crate::data::keys::*;
use js_sys::{Array, ArrayBuffer, Uint8Array};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{IdbDatabase, IdbObjectStore, IdbRequest, IdbTransactionMode};

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
    /// Another async task is already performing an upsert for this scan.
    /// The non-atomic read-then-write split in `upsert_scan` requires
    /// per-scan serialization; this is raised when that contract is
    /// violated rather than letting the writes silently race.
    ConcurrentUpsert { scan_key: String },
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
            DataError::ConcurrentUpsert { scan_key } => {
                write!(f, "Concurrent upsert in flight for scan {}", scan_key)
            }
        }
    }
}

/// Coarse classification of a [`DataError`] for callers that need to pick
/// retry / give-up / evict behavior without matching every variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Likely to succeed on retry: open still in flight, an aborted or
    /// timed-out transaction, or losing the per-scan upsert race.
    Transient,
    /// Will not succeed without a code or data change: missing key,
    /// corrupt/unreadable entry, schema mismatch.
    Permanent,
    /// Browser storage pressure — succeeds only after eviction frees space.
    Quota,
}

impl DataError {
    /// Classify this error. IDB transaction/request failures are classified
    /// by the DOMException name that [`js_err`] places at the front of the
    /// formatted message.
    pub fn kind(&self) -> ErrorKind {
        match self {
            DataError::NotOpen | DataError::ConcurrentUpsert { .. } => ErrorKind::Transient,
            DataError::QuotaExceeded { .. } => ErrorKind::Quota,
            DataError::NotFound | DataError::SerdeError(_) => ErrorKind::Permanent,
            DataError::TransactionFailed(msg) | DataError::RequestFailed(msg) => {
                classify_js_error(msg)
            }
        }
    }
}

/// Classify a `js_err`-formatted message by its leading DOMException name
/// (`js_err` emits `"Name: message"` when the JS error carried a name).
/// Unknown names default to `Permanent` — better to surface a real failure
/// than to retry blindly.
fn classify_js_error(msg: &str) -> ErrorKind {
    let name = msg.split(':').next().unwrap_or("").trim();
    match name {
        "QuotaExceededError" => ErrorKind::Quota,
        "AbortError" | "TimeoutError" | "TransactionInactiveError" | "UnknownError" => {
            ErrorKind::Transient
        }
        _ => ErrorKind::Permanent,
    }
}

/// RAII token enforcing the single-writer invariant on
/// [`IndexedDbStore::upsert_scan`]. The store's read-then-write split is
/// not atomic across its two IDB transactions, so two concurrent async
/// tasks writing the same scan would race; this token rejects the second
/// caller via [`DataError::ConcurrentUpsert`] instead of corrupting
/// `scan_availability`.
///
/// Acquire via [`UpsertScanGuard::try_acquire`]; drop releases the lock.
/// The lock is per-scan-key and lives in a thread-local (WASM is
/// single-threaded; the only contention is between concurrently-running
/// async futures on the same worker).
pub struct UpsertScanGuard {
    key: String,
}

impl UpsertScanGuard {
    /// Try to acquire the lock for `scan`. Returns `None` if another
    /// task is mid-upsert for the same scan; the caller should propagate
    /// [`DataError::ConcurrentUpsert`] in that case.
    pub fn try_acquire(scan: &crate::data::ScanKey) -> Option<Self> {
        let key = scan.to_storage_key();
        UPSERT_LOCKS.with(|locks| {
            if !locks.borrow_mut().insert(key.clone()) {
                None
            } else {
                Some(Self { key })
            }
        })
    }
}

impl Drop for UpsertScanGuard {
    fn drop(&mut self) {
        UPSERT_LOCKS.with(|locks| {
            locks.borrow_mut().remove(&self.key);
        });
    }
}

thread_local! {
    static UPSERT_LOCKS: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Format a `JsValue` error into a string. Tries to extract `name`/`message`
/// when the JsValue is a DOMException-like object; falls back to `{:?}`.
pub(super) fn js_err(e: JsValue) -> String {
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
pub(super) const DATABASE_VERSION: u32 = 5;

/// Database name.
const DATABASE_NAME: &str = "nexrad-workbench";

/// Object store names.
pub(super) const STORE_SWEEPS: &str = "sweeps";
pub(super) const STORE_SCAN_INDEX: &str = "scan_index";
/// Per-scan last-access timestamps for LRU eviction. Lives in its own
/// store so that fire-and-forget access bumps from `get_sweep` don't
/// race with chunk-ingest's read-modify-write of the scan-index entry.
pub(super) const STORE_SCAN_TOUCHES: &str = "scan_touches";

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
    /// Database name. Production uses `DATABASE_NAME` (`"nexrad-workbench"`);
    /// integration tests use unique per-test names so each test starts fresh.
    database_name: String,
}

impl Default for IndexedDbStore {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexedDbStore {
    /// Construct a store backed by the production `nexrad-workbench` IDB.
    pub fn new() -> Self {
        Self::with_database_name(DATABASE_NAME.to_string())
    }

    /// Construct a store backed by a custom-named IDB. Used by integration
    /// tests to keep each test's state isolated from prod and from sibling
    /// tests in the same browser tab.
    pub fn with_database_name(database_name: String) -> Self {
        Self {
            state: Rc::new(RefCell::new(OpenState::Closed)),
            recent_touches: Rc::new(RefCell::new(HashMap::new())),
            database_name,
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
                let result = open_database(&self.database_name).await;
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

    /// Pre-write browser-quota check for a sweep-blob batch. Awaits the
    /// browser's storage estimate, then defers to `logic::decide_quota`
    /// for the actual decision (tested in `mod logic`).
    async fn check_quota(sweep_blobs: &[(String, Vec<u8>)]) -> Result<(), DataError> {
        let batch_size: u64 = sweep_blobs.iter().map(|(_, data)| data.len() as u64).sum();
        let estimate = if batch_size > 0 {
            estimate_browser_quota().await
        } else {
            None
        };
        logic::decide_quota(batch_size, estimate)
    }

    /// Atomically writes blobs + scan-index entry for a scan, creating the
    /// entry on first call and merging on subsequent calls.
    ///
    /// Dispatches internally on `scan_availability`:
    ///
    /// - **First write** (no existing entry): derive a fresh `ScanIndexEntry`
    ///   from `header` and the uploads, then write blobs + index + an initial
    ///   `scan_touches=now` in one cross-store readwrite transaction. The
    ///   initial touch is what gives the scan its place in the LRU order.
    /// - **Merge** (entry exists): fill in `header.vcp` / `header.file_name`
    ///   only if currently `None`, append derived `CachedSweep`s to
    ///   `existing.cached_sweeps`, and add to `existing.total_size_bytes`.
    ///   `scan_touches` is preserved so a chunk-ingest flush doesn't refresh
    ///   the access timestamp on every partial write.
    ///
    /// `ElevationUpload`s with empty `blobs` are dropped silently — the
    /// manifest is derived from the blobs that are actually being written, so
    /// no `CachedSweep` can claim a sweep that doesn't exist in storage.
    ///
    /// Empty `elevations` is permitted: the entry is written (or its header
    /// fields merged) without any blob writes.
    ///
    /// Read-modify-write is *not* atomic across the readonly and readwrite
    /// transactions, so callers must serialize on the per-scan key. This
    /// is enforced at runtime by an [`UpsertScanGuard`] acquired here:
    /// two concurrent async tasks targeting the same scan are rejected
    /// with [`DataError::ConcurrentUpsert`] rather than allowed to race.
    /// (The ingest pipeline naturally serializes already via the
    /// per-worker `CHUNK_ACCUM` thread-local; the guard is the
    /// type-system backstop.)
    pub async fn upsert_scan(
        &self,
        header: &ScanHeader,
        elevations: &[ElevationUpload],
    ) -> Result<(), DataError> {
        let _guard = UpsertScanGuard::try_acquire(&header.scan).ok_or_else(|| {
            DataError::ConcurrentUpsert {
                scan_key: header.scan.to_storage_key(),
            }
        })?;

        // Drop phantom elevations at the boundary. Everything downstream
        // (manifest derivation, blob writes, size accounting) operates on
        // the filtered list, so the manifest can never claim a sweep that
        // wasn't written.
        let kept: Vec<&ElevationUpload> =
            elevations.iter().filter(|e| !e.blobs.is_empty()).collect();

        // Materialize the blob (key, bytes) pairs once so we can both
        // size-check and write them without redundant key formatting.
        let sweep_blobs: Vec<(String, Vec<u8>)> = kept
            .iter()
            .flat_map(|elev| {
                elev.blobs.iter().map(move |blob| {
                    let key =
                        SweepDataKey::new(header.scan.clone(), elev.elevation_number, blob.product);
                    (key.to_storage_key(), blob.bytes.clone())
                })
            })
            .collect();

        Self::check_quota(&sweep_blobs).await?;
        self.ensure_open().await?;

        let entry_key = header.scan.to_storage_key();
        let existing = self.scan_availability(&header.scan).await?;
        let batch_bytes: u64 = sweep_blobs.iter().map(|(_, b)| b.len() as u64).sum();

        let (entry, seed_touch) = match existing {
            Some(mut existing) => {
                if existing.vcp.is_none() {
                    existing.vcp = header.vcp.clone();
                }
                if existing.file_name.is_none() {
                    existing.file_name = header.file_name.clone();
                }
                existing
                    .cached_sweeps
                    .extend(kept.iter().map(|e| e.to_cached_sweep()));
                existing.total_size_bytes += batch_bytes;
                (existing, false)
            }
            None => {
                let cached_sweeps = kept.iter().map(|e| e.to_cached_sweep()).collect();
                let entry = ScanIndexEntry {
                    scan: header.scan.clone(),
                    vcp: header.vcp.clone(),
                    file_name: header.file_name.clone(),
                    cached_sweeps,
                    total_size_bytes: batch_bytes,
                };
                (entry, true)
            }
        };

        let entry_value = to_js_value(&entry)?;
        let touch_value = if seed_touch {
            Some(JsValue::from_f64(UnixMillis::now().0 as f64))
        } else {
            None
        };

        let stores: &[&str] = if seed_touch {
            &[STORE_SWEEPS, STORE_SCAN_INDEX, STORE_SCAN_TOUCHES]
        } else {
            &[STORE_SWEEPS, STORE_SCAN_INDEX]
        };

        self.write_tx(stores, |wtx| {
            let sweeps = wtx.object_store(STORE_SWEEPS)?;
            for (key, data) in &sweep_blobs {
                let array = Uint8Array::from(data.as_slice());
                let buffer = array.buffer();
                sweeps
                    .put_with_key(&buffer, &JsValue::from_str(key))
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
            }
            wtx.object_store(STORE_SCAN_INDEX)?
                .put_with_key(&entry_value, &JsValue::from_str(&entry_key))
                .map_err(|e| DataError::RequestFailed(js_err(e)))?;
            if let Some(ref touch) = touch_value {
                wtx.object_store(STORE_SCAN_TOUCHES)?
                    .put_with_key(touch, &JsValue::from_str(&entry_key))
                    .map_err(|e| DataError::RequestFailed(js_err(e)))?;
            }
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
            let last = touches.get(scan).copied();
            if logic::should_skip_touch(now, last, TOUCH_THROTTLE_MS) {
                return;
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

    /// Reads the `scan_touches` timestamp for a single scan, if present.
    /// Exposed for integration tests that verify the touch contract;
    /// production code uses `read_all_touches` via `evict_to_size`.
    ///
    /// `allow(dead_code)`: only used from `tests/idb.rs`, which is a
    /// separate crate from the bin; the bin's compilation unit doesn't
    /// see those callers.
    #[doc(hidden)]
    #[allow(dead_code)]
    pub async fn read_touch(&self, scan: &ScanKey) -> Result<Option<UnixMillis>, DataError> {
        self.ensure_open().await?;
        let storage_key = scan.to_storage_key();
        let result = self
            .read(STORE_SCAN_TOUCHES, |store| {
                store.get(&JsValue::from_str(&storage_key))
            })
            .await?;
        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }
        Ok(result.as_f64().map(|ms| UnixMillis(ms as i64)))
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
            let scan = match ScanKey::from_storage_key(&key_str) {
                Ok(scan) => scan,
                Err(e) => {
                    log::warn!("Skipping unparseable scan_touches key {key_str:?}: {e}");
                    continue;
                }
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
        Ok(logic::filter_scans_by_time_window(entries, start, end))
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
    /// Production code should usually evict via [`Self::evict_to_size`];
    /// this method is exposed publicly so integration tests can verify
    /// per-scan deletion semantics directly.
    pub async fn delete_scan(&self, scan: &ScanKey) -> Result<u64, DataError> {
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
    /// Sort key is the `scan_touches` timestamp. `upsert_scan` seeds a
    /// touch on the first write for a given scan key, and `touch_scan`
    /// (fired by `get_sweep`) refreshes it on render. Entries with no touch
    /// — anomalous, but possible on a fresh upgrade or after a corrupted
    /// commit — sort to position 0 and are evicted first.
    ///
    /// Reads all entries + touches once, sorts, and deletes one scan at a
    /// time until the running size estimate drops under target. Each delete
    /// is its own transaction (so a failure mid-eviction still leaves a
    /// consistent store).
    pub async fn evict_to_size(&self, target_bytes: u64) -> Result<u32, DataError> {
        self.ensure_open().await?;

        let entries = self.read_all_scan_entries().await?;
        let mut current_size: u64 = entries.iter().map(|e| e.total_size_bytes).sum();
        if current_size <= target_bytes {
            return Ok(0);
        }

        let touches = self.read_all_touches().await?;
        let order = logic::eviction_order(&entries, &touches);

        let mut evicted_count = 0u32;
        for scan in &order {
            if current_size <= target_bytes {
                break;
            }
            let bytes_freed = self.delete_scan(scan).await?;
            current_size = current_size.saturating_sub(bytes_freed);
            evicted_count += 1;
            log::debug!(
                "Evicted scan {} (freed {} bytes, {} remaining)",
                scan,
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

mod helpers;
mod logic;
pub use helpers::WriteTransaction;
use helpers::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{ScanKey, SiteId, UnixMillis};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn key(site: &str, ms: i64) -> ScanKey {
        ScanKey::new(SiteId::new(site), UnixMillis(ms))
    }

    #[wasm_bindgen_test]
    fn upsert_guard_blocks_concurrent_acquisition_for_same_scan() {
        let scan = key("KDMX", 1_700_000_000_000);
        let g1 = UpsertScanGuard::try_acquire(&scan).expect("first acquire succeeds");
        assert!(
            UpsertScanGuard::try_acquire(&scan).is_none(),
            "second concurrent acquire must fail"
        );
        drop(g1);
        // Once the first guard is dropped, the lock is released and a
        // fresh acquire is allowed again.
        assert!(UpsertScanGuard::try_acquire(&scan).is_some());
    }

    #[wasm_bindgen_test]
    fn upsert_guard_independent_per_scan() {
        let scan_a = key("KDMX", 1_700_000_000_000);
        let scan_b = key("KFTG", 1_700_000_000_000);
        let _ga = UpsertScanGuard::try_acquire(&scan_a).expect("acquire A");
        let _gb =
            UpsertScanGuard::try_acquire(&scan_b).expect("acquire B independent of A succeeds");
    }

    #[wasm_bindgen_test]
    fn error_kind_classifies_structured_variants() {
        assert_eq!(DataError::NotOpen.kind(), ErrorKind::Transient);
        assert_eq!(
            DataError::ConcurrentUpsert {
                scan_key: "KDMX|0".into()
            }
            .kind(),
            ErrorKind::Transient
        );
        assert_eq!(
            DataError::QuotaExceeded {
                available_mb: 1.0,
                required_mb: 2.0
            }
            .kind(),
            ErrorKind::Quota
        );
        assert_eq!(DataError::NotFound.kind(), ErrorKind::Permanent);
        assert_eq!(
            DataError::SerdeError("bad".into()).kind(),
            ErrorKind::Permanent
        );
    }

    #[wasm_bindgen_test]
    fn error_kind_classifies_domexception_names_in_js_messages() {
        // `js_err` formats DOMException-like values as "Name: message".
        let cases = [
            ("QuotaExceededError: out of space", ErrorKind::Quota),
            ("AbortError: transaction aborted", ErrorKind::Transient),
            ("TimeoutError: took too long", ErrorKind::Transient),
            ("TransactionInactiveError: too late", ErrorKind::Transient),
            ("UnknownError: disk hiccup", ErrorKind::Transient),
            ("DataError: bad key", ErrorKind::Permanent),
            ("free-form message with no name", ErrorKind::Permanent),
        ];
        for (msg, expected) in cases {
            assert_eq!(
                DataError::TransactionFailed(msg.to_string()).kind(),
                expected,
                "TransactionFailed({msg:?})"
            );
            assert_eq!(
                DataError::RequestFailed(msg.to_string()).kind(),
                expected,
                "RequestFailed({msg:?})"
            );
        }
    }
}
