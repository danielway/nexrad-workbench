//! Generic storage abstraction for persistent data.
//!
//! This module provides a platform-agnostic interface for key-value storage.
//! On WASM targets, it uses IndexedDB for persistence. On native targets,
//! it provides a no-op implementation (can be extended to use filesystem storage).

#[cfg(target_arch = "wasm32")]
mod indexeddb;

#[cfg(target_arch = "wasm32")]
pub use indexeddb::IndexedDbStore;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::future::Future;

/// A cached file with metadata.
///
/// This struct is used to persist uploaded files in the browser's IndexedDB.
/// The data is stored as a base64-encoded string for JSON serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFile {
    /// Original file name
    pub file_name: String,
    /// File size in bytes
    pub file_size: u64,
    /// File data encoded as base64
    pub data_base64: String,
}

impl CachedFile {
    /// Creates a new cached file from raw bytes.
    pub fn new(file_name: String, file_data: &[u8]) -> Self {
        use base64::{engine::general_purpose::STANDARD, Engine};
        Self {
            file_name,
            file_size: file_data.len() as u64,
            data_base64: STANDARD.encode(file_data),
        }
    }

    /// Decodes the file data from base64.
    #[allow(dead_code)] // Part of storage API
    pub fn decode_data(&self) -> Result<Vec<u8>, base64::DecodeError> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        STANDARD.decode(&self.data_base64)
    }
}

/// Errors that can occur during storage operations.
#[derive(Debug, Clone)]
#[allow(dead_code)] // NotFound variant is part of storage API
pub enum StorageError {
    /// The database could not be opened or initialized.
    DatabaseOpenFailed(String),
    /// A transaction failed to complete.
    TransactionFailed(String),
    /// Serialization or deserialization failed.
    SerializationError(String),
    /// The requested key was not found.
    NotFound,
    /// An unexpected error occurred.
    Other(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::DatabaseOpenFailed(msg) => write!(f, "Database open failed: {}", msg),
            StorageError::TransactionFailed(msg) => write!(f, "Transaction failed: {}", msg),
            StorageError::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            StorageError::NotFound => write!(f, "Key not found"),
            StorageError::Other(msg) => write!(f, "Storage error: {}", msg),
        }
    }
}

impl std::error::Error for StorageError {}

/// A generic key-value storage interface.
///
/// This trait defines the operations available for persistent storage.
/// Implementations can use different backends (IndexedDB, localStorage,
/// filesystem, etc.) while providing a consistent API.
///
/// Note: This trait does not require `Send` bounds since WASM is single-threaded
/// and JS types cannot be sent between threads.
#[allow(dead_code)] // Trait methods are part of storage API, only `put` currently used
pub trait KeyValueStore {
    /// Stores a value under the given key.
    ///
    /// If a value already exists for the key, it will be overwritten.
    fn put<T: Serialize + 'static>(
        &self,
        key: &str,
        value: &T,
    ) -> impl Future<Output = Result<(), StorageError>>;

    /// Retrieves a value by key.
    ///
    /// Returns `Ok(None)` if the key doesn't exist.
    fn get<T: DeserializeOwned + 'static>(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<T>, StorageError>>;

    /// Deletes a value by key.
    ///
    /// Returns `Ok(())` even if the key didn't exist.
    fn delete(&self, key: &str) -> impl Future<Output = Result<(), StorageError>>;

    /// Retrieves all keys in the store.
    fn get_all_keys(&self) -> impl Future<Output = Result<Vec<String>, StorageError>>;

    /// Removes all entries from the store.
    fn clear(&self) -> impl Future<Output = Result<(), StorageError>>;
}

/// Configuration for creating a storage instance.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Name of the database (used as IndexedDB database name on web).
    pub database_name: String,
    /// Name of the object store within the database.
    pub store_name: String,
    /// Database version (incrementing triggers upgrade).
    /// Note: On WASM, this is overridden by DATABASE_VERSION in indexeddb.rs
    /// to ensure all stores use the same version.
    #[allow(dead_code)] // Used for documentation; actual version from DATABASE_VERSION
    pub version: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database_name: "nexrad-workbench".to_string(),
            store_name: "cache".to_string(),
            version: 3, // Schema v3: nexrad-scans, scan-metadata, file-cache
        }
    }
}

impl StorageConfig {
    /// Creates a new configuration with the given database and store names.
    pub fn new(database_name: impl Into<String>, store_name: impl Into<String>) -> Self {
        Self {
            database_name: database_name.into(),
            store_name: store_name.into(),
            version: 3, // Schema v3: nexrad-scans, scan-metadata, file-cache
        }
    }
}

// Native stub implementation for development/testing
#[cfg(not(target_arch = "wasm32"))]
pub mod native {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    /// A simple in-memory store for native builds.
    ///
    /// This is primarily for development and testing. Data is not persisted
    /// across application restarts.
    #[derive(Clone, Default)]
    pub struct MemoryStore {
        data: Arc<RwLock<HashMap<String, String>>>,
    }

    impl MemoryStore {
        pub fn new(_config: StorageConfig) -> Self {
            Self {
                data: Arc::new(RwLock::new(HashMap::new())),
            }
        }
    }

    impl KeyValueStore for MemoryStore {
        async fn put<T: Serialize + 'static>(
            &self,
            key: &str,
            value: &T,
        ) -> Result<(), StorageError> {
            let json = serde_json::to_string(value)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?;
            self.data
                .write()
                .map_err(|e| StorageError::Other(e.to_string()))?
                .insert(key.to_string(), json);
            Ok(())
        }

        async fn get<T: DeserializeOwned + 'static>(
            &self,
            key: &str,
        ) -> Result<Option<T>, StorageError> {
            let data = self
                .data
                .read()
                .map_err(|e| StorageError::Other(e.to_string()))?;
            match data.get(key) {
                Some(json) => {
                    let value = serde_json::from_str(json)
                        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
                    Ok(Some(value))
                }
                None => Ok(None),
            }
        }

        async fn delete(&self, key: &str) -> Result<(), StorageError> {
            self.data
                .write()
                .map_err(|e| StorageError::Other(e.to_string()))?
                .remove(key);
            Ok(())
        }

        async fn get_all_keys(&self) -> Result<Vec<String>, StorageError> {
            let data = self
                .data
                .read()
                .map_err(|e| StorageError::Other(e.to_string()))?;
            Ok(data.keys().cloned().collect())
        }

        async fn clear(&self) -> Result<(), StorageError> {
            self.data
                .write()
                .map_err(|e| StorageError::Other(e.to_string()))?
                .clear();
            Ok(())
        }
    }
}
