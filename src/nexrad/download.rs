//! AWS download pipeline for NEXRAD archive data.
//!
//! Uses channel-based communication to bridge async downloads
//! with egui's synchronous update loop.

use super::archive_index::{current_timestamp_secs, ArchiveFileMeta, ArchiveListing};
use super::types::{CachedScan, DownloadResult};
use crate::data::{DataFacade, ScanCompleteness, ScanKey};
use chrono::NaiveDate;
use eframe::egui;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};

/// Result of an archive listing request.
#[derive(Debug, Clone)]
pub enum ListingResult {
    /// Successfully fetched listing
    Success {
        site_id: String,
        date: NaiveDate,
        listing: ArchiveListing,
    },
    /// Listing request failed
    Error(String),
}

/// Shared network statistics for live tracking.
#[derive(Clone, Default)]
pub struct NetworkStats {
    /// Number of currently active (in-flight) network requests
    pub active_requests: Rc<RefCell<u32>>,
    /// Total number of network requests made this session
    pub total_requests: Rc<RefCell<u32>>,
    /// Total bytes transferred (downloaded) this session
    pub total_bytes: Rc<RefCell<u64>>,
}

impl NetworkStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get current active request count.
    pub fn active_count(&self) -> u32 {
        *self.active_requests.borrow()
    }

    /// Get total request count.
    pub fn total_count(&self) -> u32 {
        *self.total_requests.borrow()
    }

    /// Get total bytes transferred.
    pub fn bytes_transferred(&self) -> u64 {
        *self.total_bytes.borrow()
    }

    /// Record start of a network request.
    pub fn request_started(&self) {
        *self.active_requests.borrow_mut() += 1;
        *self.total_requests.borrow_mut() += 1;
    }

    /// Record completion of a network request.
    pub fn request_completed(&self, bytes: u64) {
        let mut active = self.active_requests.borrow_mut();
        if *active > 0 {
            *active -= 1;
        }
        *self.total_bytes.borrow_mut() += bytes;
    }
}

/// Channel-based downloader for async NEXRAD data retrieval.
///
/// Downloads are async but egui's update() is synchronous.
/// This struct provides a channel to pass results from the async
/// download task back to the UI thread.
pub struct DownloadChannel {
    sender: Sender<DownloadResult>,
    receiver: Receiver<DownloadResult>,
    /// Sender for listing results
    listing_sender: Sender<ListingResult>,
    /// Receiver for listing results
    listing_receiver: Receiver<ListingResult>,
    /// Track pending downloads to avoid duplicates (by storage key)
    pending_downloads: Rc<RefCell<HashSet<String>>>,
    /// Track pending listing requests to avoid duplicates
    pending_listings: Rc<RefCell<HashSet<String>>>,
    /// Live network statistics
    stats: NetworkStats,
}

impl Default for DownloadChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadChannel {
    pub fn new() -> Self {
        let (sender, receiver) = channel();
        let (listing_sender, listing_receiver) = channel();
        Self {
            sender,
            receiver,
            listing_sender,
            listing_receiver,
            pending_downloads: Rc::new(RefCell::new(HashSet::new())),
            pending_listings: Rc::new(RefCell::new(HashSet::new())),
            stats: NetworkStats::new(),
        }
    }

    /// Get a clone of the network stats for UI display.
    pub fn stats(&self) -> NetworkStats {
        self.stats.clone()
    }

    /// Download a specific file from the archive by name.
    ///
    /// Returns false if the download is already pending.
    pub fn download_file(
        &self,
        ctx: egui::Context,
        site_id: String,
        date: NaiveDate,
        file_name: String,
        timestamp: i64,
        facade: DataFacade,
    ) -> bool {
        let storage_key = format!("{}_{}", site_id, timestamp);

        // Check if already pending
        if !self
            .pending_downloads
            .borrow_mut()
            .insert(storage_key.clone())
        {
            log::debug!("Download already pending: {}", file_name);
            return false;
        }

        let sender = self.sender.clone();
        let pending = self.pending_downloads.clone();
        let stats = self.stats.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let result =
                download_specific_file(&site_id, date, &file_name, timestamp, facade, stats).await;

            // Remove from pending set
            pending.borrow_mut().remove(&storage_key);

            let _ = sender.send(result);
            ctx.request_repaint();
        });

        true
    }

    /// Check if a download is pending for the given storage key.
    pub fn is_download_pending(&self, site_id: &str, timestamp: i64) -> bool {
        let storage_key = format!("{}_{}", site_id, timestamp);
        self.pending_downloads.borrow().contains(&storage_key)
    }

    /// Fetch archive listing for a site/date.
    ///
    /// Returns false if the request is already pending.
    pub fn fetch_listing(&self, ctx: egui::Context, site_id: String, date: NaiveDate) -> bool {
        let listing_key = format!("{}_{}", site_id, date);

        // Check if already pending
        if !self
            .pending_listings
            .borrow_mut()
            .insert(listing_key.clone())
        {
            log::debug!("Listing already pending: {}", listing_key);
            return false;
        }

        let sender = self.listing_sender.clone();
        let pending = self.pending_listings.clone();
        let stats = self.stats.clone();

        // Track request start
        stats.request_started();

        wasm_bindgen_futures::spawn_local(async move {
            let result = fetch_archive_listing(&site_id, date).await;

            // Remove from pending set
            pending.borrow_mut().remove(&listing_key);

            // Listing requests don't transfer much data, count as 0 bytes
            stats.request_completed(0);

            let _ = sender.send(result);
            ctx.request_repaint();
        });

        true
    }

    /// Check if a listing request is pending.
    pub fn is_listing_pending(&self, site_id: &str, date: &NaiveDate) -> bool {
        let listing_key = format!("{}_{}", site_id, date);
        self.pending_listings.borrow().contains(&listing_key)
    }

    /// Non-blocking check for a completed download.
    pub fn try_recv(&self) -> Option<DownloadResult> {
        self.receiver.try_recv().ok()
    }

    /// Non-blocking check for a completed listing request.
    pub fn try_recv_listing(&self) -> Option<ListingResult> {
        self.listing_receiver.try_recv().ok()
    }
}

/// Fetches the archive listing for a site/date.
async fn fetch_archive_listing(site_id: &str, date: NaiveDate) -> ListingResult {
    use nexrad::data::aws::archive;

    log::debug!("Fetching archive listing for {}/{}", site_id, date);

    let files = match with_timeout(
        archive::list_files(site_id, &date),
        REQUEST_TIMEOUT_MS,
        "Archive listing",
    )
    .await
    {
        Ok(Ok(files)) => files,
        Ok(Err(e)) => {
            return ListingResult::Error(format!("Failed to list files: {}", e));
        }
        Err(timeout_msg) => {
            return ListingResult::Error(timeout_msg);
        }
    };

    let mut file_metas: Vec<ArchiveFileMeta> = files
        .iter()
        .filter_map(|f| {
            let name = f.name().to_string();
            let timestamp = ArchiveFileMeta::parse_timestamp_from_name(&name, &date)?;
            Some(ArchiveFileMeta {
                name,
                size: 0,
                timestamp,
            })
        })
        .collect();

    file_metas.sort_by_key(|f| f.timestamp);

    log::debug!(
        "Archive listing for {}/{}: {} files",
        site_id,
        date,
        file_metas.len()
    );

    ListingResult::Success {
        site_id: site_id.to_string(),
        date,
        listing: ArchiveListing {
            files: file_metas,
            fetched_at: current_timestamp_secs(),
        },
    }
}

/// Timeout duration for individual network requests (listing + download).
const REQUEST_TIMEOUT_MS: u32 = 30_000; // 30 seconds

/// Run a future with a timeout. Returns `Err(msg)` if the timeout fires first.
async fn with_timeout<T>(
    future: impl std::future::Future<Output = T>,
    timeout_ms: u32,
    label: &str,
) -> Result<T, String> {
    let label = label.to_string();
    let timeout = async {
        sleep_ms(timeout_ms).await;
    };

    futures_util::pin_mut!(future);
    futures_util::pin_mut!(timeout);

    match futures_util::future::select(future, timeout).await {
        futures_util::future::Either::Left((val, _)) => Ok(val),
        futures_util::future::Either::Right(_) => {
            Err(format!("{} timed out after {}s", label, timeout_ms / 1000))
        }
    }
}

async fn sleep_ms(ms: u32) {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        fn set_timeout(closure: &Closure<dyn FnMut()>, millis: u32) -> i32;
    }

    let (tx, rx) = futures_channel::oneshot::channel::<()>();
    let closure = Closure::once(move || {
        let _ = tx.send(());
    });
    set_timeout(&closure, ms);
    let _ = rx.await;
}

/// Downloads a specific file from the archive.
async fn download_specific_file(
    site_id: &str,
    date: NaiveDate,
    file_name: &str,
    timestamp: i64,
    facade: DataFacade,
    stats: NetworkStats,
) -> DownloadResult {
    use nexrad::data::aws::archive;

    // Check cache first (no network call).
    let scan_key = ScanKey::from_secs(site_id, timestamp);
    if let Ok(Some(entry)) = facade.scan_availability(&scan_key).await {
        if entry.completeness() == ScanCompleteness::Complete {
            log::debug!("Cache hit for {}", scan_key);
            let cached = CachedScan::new(site_id, timestamp, file_name.to_string(), vec![]);
            return DownloadResult::CacheHit(cached);
        }
    }

    log::debug!("Cache miss, downloading: {}", file_name);

    // Request 1: List files to find the one we want
    stats.request_started();
    let files = match with_timeout(
        archive::list_files(site_id, &date),
        REQUEST_TIMEOUT_MS,
        "Archive listing",
    )
    .await
    {
        Ok(Ok(files)) => {
            stats.request_completed(0);
            files
        }
        Ok(Err(e)) => {
            stats.request_completed(0);
            return DownloadResult::Error {
                message: format!("Failed to list files: {}", e),
                scan_start: timestamp,
            };
        }
        Err(timeout_msg) => {
            stats.request_completed(0);
            return DownloadResult::Error {
                message: timeout_msg,
                scan_start: timestamp,
            };
        }
    };

    // Find the specific file
    let file_meta = match files.iter().find(|f| f.name() == file_name) {
        Some(f) => f.clone(),
        None => {
            return DownloadResult::Error {
                message: format!("File not found: {}", file_name),
                scan_start: timestamp,
            };
        }
    };

    // Request 2: Download the file
    stats.request_started();
    let fetch_start = web_time::Instant::now();
    let file = match with_timeout(
        archive::download_file(file_meta),
        REQUEST_TIMEOUT_MS,
        "File download",
    )
    .await
    {
        Ok(Ok(file)) => file,
        Ok(Err(e)) => {
            stats.request_completed(0);
            return DownloadResult::Error {
                message: format!("Download failed: {}", e),
                scan_start: timestamp,
            };
        }
        Err(timeout_msg) => {
            stats.request_completed(0);
            return DownloadResult::Error {
                message: timeout_msg,
                scan_start: timestamp,
            };
        }
    };
    let fetch_ms = fetch_start.elapsed().as_secs_f64() * 1000.0;

    let data = file.data().to_vec();
    let bytes_downloaded = data.len() as u64;
    log::debug!("Downloaded {} bytes in {:.0}ms", bytes_downloaded, fetch_ms);

    let cached = CachedScan::new(site_id, timestamp, file_name.to_string(), data);

    stats.request_completed(bytes_downloaded);
    DownloadResult::Success {
        scan: cached,
        fetch_latency_ms: fetch_ms,
        decode_latency_ms: 0.0,
    }
}
