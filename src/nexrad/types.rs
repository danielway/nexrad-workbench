//! Type definitions for the NEXRAD download pipeline.

use crate::data::ScanKey;
use serde::{Deserialize, Serialize};

/// A cached NEXRAD scan with metadata.
///
/// Wraps the downloaded scan data with metadata for the download pipeline.
/// The scan data is the raw archive bytes before worker processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedScan {
    /// Storage key identifying this scan
    pub key: ScanKey,
    /// Original file name from AWS
    pub file_name: String,
    /// File size in bytes
    pub file_size: u64,
    /// Raw archive data (bzip2 compressed Archive2 format)
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
}

impl CachedScan {
    /// Creates a new cached scan from raw data.
    pub fn new(site_id: &str, timestamp_secs: i64, file_name: String, data: Vec<u8>) -> Self {
        Self {
            key: ScanKey::from_secs(site_id, timestamp_secs),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanMetadata {
    /// Storage key identifying this scan
    pub key: ScanKey,
    /// Original file name from AWS
    pub file_name: String,
    /// File size in bytes
    pub file_size: u64,
    /// End timestamp of the scan in seconds (populated when scan is decoded)
    pub end_timestamp: Option<i64>,
    /// Full Volume Coverage Pattern extracted from scan data.
    pub vcp: Option<crate::data::keys::ExtractedVcp>,
    /// Completeness state for this scan.
    pub completeness: Option<crate::data::ScanCompleteness>,
    /// Number of records currently present.
    pub present_records: Option<u32>,
    /// Expected number of records (from VCP).
    pub expected_records: Option<u32>,
    /// Sweep metadata from a previous decode, if available.
    pub sweeps: Option<Vec<crate::data::SweepMeta>>,
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
    /// Download failed.
    ///
    /// `scan_start` is the timestamp (Unix seconds) of the scan the failed
    /// download was attempting. With parallel downloads this is essential to
    /// correlate the failure with the right queue entry.
    Error { message: String, scan_start: i64 },
    /// Found in cache, no download needed
    CacheHit(CachedScan),
}
