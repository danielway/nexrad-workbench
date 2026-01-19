//! IndexedDB-based storage implementation for WASM targets.
//!
//! This module provides persistent storage using the browser's IndexedDB API.
//! It wraps the low-level web-sys bindings in a Rust-friendly async interface.

use super::{KeyValueStore, StorageConfig, StorageError};
use js_sys::Array;
use serde::{de::DeserializeOwned, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use web_sys::{IdbDatabase, IdbObjectStore, IdbRequest, IdbTransactionMode};

/// IndexedDB-based key-value store.
///
/// This store persists data in the browser's IndexedDB, which can handle
/// large amounts of structured data (suitable for radar data caching).
#[derive(Clone)]
pub struct IndexedDbStore {
    config: StorageConfig,
    db: Rc<RefCell<Option<IdbDatabase>>>,
}

impl IndexedDbStore {
    /// Creates a new IndexedDB store with the given configuration.
    ///
    /// Note: The database is opened lazily on first use.
    pub fn new(config: StorageConfig) -> Self {
        Self {
            config,
            db: Rc::new(RefCell::new(None)),
        }
    }

    /// Opens the database connection if not already open.
    async fn ensure_open(&self) -> Result<(), StorageError> {
        if self.db.borrow().is_some() {
            return Ok(());
        }

        let db = open_database(&self.config).await?;
        *self.db.borrow_mut() = Some(db);
        Ok(())
    }

    /// Gets the database reference, opening it if necessary.
    async fn get_db(&self) -> Result<IdbDatabase, StorageError> {
        self.ensure_open().await?;
        self.db
            .borrow()
            .clone()
            .ok_or_else(|| StorageError::DatabaseOpenFailed("Database not open".to_string()))
    }

    /// Gets an object store for the given transaction mode.
    fn get_object_store(
        &self,
        db: &IdbDatabase,
        mode: IdbTransactionMode,
    ) -> Result<IdbObjectStore, StorageError> {
        let transaction = db
            .transaction_with_str_and_mode(&self.config.store_name, mode)
            .map_err(|e| StorageError::TransactionFailed(format!("{:?}", e)))?;

        transaction
            .object_store(&self.config.store_name)
            .map_err(|e| StorageError::TransactionFailed(format!("{:?}", e)))
    }
}

impl KeyValueStore for IndexedDbStore {
    async fn put<T: Serialize + 'static>(&self, key: &str, value: &T) -> Result<(), StorageError> {
        let db = self.get_db().await?;
        let store = self.get_object_store(&db, IdbTransactionMode::Readwrite)?;

        // Serialize value to JSON string
        let json = serde_json::to_string(value)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        // Create a JS object with key and value
        let js_value = JsValue::from_str(&json);

        let request = store
            .put_with_key(&js_value, &JsValue::from_str(key))
            .map_err(|e| StorageError::TransactionFailed(format!("{:?}", e)))?;

        wait_for_request(&request).await?;
        Ok(())
    }

    async fn get<T: DeserializeOwned + 'static>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StorageError> {
        let db = self.get_db().await?;
        let store = self.get_object_store(&db, IdbTransactionMode::Readonly)?;

        let request = store
            .get(&JsValue::from_str(key))
            .map_err(|e| StorageError::TransactionFailed(format!("{:?}", e)))?;

        let result = wait_for_request(&request).await?;

        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }

        // Result should be a string (JSON)
        let json = result
            .as_string()
            .ok_or_else(|| StorageError::SerializationError("Expected string value".to_string()))?;

        let value = serde_json::from_str(&json)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        Ok(Some(value))
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let db = self.get_db().await?;
        let store = self.get_object_store(&db, IdbTransactionMode::Readwrite)?;

        let request = store
            .delete(&JsValue::from_str(key))
            .map_err(|e| StorageError::TransactionFailed(format!("{:?}", e)))?;

        wait_for_request(&request).await?;
        Ok(())
    }

    async fn get_all_keys(&self) -> Result<Vec<String>, StorageError> {
        let db = self.get_db().await?;
        let store = self.get_object_store(&db, IdbTransactionMode::Readonly)?;

        let request = store
            .get_all_keys()
            .map_err(|e| StorageError::TransactionFailed(format!("{:?}", e)))?;

        let result = wait_for_request(&request).await?;

        // Result is an array of keys
        let array = Array::from(&result);
        let mut keys = Vec::with_capacity(array.length() as usize);

        for i in 0..array.length() {
            if let Some(key) = array.get(i).as_string() {
                keys.push(key);
            }
        }

        Ok(keys)
    }

    async fn clear(&self) -> Result<(), StorageError> {
        let db = self.get_db().await?;
        let store = self.get_object_store(&db, IdbTransactionMode::Readwrite)?;

        let request = store
            .clear()
            .map_err(|e| StorageError::TransactionFailed(format!("{:?}", e)))?;

        wait_for_request(&request).await?;
        Ok(())
    }
}

/// Opens an IndexedDB database with the given configuration.
async fn open_database(config: &StorageConfig) -> Result<IdbDatabase, StorageError> {
    let window = web_sys::window()
        .ok_or_else(|| StorageError::DatabaseOpenFailed("No window object".to_string()))?;

    let idb_factory = window
        .indexed_db()
        .map_err(|e| StorageError::DatabaseOpenFailed(format!("{:?}", e)))?
        .ok_or_else(|| StorageError::DatabaseOpenFailed("IndexedDB not available".to_string()))?;

    let open_request = idb_factory
        .open_with_u32(&config.database_name, config.version)
        .map_err(|e| StorageError::DatabaseOpenFailed(format!("{:?}", e)))?;

    // Set up upgrade handler to create object store if needed
    let store_name = config.store_name.clone();
    let onupgradeneeded = Closure::wrap(Box::new(move |event: web_sys::IdbVersionChangeEvent| {
        let request: IdbRequest = event
            .target()
            .unwrap()
            .dyn_into()
            .expect("Event target should be IdbRequest");

        let db: IdbDatabase = request.result().unwrap().dyn_into().unwrap();

        // Create object store if it doesn't exist
        if !db.object_store_names().contains(&store_name) {
            let params = web_sys::IdbObjectStoreParameters::new();
            // We're using explicit keys, so no keyPath needed
            db.create_object_store_with_optional_parameters(&store_name, &params)
                .expect("Failed to create object store");
            log::info!("Created IndexedDB object store: {}", store_name);
        }
    }) as Box<dyn FnMut(_)>);

    open_request.set_onupgradeneeded(Some(onupgradeneeded.as_ref().unchecked_ref()));
    onupgradeneeded.forget(); // Prevent closure from being dropped

    // Wait for the database to open
    let db_result = wait_for_request(&open_request).await?;

    let db: IdbDatabase = db_result.dyn_into().map_err(|_| {
        StorageError::DatabaseOpenFailed("Failed to cast to IdbDatabase".to_string())
    })?;

    log::info!(
        "Opened IndexedDB database: {} v{}",
        config.database_name,
        config.version
    );

    Ok(db)
}

/// Waits for an IDB request to complete and returns the result.
async fn wait_for_request(request: &IdbRequest) -> Result<JsValue, StorageError> {
    let (tx, rx) = futures_channel::oneshot::channel::<Result<JsValue, StorageError>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let tx_success = tx.clone();
    let onsuccess = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let request: IdbRequest = event
            .target()
            .unwrap()
            .dyn_into()
            .expect("Event target should be IdbRequest");

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
            .expect("Event target should be IdbRequest");

        let error_msg = request
            .error()
            .ok()
            .flatten()
            .map(|e| e.message())
            .unwrap_or_else(|| "Unknown error".to_string());

        if let Some(tx) = tx_error.borrow_mut().take() {
            let _ = tx.send(Err(StorageError::TransactionFailed(error_msg)));
        }
    }) as Box<dyn FnMut(_)>);

    request.set_onsuccess(Some(onsuccess.as_ref().unchecked_ref()));
    request.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    // Keep closures alive until the request completes
    let result = rx
        .await
        .map_err(|_| StorageError::Other("Channel closed".to_string()))?;

    // Clean up event handlers
    request.set_onsuccess(None);
    request.set_onerror(None);

    // Drop closures after request completes
    drop(onsuccess);
    drop(onerror);

    result
}
