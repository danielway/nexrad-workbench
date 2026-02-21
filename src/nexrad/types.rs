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

    /// Convert to v4 ScanKey format (data::keys::ScanKey).
    pub fn to_v4_key(&self) -> crate::data::ScanKey {
        crate::data::ScanKey::from_legacy(&self.site_id, self.timestamp)
    }

    /// Convert from v4 ScanKey format (data::keys::ScanKey).
    pub fn from_v4_key(key: &crate::data::ScanKey) -> Self {
        Self {
            site_id: key.site.0.clone(),
            timestamp: key.scan_start.as_secs(),
        }
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

/// Lightweight metadata for timeline display (avoids loading full scan data).
///
/// This struct contains only the essential information needed to display
/// scans in the timeline UI, without the heavy scan data payload (~1-5MB).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanMetadata {
    /// Storage key identifying this scan
    pub key: ScanKey,
    /// Original file name from AWS
    pub file_name: String,
    /// File size in bytes
    pub file_size: u64,
    /// End timestamp of the scan (populated when scan is decoded)
    pub end_timestamp: Option<i64>,
    /// Volume Coverage Pattern identifier
    pub vcp: Option<u16>,
    /// Completeness state for this scan.
    pub completeness: Option<crate::data::ScanCompleteness>,
    /// Number of records currently present.
    pub present_records: Option<u32>,
    /// Expected number of records (from VCP).
    pub expected_records: Option<u32>,
    /// Sweep metadata from a previous decode, if available.
    pub sweeps: Option<Vec<crate::data::SweepMeta>>,
}

impl ScanMetadata {
    /// Creates metadata from a cached scan.
    ///
    /// Note: end_timestamp and vcp are set to None here since they require
    /// decoding the scan data. They should be updated when the scan is decoded.
    pub fn from_cached_scan(scan: &CachedScan) -> Self {
        Self {
            key: scan.key.clone(),
            file_name: scan.file_name.clone(),
            file_size: scan.file_size,
            end_timestamp: None,
            vcp: None,
            completeness: None,
            present_records: None,
            expected_records: None,
            sweeps: None,
        }
    }

    /// Creates metadata with decoded information.
    #[allow(dead_code)] // API method for future use when decoding is implemented
    pub fn from_cached_scan_with_info(
        scan: &CachedScan,
        end_timestamp: Option<i64>,
        vcp: Option<u16>,
    ) -> Self {
        Self {
            key: scan.key.clone(),
            file_name: scan.file_name.clone(),
            file_size: scan.file_size,
            end_timestamp,
            vcp,
            completeness: None,
            present_records: None,
            expected_records: None,
            sweeps: None,
        }
    }
}

/// Result of a download operation.
#[derive(Debug, Clone)]
pub enum DownloadResult {
    /// Download completed successfully, with timing info
    Success {
        scan: CachedScan,
        fetch_latency_ms: f64,
        decode_latency_ms: f64,
    },
    /// Download failed with an error message
    Error(String),
    /// Download progress update (current bytes, total bytes)
    #[allow(dead_code)]
    Progress(usize, usize),
    /// Found in cache, no download needed
    CacheHit(CachedScan),
}
