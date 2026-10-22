//! Async channel for loading cache metadata without blocking the UI.
//!
//! This module provides a channel-based interface for loading scan metadata
//! from IndexedDB asynchronously. The UI can request a cache load and poll
//! for results each frame.

use crate::core::{CacheLoadResult, ScanMetadata};
use crate::data::{MainThreadStore, SiteId, UnixMillis};
use eframe::egui::Context;
use std::cell::RefCell;
use std::rc::Rc;

/// Channel for async cache loading operations.
///
/// Allows the UI to request metadata loading from IndexedDB without blocking.
pub(crate) struct CacheLoadChannel {
    /// Completed cache loads, oldest first (one per site loaded).
    receiver: Rc<RefCell<std::collections::VecDeque<CacheLoadResult>>>,
    /// Flag indicating a load is in progress
    loading: Rc<RefCell<bool>>,
}

impl CacheLoadChannel {
    /// Creates a new cache load channel.
    pub(crate) fn new() -> Self {
        Self {
            receiver: Rc::new(RefCell::new(std::collections::VecDeque::new())),
            loading: Rc::new(RefCell::new(false)),
        }
    }

    /// Returns true if a cache load is currently in progress.
    pub(crate) fn is_loading(&self) -> bool {
        *self.loading.borrow()
    }

    /// Initiates an async load of timeline metadata for each site, one
    /// after another; each site's result arrives on its own.
    ///
    /// If a load is already in progress, this call is ignored.
    pub(crate) fn load_site_timelines(
        &self,
        ctx: Context,
        facade: MainThreadStore,
        site_ids: Vec<String>,
    ) {
        if *self.loading.borrow() {
            log::debug!("Cache load already in progress, ignoring request");
            return;
        }

        *self.loading.borrow_mut() = true;
        let receiver = self.receiver.clone();
        let loading = self.loading.clone();

        wasm_bindgen_futures::spawn_local(async move {
            for site_id in site_ids {
                let result = load_one(&facade, site_id).await;
                receiver.borrow_mut().push_back(result);
                ctx.request_repaint();
            }
            *loading.borrow_mut() = false;
            ctx.request_repaint();
        });
    }

    /// Non-blocking receive for cache load results.
    pub(crate) fn try_recv(&self) -> Option<CacheLoadResult> {
        self.receiver.borrow_mut().pop_front()
    }

    /// Clears all cached data.
    pub(crate) fn clear_cache(&self, ctx: Context, facade: MainThreadStore) {
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

            receiver.borrow_mut().push_back(result);
            *loading.borrow_mut() = false;

            ctx.request_repaint();
        });
    }
}

/// Load one site's timeline metadata from the store.
async fn load_one(facade: &MainThreadStore, site_id: String) -> CacheLoadResult {
    let t_total = web_time::Instant::now();
    log::debug!("Loading cache metadata for site: {}", site_id);
    let site = SiteId::new(&site_id);
    let start = UnixMillis(0);
    let end = UnixMillis::now();

    match facade.list_scans(&site, start, end).await {
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
        ch.receiver
            .borrow_mut()
            .push_back(CacheLoadResult::Success {
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
    fn each_sites_load_arrives_in_turn() {
        let ch = CacheLoadChannel::new();
        for site in ["KTBW", "KMLB"] {
            ch.receiver
                .borrow_mut()
                .push_back(CacheLoadResult::Success {
                    site_id: site.to_string(),
                    metadata: Vec::new(),
                    total_cache_size: 0,
                });
        }
        let ids: Vec<String> = std::iter::from_fn(|| ch.try_recv())
            .map(|r| match r {
                CacheLoadResult::Success { site_id, .. } => site_id,
                CacheLoadResult::Error(e) => e,
            })
            .collect();
        assert_eq!(ids, vec!["KTBW", "KMLB"]);
    }

    #[wasm_bindgen_test]
    fn try_recv_takes_error_result() {
        let ch = CacheLoadChannel::new();
        ch.receiver
            .borrow_mut()
            .push_back(CacheLoadResult::Error("boom".to_string()));

        match ch.try_recv() {
            Some(CacheLoadResult::Error(msg)) => assert_eq!(msg, "boom"),
            other => panic!("expected Error, got {:?}", other),
        }
        assert!(ch.try_recv().is_none());
    }

    #[wasm_bindgen_test]
    fn empty_success_result_round_trips_via_receiver() {
        // Mirrors the clear_cache success payload shape.
        let ch = CacheLoadChannel::new();
        ch.receiver
            .borrow_mut()
            .push_back(CacheLoadResult::Success {
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
}
