//! Async channel for loading cache metadata without blocking the UI.
//!
//! This module provides a channel-based interface for loading scan metadata
//! from IndexedDB asynchronously. The UI can request a cache load and poll
//! for results each frame.

use super::types::ScanMetadata;
use crate::data::{DataFacade, SiteId, UnixMillis};
use eframe::egui::Context;
use std::cell::RefCell;
use std::rc::Rc;

/// Result of a cache load operation.
#[derive(Debug, Clone)]
pub enum CacheLoadResult {
    /// Successfully loaded metadata for a site
    Success {
        site_id: String,
        metadata: Vec<ScanMetadata>,
        /// Total cache size across all sites (in bytes)
        total_cache_size: u64,
    },
    /// Cache load failed with an error
    Error(String),
}

/// Channel for async cache loading operations.
///
/// Allows the UI to request metadata loading from IndexedDB without blocking.
/// The sender/receiver pattern enables the async operation to complete
/// while the UI continues rendering.
pub struct CacheLoadChannel {
    /// Receiver for completed cache loads
    receiver: Rc<RefCell<Option<CacheLoadResult>>>,
    /// Flag indicating a load is in progress
    loading: Rc<RefCell<bool>>,
}

impl CacheLoadChannel {
    /// Creates a new cache load channel.
    pub fn new() -> Self {
        Self {
            receiver: Rc::new(RefCell::new(None)),
            loading: Rc::new(RefCell::new(false)),
        }
    }

    /// Returns true if a cache load is currently in progress.
    pub fn is_loading(&self) -> bool {
        *self.loading.borrow()
    }

    /// Initiates an async load of timeline metadata for a site.
    ///
    /// If a load is already in progress, this call is ignored.
    /// Results can be retrieved via `try_recv()`.
    pub fn load_site_timeline(&self, ctx: Context, facade: DataFacade, site_id: String) {
        // Don't start a new load if one is in progress
        if *self.loading.borrow() {
            log::debug!("Cache load already in progress, ignoring request");
            return;
        }

        *self.loading.borrow_mut() = true;
        let receiver = self.receiver.clone();
        let loading = self.loading.clone();

        wasm_bindgen_futures::spawn_local(async move {
            log::info!("Loading cache metadata for site: {}", site_id);

            // Query scan index
            let site = SiteId::new(&site_id);
            let start = UnixMillis(0);
            let end = UnixMillis::now();

            let result = match facade.list_scans(&site, start, end).await {
                Ok(scan_entries) => {
                    // Convert ScanIndexEntry to ScanMetadata for UI compatibility
                    let metadata: Vec<ScanMetadata> = scan_entries
                        .iter()
                        .map(|entry| {
                            use super::types::ScanKey;
                            ScanMetadata {
                                key: ScanKey::new(
                                    &entry.scan.site.0,
                                    entry.scan.scan_start.as_secs(),
                                ),
                                file_name: entry.file_name.clone().unwrap_or_default(),
                                file_size: entry.total_size_bytes,
                                end_timestamp: entry.end_timestamp_secs,
                                vcp: None,
                                completeness: Some(entry.completeness()),
                                present_records: Some(entry.present_records),
                                expected_records: entry.expected_records,
                                sweeps: entry.sweeps.clone(),
                            }
                        })
                        .collect();

                    log::info!(
                        "Loaded {} cached scan(s) for site {}",
                        metadata.len(),
                        site_id
                    );

                    // Also calculate total cache size across all sites
                    let total_cache_size = facade.total_cache_size().await.unwrap_or(0);
                    log::info!("Total cache size: {} bytes", total_cache_size);

                    CacheLoadResult::Success {
                        site_id,
                        metadata,
                        total_cache_size,
                    }
                }
                Err(e) => {
                    log::error!("Failed to load cache metadata: {}", e);
                    CacheLoadResult::Error(e)
                }
            };

            *receiver.borrow_mut() = Some(result);
            *loading.borrow_mut() = false;

            // Request a repaint to process the result
            ctx.request_repaint();
        });
    }

    /// Non-blocking receive for cache load results.
    ///
    /// Returns `Some(result)` if a cache load has completed, `None` otherwise.
    pub fn try_recv(&self) -> Option<CacheLoadResult> {
        self.receiver.borrow_mut().take()
    }

    /// Clears all cached data.
    ///
    /// After clearing, the cache size will be 0 and timeline will be empty.
    pub fn clear_cache(&self, ctx: Context, facade: DataFacade) {
        // Don't start if a load is in progress
        if *self.loading.borrow() {
            log::debug!("Cache operation in progress, ignoring clear request");
            return;
        }

        *self.loading.borrow_mut() = true;
        let receiver = self.receiver.clone();
        let loading = self.loading.clone();

        wasm_bindgen_futures::spawn_local(async move {
            log::info!("Clearing cache...");

            let result = match facade.clear_all().await {
                Ok(()) => {
                    log::info!("Cache cleared successfully");
                    CacheLoadResult::Success {
                        site_id: String::new(),
                        metadata: Vec::new(),
                        total_cache_size: 0,
                    }
                }
                Err(e) => {
                    log::error!("Failed to clear cache: {}", e);
                    CacheLoadResult::Error(e)
                }
            };

            *receiver.borrow_mut() = Some(result);
            *loading.borrow_mut() = false;

            ctx.request_repaint();
        });
    }

}

impl Default for CacheLoadChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of loading a single scan from cache (for scrubbing).
#[derive(Debug, Clone)]
pub enum ScrubLoadResult {
    /// Successfully loaded scan data
    Success { timestamp: i64, data: Vec<u8> },
    /// Scan not found in cache
    NotFound { timestamp: i64 },
    /// Load failed with an error
    Error(String),
}

/// Channel for loading individual scans from cache on-demand (for scrubbing).
///
/// This is separate from CacheLoadChannel to allow concurrent scrubbing
/// while timeline metadata is being loaded.
pub struct ScrubLoadChannel {
    /// Receiver for completed scan loads
    receiver: Rc<RefCell<Option<ScrubLoadResult>>>,
    /// Flag indicating a load is in progress
    loading: Rc<RefCell<bool>>,
    /// Timestamp of the scan currently being loaded (to avoid duplicate requests)
    pending_timestamp: Rc<RefCell<Option<i64>>>,
}

impl ScrubLoadChannel {
    /// Creates a new scrub load channel.
    pub fn new() -> Self {
        Self {
            receiver: Rc::new(RefCell::new(None)),
            loading: Rc::new(RefCell::new(false)),
            pending_timestamp: Rc::new(RefCell::new(None)),
        }
    }

    /// Returns true if a load is currently in progress.
    pub fn is_loading(&self) -> bool {
        *self.loading.borrow()
    }

    /// Returns the timestamp of the scan being loaded, if any.
    pub fn pending_timestamp(&self) -> Option<i64> {
        *self.pending_timestamp.borrow()
    }

    /// Load a scan from cache by site ID and timestamp.
    pub fn load_scan(&self, ctx: Context, facade: DataFacade, site_id: String, timestamp: i64) {
        use crate::data::{reassemble_records, ScanKey as DataScanKey};

        // Don't start if already loading this timestamp
        if let Some(pending) = *self.pending_timestamp.borrow() {
            if pending == timestamp {
                return;
            }
        }

        // Don't start if another load is in progress (wait for it to complete)
        if *self.loading.borrow() {
            return;
        }

        *self.loading.borrow_mut() = true;
        *self.pending_timestamp.borrow_mut() = Some(timestamp);

        let receiver = self.receiver.clone();
        let loading = self.loading.clone();
        let pending = self.pending_timestamp.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let scan_key = DataScanKey::from_secs(&site_id, timestamp);

            // List all records for this scan
            let result = match facade.cache().list_records_for_scan(&scan_key).await {
                Ok(record_keys) => {
                    if record_keys.is_empty() {
                        log::debug!("Scrub load: cache miss for {} (no records)", timestamp);
                        ScrubLoadResult::NotFound { timestamp }
                    } else {
                        // Fetch all records
                        let mut records = Vec::with_capacity(record_keys.len());
                        let mut fetch_error = None;

                        for key in record_keys {
                            match facade.get_record(&key).await {
                                Ok(Some(record)) => records.push(record),
                                Ok(None) => {
                                    log::warn!("Record {} not found during fetch", key);
                                }
                                Err(e) => {
                                    fetch_error = Some(e);
                                    break;
                                }
                            }
                        }

                        if let Some(e) = fetch_error {
                            log::error!("Scrub load failed: {}", e);
                            ScrubLoadResult::Error(e)
                        } else if records.is_empty() {
                            log::debug!("Scrub load: all records missing for {}", timestamp);
                            ScrubLoadResult::NotFound { timestamp }
                        } else {
                            // Sort by record_id and reassemble
                            records.sort_by_key(|r| r.key.record_id);
                            let data = reassemble_records(&records);
                            log::debug!(
                                "Scrub load: cache hit for {} ({} records, {} bytes)",
                                timestamp,
                                records.len(),
                                data.len()
                            );
                            ScrubLoadResult::Success { timestamp, data }
                        }
                    }
                }
                Err(e) => {
                    log::error!("Scrub load failed: {}", e);
                    ScrubLoadResult::Error(e)
                }
            };

            *receiver.borrow_mut() = Some(result);
            *loading.borrow_mut() = false;
            *pending.borrow_mut() = None;

            ctx.request_repaint();
        });
    }

    /// Non-blocking receive for scan load results.
    pub fn try_recv(&self) -> Option<ScrubLoadResult> {
        self.receiver.borrow_mut().take()
    }

}

impl Default for ScrubLoadChannel {
    fn default() -> Self {
        Self::new()
    }
}
