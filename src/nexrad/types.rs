//! Type definitions for NEXRAD data handling.
//!
//! This module defines wrapper types for nexrad-model data structures
//! that add serialization support and storage key generation.

use serde::{Deserialize, Serialize};

/// Storage key for identifying cached scans.
///
/// Combines site ID and timestamp to create a unique identifier
/// for storing/retrieving scans from IndexedDB.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScanKey {
    /// Four-letter NEXRAD site identifier (e.g., "KDMX")
    pub site_id: String,
    /// Unix timestamp of the scan start
    pub timestamp: i64,
}

impl ScanKey {
    /// Creates a new scan key from site ID and timestamp.
    pub fn new(site_id: impl Into<String>, timestamp: i64) -> Self {
        Self {
            site_id: site_id.into(),
            timestamp,
        }
    }

    /// Generates the storage key string for IndexedDB.
    pub fn to_storage_key(&self) -> String {
        format!("{}_{}", self.site_id, self.timestamp)
    }

    /// Parses a storage key string back into a ScanKey.
    #[allow(dead_code)]
    pub fn from_storage_key(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.splitn(2, '_').collect();
        if parts.len() != 2 {
            return None;
        }
        let timestamp = parts[1].parse().ok()?;
        Some(Self {
            site_id: parts[0].to_string(),
            timestamp,
        })
    }
}

/// A cached NEXRAD scan with metadata.
///
/// Wraps the nexrad_model::Scan with additional metadata needed
/// for storage and retrieval. The scan data is stored as compressed
/// bytes to minimize storage size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedScan {
    /// Storage key identifying this scan
    pub key: ScanKey,
    /// Original file name from AWS
    pub file_name: String,
    /// File size in bytes
    pub file_size: u64,
    /// Compressed scan data (bzip2 compressed Archive2 format)
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
}

impl CachedScan {
    /// Creates a new cached scan from raw data.
    pub fn new(key: ScanKey, file_name: String, data: Vec<u8>) -> Self {
        Self {
            key,
            file_size: data.len() as u64,
            file_name,
            data,
        }
    }
}

/// Serde helper module for base64 encoding/decoding of byte vectors.
mod base64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

/// Result of a download operation.
#[derive(Debug, Clone)]
pub enum DownloadResult {
    /// Download completed successfully
    Success(CachedScan),
    /// Download failed with an error message
    Error(String),
    /// Download progress update (current bytes, total bytes)
    #[allow(dead_code)]
    Progress(usize, usize),
    /// Found in cache, no download needed
    CacheHit(CachedScan),
}
