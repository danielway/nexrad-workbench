//! Async channel for loading cache metadata without blocking the UI.
//!
//! This module provides a channel-based interface for loading scan metadata
//! from IndexedDB asynchronously. The UI can request a cache load and poll
//! for results each frame.

use crate::core::ScanMetadata;
use crate::data::{DataFacade, SiteId, UnixMillis};
use eframe::egui::Context;
use std::cell::RefCell;
use std::rc::Rc;

/// Result of a cache load operation.
#[derive(Debug, Clone)]
pub(crate) enum CacheLoadResult {
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
pub(crate) struct CacheLoadChannel {
    /// Receiver for completed cache loads
    receiver: Rc<RefCell<Option<CacheLoadResult>>>,
    /// Flag indicating a load is in progress
    loading: Rc<RefCell<bool>>,
}

impl CacheLoadChannel {
    /// Creates a new cache load channel.
    pub(crate) fn new() -> Self {
        Self {
            receiver: Rc::new(RefCell::new(None)),
            loading: Rc::new(RefCell::new(false)),
        }
    }

    /// Returns true if a cache load is currently in progress.
    pub(crate) fn is_loading(&self) -> bool {
        *self.loading.borrow()
    }

    /// Initiates an async load of timeline metadata for a site.
    ///
    /// If a load is already in progress, this call is ignored.
    pub(crate) fn load_site_timeline(&self, ctx: Context, facade: DataFacade, site_id: String) {
        if *self.loading.borrow() {
            log::debug!("Cache load already in progress, ignoring request");
            return;
        }

        *self.loading.borrow_mut() = true;
        let receiver = self.receiver.clone();
        let loading = self.loading.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let t_total = web_time::Instant::now();
            log::debug!("Loading cache metadata for site: {}", site_id);

            let site = SiteId::new(&site_id);
            let start = UnixMillis(0);
            let end = UnixMillis::now();

            let result = match facade.list_scans(&site, start, end).await {
                Ok(scan_entries) => {
                    let list_ms = t_total.elapsed().as_secs_f64() * 1000.0;

                    let metadata: Vec<ScanMetadata> = scan_entries
                        .iter()
                        .map(|entry| ScanMetadata {
                            key: entry.scan.clone(),
                            file_name: entry.file_name.clone().unwrap_or_default(),
                            file_size: entry.total_size_bytes,
                            end_timestamp: entry.end_timestamp_secs(),
                            vcp: entry.vcp.clone(),
                            completeness: Some(entry.completeness()),
                            cached_sweep_count: Some(entry.cached_sweep_count()),
                            planned_sweep_count: entry.planned_sweep_count(),
                            sweeps: Some(entry.cached_sweeps.clone()),
                        })
                        .collect();

                    let total_cache_size = facade.total_cache_size().await.unwrap_or(0);

                    let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
                    log::debug!(
                        "Timeline loaded: {} scan(s) for {} in {:.0}ms (list_scans: {:.0}ms)",
                        metadata.len(),
                        site_id,
                        total_ms,
                        list_ms,
                    );

                    CacheLoadResult::Success {
                        site_id,
                        metadata,
                        total_cache_size,
                    }
                }
                Err(e) => {
                    // Transient IDB errors (open still settling, aborted txn)
                    // resolve on the next refresh — don't log them as errors.
                    if e.kind() == crate::data::indexeddb::ErrorKind::Transient {
                        log::warn!("Cache metadata load hit a transient error: {}", e);
                    } else {
                        log::error!("Failed to load cache metadata: {}", e);
                    }
                    CacheLoadResult::Error(e.to_string())
                }
            };

            *receiver.borrow_mut() = Some(result);
            *loading.borrow_mut() = false;

            ctx.request_repaint();
        });
    }

    /// Non-blocking receive for cache load results.
    pub(crate) fn try_recv(&self) -> Option<CacheLoadResult> {
        self.receiver.borrow_mut().take()
    }

    /// Clears all cached data.
    pub(crate) fn clear_cache(&self, ctx: Context, facade: DataFacade) {
        if *self.loading.borrow() {
            log::debug!("Cache operation in progress, ignoring clear request");
            return;
        }

        *self.loading.borrow_mut() = true;
        let receiver = self.receiver.clone();
        let loading = self.loading.clone();

        wasm_bindgen_futures::spawn_local(async move {
            log::debug!("Clearing cache...");

            let result = match facade.clear_all().await {
                Ok(()) => {
                    log::debug!("Cache cleared successfully");
                    CacheLoadResult::Success {
                        site_id: String::new(),
                        metadata: Vec::new(),
                        total_cache_size: 0,
                    }
                }
                Err(e) => {
                    log::error!("Failed to clear cache: {}", e);
                    CacheLoadResult::Error(e.to_string())
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sample_metadata() -> ScanMetadata {
        ScanMetadata {
            key: crate::data::ScanKey::new("KDMX", UnixMillis(1_700_000_000_000)),
            file_name: "KDMX20230101_000000_V06".to_string(),
            file_size: 4096,
            end_timestamp: Some(1_700_000_300),
            vcp: None,
            completeness: None,
            cached_sweep_count: Some(7),
            planned_sweep_count: Some(14),
            sweeps: None,
        }
    }

    #[wasm_bindgen_test]
    fn new_channel_is_not_loading() {
        let ch = CacheLoadChannel::new();
        assert!(!ch.is_loading());
    }

    #[wasm_bindgen_test]
    fn default_channel_is_not_loading() {
        let ch = CacheLoadChannel::default();
        assert!(!ch.is_loading());
    }

    #[wasm_bindgen_test]
    fn new_channel_try_recv_is_none() {
        let ch = CacheLoadChannel::new();
        assert!(ch.try_recv().is_none());
        // Still none on a second call.
        assert!(ch.try_recv().is_none());
    }

    #[wasm_bindgen_test]
    fn is_loading_reflects_internal_flag() {
        let ch = CacheLoadChannel::new();
        assert!(!ch.is_loading());
        *ch.loading.borrow_mut() = true;
        assert!(ch.is_loading());
        *ch.loading.borrow_mut() = false;
        assert!(!ch.is_loading());
    }

    #[wasm_bindgen_test]
    fn try_recv_takes_success_result_and_leaves_none() {
        let ch = CacheLoadChannel::new();
        *ch.receiver.borrow_mut() = Some(CacheLoadResult::Success {
            site_id: "KDMX".to_string(),
            metadata: vec![sample_metadata()],
            total_cache_size: 8192,
        });

        let got = ch.try_recv();
        match got {
            Some(CacheLoadResult::Success {
                site_id,
                metadata,
                total_cache_size,
            }) => {
                assert_eq!(site_id, "KDMX");
                assert_eq!(metadata.len(), 1);
                assert_eq!(metadata[0].file_size, 4096);
                assert_eq!(total_cache_size, 8192);
            }
            other => panic!("expected Success, got {:?}", other),
        }

        // The value was taken; the receiver is now empty.
        assert!(ch.try_recv().is_none());
    }

    #[wasm_bindgen_test]
    fn try_recv_takes_error_result() {
        let ch = CacheLoadChannel::new();
        *ch.receiver.borrow_mut() = Some(CacheLoadResult::Error("boom".to_string()));

        match ch.try_recv() {
            Some(CacheLoadResult::Error(msg)) => assert_eq!(msg, "boom"),
            other => panic!("expected Error, got {:?}", other),
        }
        assert!(ch.try_recv().is_none());
    }

    #[wasm_bindgen_test]
    fn success_result_clone_preserves_fields() {
        let original = CacheLoadResult::Success {
            site_id: "KTLX".to_string(),
            metadata: vec![sample_metadata(), sample_metadata()],
            total_cache_size: 100,
        };
        let cloned = original.clone();
        match cloned {
            CacheLoadResult::Success {
                site_id,
                metadata,
                total_cache_size,
            } => {
                assert_eq!(site_id, "KTLX");
                assert_eq!(metadata.len(), 2);
                assert_eq!(total_cache_size, 100);
            }
            other => panic!("expected Success, got {:?}", other),
        }
    }

    #[wasm_bindgen_test]
    fn error_result_clone_preserves_message() {
        let original = CacheLoadResult::Error("io failure".to_string());
        let cloned = original.clone();
        match cloned {
            CacheLoadResult::Error(msg) => assert_eq!(msg, "io failure"),
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[wasm_bindgen_test]
    fn empty_success_result_round_trips_via_receiver() {
        // Mirrors the clear_cache success payload shape.
        let ch = CacheLoadChannel::new();
        *ch.receiver.borrow_mut() = Some(CacheLoadResult::Success {
            site_id: String::new(),
            metadata: Vec::new(),
            total_cache_size: 0,
        });

        match ch.try_recv() {
            Some(CacheLoadResult::Success {
                site_id,
                metadata,
                total_cache_size,
            }) => {
                assert!(site_id.is_empty());
                assert!(metadata.is_empty());
                assert_eq!(total_cache_size, 0);
            }
            other => panic!("expected empty Success, got {:?}", other),
        }
    }

    #[wasm_bindgen_test]
    fn result_debug_is_non_empty() {
        let s = format!("{:?}", CacheLoadResult::Error("x".to_string()));
        assert!(s.contains("Error"));
        let s2 = format!(
            "{:?}",
            CacheLoadResult::Success {
                site_id: "KDMX".to_string(),
                metadata: Vec::new(),
                total_cache_size: 5,
            }
        );
        assert!(s2.contains("Success"));
    }
}
