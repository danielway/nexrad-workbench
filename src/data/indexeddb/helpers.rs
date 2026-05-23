//! Low-level IDB helpers and the `WriteTransaction` wrapper.
//!
//! These functions sit between the `web_sys` IDB bindings and the
//! `IndexedDbStore` methods in `mod.rs`. They are split out so the
//! orchestration code stays focused on storage policy (eviction, quota
//! checks, key construction) without low-level promise/cursor plumbing.

use super::{
    js_err, logic, DataError, ScanKey, SiteId, StorageQuotaEstimate, DATABASE_VERSION,
    STORE_SCAN_INDEX, STORE_SCAN_TOUCHES, STORE_SWEEPS,
};
use js_sys::Array;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{IdbDatabase, IdbKeyRange, IdbObjectStore, IdbRequest, IdbTransaction};

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
    pub(super) fn new(tx: &'a IdbTransaction) -> Self {
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

/// Wraps `logic::site_prefix_bounds` in an `IdbKeyRange` for actual
/// IDB queries. The bound math is tested in `mod logic`.
pub(super) fn site_prefix_range(site: &SiteId) -> Result<IdbKeyRange, DataError> {
    let (lower, upper) = logic::site_prefix_bounds(site);
    IdbKeyRange::bound(&JsValue::from_str(&lower), &JsValue::from_str(&upper))
        .map_err(|e| DataError::RequestFailed(js_err(e)))
}

/// Wraps `logic::scan_prefix_bounds` in an `IdbKeyRange`.
pub(super) fn scan_prefix_range(scan: &ScanKey) -> Result<IdbKeyRange, DataError> {
    let (lower, upper) = logic::scan_prefix_bounds(scan);
    IdbKeyRange::bound(&JsValue::from_str(&lower), &JsValue::from_str(&upper))
        .map_err(|e| DataError::RequestFailed(js_err(e)))
}

// ============================================================================
// Helper functions
// ============================================================================

/// Gets the IdbFactory from the current global scope (works in both Window and Worker).
pub(super) fn get_idb_factory() -> Result<web_sys::IdbFactory, DataError> {
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
pub(super) async fn open_database(database_name: &str) -> Result<IdbDatabase, DataError> {
    let idb_factory = get_idb_factory()?;

    let open_request = idb_factory
        .open_with_u32(database_name, DATABASE_VERSION)
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

    log::info!("Opened IndexedDB {} v{}", database_name, DATABASE_VERSION);

    Ok(db)
}

/// Waits for an IDB request to complete.
pub(super) async fn wait_for_request(request: &IdbRequest) -> Result<JsValue, DataError> {
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
pub(super) async fn wait_for_transaction(tx: &IdbTransaction) -> Result<(), DataError> {
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
pub(super) fn to_js_value<T: Serialize>(value: &T) -> Result<JsValue, DataError> {
    serde_wasm_bindgen::to_value(value).map_err(|e| DataError::SerdeError(e.to_string()))
}

/// Convert a `JsValue` returned from IDB to a Rust value, treating
/// undefined/null as `None`.
pub(super) fn from_js_value_opt<T: DeserializeOwned>(
    value: &JsValue,
) -> Result<Option<T>, DataError> {
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    serde_wasm_bindgen::from_value(value.clone())
        .map(Some)
        .map_err(|e| DataError::SerdeError(e.to_string()))
}

pub(super) fn deserialize_js_array<T: DeserializeOwned>(array: &Array) -> Vec<T> {
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
pub(super) async fn estimate_browser_quota() -> Option<StorageQuotaEstimate> {
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

// ============================================================================
// Pure decision logic
// ============================================================================
//
// The functions in this module describe *what* the IDB layer decides without
// touching `web_sys`/`js_sys`. Tests below run in pure Rust on `wasm32+node`
// via `wasm-bindgen-test`, so the consistency-critical math (key range
// bounds, eviction order, throttle decisions, quota math, time-window
// filter) gets exercised on every `cargo test` run.
//
// The wasm-only orchestration that calls these helpers (transactions,
// `JsValue` marshaling, real IDB requests) is exercised by a separate
// browser-driven test suite (`tests/idb.rs`, deferred — runs in CI).
