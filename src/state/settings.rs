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
        Self {
            quota_bytes: 500 * 1024 * 1024,           // 500 MB
            eviction_target_bytes: 400 * 1024 * 1024, // 400 MB (80% of quota)
        }
    }
}

impl StorageSettings {
    /// localStorage key for persisting settings.
    const STORAGE_KEY: &'static str = "nexrad_storage_settings";

    /// Creates new storage settings with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the quota and automatically calculates eviction target (80% of quota).
    pub fn set_quota(&mut self, quota_bytes: u64) {
        self.quota_bytes = quota_bytes;
        self.eviction_target_bytes = (quota_bytes as f64 * 0.8) as u64;
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
                log::info!("Loaded storage settings from localStorage");
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
            log::info!("Saved storage settings to localStorage");
        }
    }

    /// Format quota as human-readable string.
    pub fn format_quota(&self) -> String {
        format_bytes(self.quota_bytes)
    }

    /// Returns minimum quota (100 MB).
    pub fn min_quota() -> u64 {
        100 * 1024 * 1024
    }

    /// Returns maximum quota (2 GB).
    pub fn max_quota() -> u64 {
        2 * 1024 * 1024 * 1024
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
