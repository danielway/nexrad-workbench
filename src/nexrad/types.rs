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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn cached_scan_new_sets_file_size_from_data_len() {
        let data = vec![0u8, 1, 2, 3, 4, 5, 6];
        let scan = CachedScan::new("KDMX", 1_700_000_000, "file.ar2v".to_string(), data.clone());
        assert_eq!(scan.file_size, 7);
        assert_eq!(scan.data, data);
        assert_eq!(scan.file_name, "file.ar2v");
    }

    #[wasm_bindgen_test]
    fn cached_scan_new_empty_data_has_zero_size() {
        let scan = CachedScan::new("KABR", 0, "empty".to_string(), vec![]);
        assert_eq!(scan.file_size, 0);
        assert!(scan.data.is_empty());
    }

    #[wasm_bindgen_test]
    fn cached_scan_new_key_converts_secs_to_millis() {
        // ScanKey::from_secs multiplies seconds by 1000 into UnixMillis.
        let scan = CachedScan::new("KTLX", 1_700_000_000, "f".to_string(), vec![9]);
        assert_eq!(scan.key.scan_start.0, 1_700_000_000_000);
        assert_eq!(scan.key.site.0, "KTLX");
    }

    #[wasm_bindgen_test]
    fn cached_scan_new_negative_timestamp() {
        let scan = CachedScan::new("KFTG", -5, "f".to_string(), vec![1, 2]);
        assert_eq!(scan.key.scan_start.0, -5_000);
        assert_eq!(scan.file_size, 2);
    }

    #[wasm_bindgen_test]
    fn cached_scan_key_storage_round_trips() {
        let scan = CachedScan::new("KDMX", 1_500_000_000, "n".to_string(), vec![]);
        let storage = scan.key.to_storage_key();
        assert_eq!(storage, "KDMX|1500000000000");
        let parsed = ScanKey::from_storage_key(&storage).unwrap();
        assert_eq!(parsed, scan.key);
    }

    #[wasm_bindgen_test]
    fn cached_scan_serde_encodes_data_as_base64() {
        let scan = CachedScan::new(
            "KDMX",
            1_700_000_000,
            "hello.ar2v".to_string(),
            b"hello".to_vec(),
        );
        let json = serde_json::to_string(&scan).unwrap();
        // base64 STANDARD of "hello" is "aGVsbG8=".
        assert!(json.contains("aGVsbG8="), "json was: {json}");
        // The raw bytes must not appear as a JSON array.
        assert!(
            !json.contains("[104,101"),
            "data should be base64 string, not array: {json}"
        );
    }

    #[wasm_bindgen_test]
    fn cached_scan_serde_round_trips_data_bytes() {
        let data: Vec<u8> = (0u8..=255u8).collect();
        let scan = CachedScan::new("KABX", 1_234_567, "binary".to_string(), data.clone());
        let json = serde_json::to_string(&scan).unwrap();
        let back: CachedScan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.data, data);
        assert_eq!(back.file_size, 256);
        assert_eq!(back.file_name, "binary");
        assert_eq!(back.key, scan.key);
    }

    #[wasm_bindgen_test]
    fn cached_scan_serde_empty_data_round_trips() {
        let scan = CachedScan::new("KCAE", 100, "e".to_string(), vec![]);
        let json = serde_json::to_string(&scan).unwrap();
        // Empty bytes base64-encode to the empty string.
        assert!(json.contains("\"data\":\"\""), "json was: {json}");
        let back: CachedScan = serde_json::from_str(&json).unwrap();
        assert!(back.data.is_empty());
    }

    #[wasm_bindgen_test]
    fn cached_scan_clone_is_independent_copy() {
        let scan = CachedScan::new("KMVX", 42, "orig".to_string(), vec![7, 8, 9]);
        let cloned = scan.clone();
        assert_eq!(cloned.data, scan.data);
        assert_eq!(cloned.file_size, scan.file_size);
        assert_eq!(cloned.key, scan.key);
        assert_eq!(cloned.file_name, scan.file_name);
    }
}
