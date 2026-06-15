//! Storage settings for cache management.
//!
//! Settings are persisted to localStorage so they survive page reloads.

use serde::{Deserialize, Serialize};

/// Storage quota and eviction settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSettings {
    /// Maximum cache size in bytes before eviction triggers.
    pub quota_bytes: u64,
    /// Target size after eviction (typically 80% of quota).
    pub eviction_target_bytes: u64,
}

impl Default for StorageSettings {
    fn default() -> Self {
        let mut s = Self {
            quota_bytes: 0,
            eviction_target_bytes: 0,
        };
        s.set_quota(2 * 1024 * 1024 * 1024); // 2 GB
        s
    }
}

impl StorageSettings {
    /// localStorage key for persisting settings.
    const STORAGE_KEY: &'static str = "nexrad_storage_settings";

    /// Sets the quota and derives the eviction target from
    /// `QuotaPolicy::eviction_target_fraction`.
    pub fn set_quota(&mut self, quota_bytes: u64) {
        self.quota_bytes = quota_bytes;
        self.eviction_target_bytes = (quota_bytes as f64
            * crate::data::quota::QuotaPolicy::DEFAULT.eviction_target_fraction)
            as u64;
    }

    /// Load settings from localStorage.
    pub fn load() -> Self {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return Self::default(),
        };

        let storage = match window.local_storage() {
            Ok(Some(s)) => s,
            _ => return Self::default(),
        };

        let json = match storage.get_item(Self::STORAGE_KEY) {
            Ok(Some(s)) => s,
            _ => return Self::default(),
        };

        match serde_json::from_str(&json) {
            Ok(settings) => {
                log::debug!("Loaded storage settings from localStorage");
                settings
            }
            Err(e) => {
                log::warn!("Failed to parse storage settings: {}", e);
                Self::default()
            }
        }
    }

    /// Save settings to localStorage.
    pub fn save(&self) {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return,
        };

        let storage = match window.local_storage() {
            Ok(Some(s)) => s,
            _ => return,
        };

        let json = match serde_json::to_string(self) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Failed to serialize storage settings: {}", e);
                return;
            }
        };

        if let Err(e) = storage.set_item(Self::STORAGE_KEY, &json) {
            log::warn!("Failed to save storage settings: {:?}", e);
        } else {
            log::debug!("Saved storage settings to localStorage");
        }
    }

    /// Returns minimum quota (100 MB).
    pub fn min_quota() -> u64 {
        100 * 1024 * 1024
    }

    /// Returns maximum quota (20 GB).
    pub fn max_quota() -> u64 {
        20 * 1024 * 1024 * 1024
    }
}

/// Format bytes as human-readable string.
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn default_quota_is_2gb() {
        let s = StorageSettings::default();
        assert_eq!(s.quota_bytes, 2 * 1024 * 1024 * 1024);
    }

    #[wasm_bindgen_test]
    fn default_eviction_target_is_80_percent() {
        let s = StorageSettings::default();
        // 2147483648 * 0.80 = 1717986918.4 -> truncates to 1717986918
        assert_eq!(s.eviction_target_bytes, 1_717_986_918);
    }

    #[wasm_bindgen_test]
    fn set_quota_derives_target_from_fraction() {
        let mut s = StorageSettings::default();
        s.set_quota(1000);
        assert_eq!(s.quota_bytes, 1000);
        // 1000 * 0.80 = 800
        assert_eq!(s.eviction_target_bytes, 800);
    }

    #[wasm_bindgen_test]
    fn set_quota_zero_yields_zero_target() {
        let mut s = StorageSettings::default();
        s.set_quota(0);
        assert_eq!(s.quota_bytes, 0);
        assert_eq!(s.eviction_target_bytes, 0);
    }

    #[wasm_bindgen_test]
    fn set_quota_overwrites_previous() {
        let mut s = StorageSettings::default();
        s.set_quota(1000);
        s.set_quota(50);
        assert_eq!(s.quota_bytes, 50);
        // 50 * 0.80 = 40
        assert_eq!(s.eviction_target_bytes, 40);
    }

    #[wasm_bindgen_test]
    fn min_quota_is_100mb() {
        assert_eq!(StorageSettings::min_quota(), 100 * 1024 * 1024);
        assert_eq!(StorageSettings::min_quota(), 104_857_600);
    }

    #[wasm_bindgen_test]
    fn max_quota_is_20gb() {
        assert_eq!(StorageSettings::max_quota(), 20 * 1024 * 1024 * 1024);
        assert_eq!(StorageSettings::max_quota(), 21_474_836_480);
    }

    #[wasm_bindgen_test]
    fn serde_round_trip_preserves_fields() {
        let mut s = StorageSettings::default();
        s.set_quota(12345);
        let json = serde_json::to_string(&s).expect("serialize");
        let back: StorageSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.quota_bytes, s.quota_bytes);
        assert_eq!(back.eviction_target_bytes, s.eviction_target_bytes);
    }

    #[wasm_bindgen_test]
    fn format_bytes_gb_one_decimal() {
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[wasm_bindgen_test]
    fn format_bytes_mb_no_decimal() {
        assert_eq!(format_bytes(5 * 1024 * 1024), "5 MB");
        // exact 1 MiB boundary
        assert_eq!(format_bytes(1024 * 1024), "1 MB");
    }

    #[wasm_bindgen_test]
    fn format_bytes_kb_no_decimal() {
        assert_eq!(format_bytes(3 * 1024), "3 KB");
        // exact 1 KiB boundary
        assert_eq!(format_bytes(1024), "1 KB");
    }

    #[wasm_bindgen_test]
    fn format_bytes_raw_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
    }
}
