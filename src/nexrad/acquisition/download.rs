//! AWS download pipeline for NEXRAD archive data.
//!
//! Uses channel-based communication to bridge async downloads
//! with egui's synchronous update loop.

use super::archive_index::{current_timestamp_secs, ArchiveFileMeta, ArchiveListing};
use super::types::{CachedScan, DownloadResult};
use crate::data::{DataFacade, ScanCompleteness, ScanKey};
use crate::net::retry::{with_retry, Verdict, DEFAULT_POLICY};
use chrono::NaiveDate;
use eframe::egui;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};

/// Result of an archive listing request.
#[derive(Debug, Clone)]
pub(crate) enum ListingResult {
    /// Successfully fetched listing
    Success {
        site_id: String,
        date: NaiveDate,
        listing: ArchiveListing,
    },
    /// Listing request failed (site/date identify the request for backoff).
    Error {
        site_id: String,
        date: NaiveDate,
        message: String,
    },
}

/// Shared network statistics for live tracking.
#[derive(Clone, Default)]
pub(crate) struct NetworkStats {
    /// Number of currently active (in-flight) network requests
    pub active_requests: Rc<RefCell<u32>>,
    /// Total number of network requests made this session
    pub total_requests: Rc<RefCell<u32>>,
    /// Total bytes transferred (downloaded) this session
    pub total_bytes: Rc<RefCell<u64>>,
}

impl NetworkStats {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Get current active request count.
    pub(crate) fn active_count(&self) -> u32 {
        *self.active_requests.borrow()
    }

    /// Get total request count.
    pub(crate) fn total_count(&self) -> u32 {
        *self.total_requests.borrow()
    }

    /// Get total bytes transferred.
    pub(crate) fn bytes_transferred(&self) -> u64 {
        *self.total_bytes.borrow()
    }

    /// Record start of a network request.
    pub(crate) fn request_started(&self) {
        *self.active_requests.borrow_mut() += 1;
        *self.total_requests.borrow_mut() += 1;
    }

    /// Record completion of a network request.
    pub(crate) fn request_completed(&self, bytes: u64) {
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
pub(crate) struct DownloadChannel {
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
    pub(crate) fn new() -> Self {
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
    pub(crate) fn stats(&self) -> NetworkStats {
        self.stats.clone()
    }

    /// Download a specific file from the archive by name.
    ///
    /// Returns false if the download is already pending.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn download_file(
        &self,
        ctx: egui::Context,
        site_id: String,
        date: NaiveDate,
        file_name: String,
        timestamp: i64,
        facade: DataFacade,
        elevation_filter: Option<u8>,
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
            let result = download_specific_file(
                &site_id,
                date,
                &file_name,
                timestamp,
                facade,
                stats,
                elevation_filter,
            )
            .await;

            // Remove from pending set
            pending.borrow_mut().remove(&storage_key);

            let _ = sender.send(result);
            ctx.request_repaint();
        });

        true
    }

    /// Check if a download is pending for the given storage key.
    pub(crate) fn is_download_pending(&self, site_id: &str, timestamp: i64) -> bool {
        let storage_key = format!("{}_{}", site_id, timestamp);
        self.pending_downloads.borrow().contains(&storage_key)
    }

    /// Fetch archive listing for a site/date.
    ///
    /// Returns false if the request is already pending.
    pub(crate) fn fetch_listing(
        &self,
        ctx: egui::Context,
        site_id: String,
        date: NaiveDate,
    ) -> bool {
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
    pub(crate) fn is_listing_pending(&self, site_id: &str, date: &NaiveDate) -> bool {
        let listing_key = format!("{}_{}", site_id, date);
        self.pending_listings.borrow().contains(&listing_key)
    }

    /// Non-blocking check for a completed download.
    pub(crate) fn try_recv(&self) -> Option<DownloadResult> {
        self.receiver.try_recv().ok()
    }

    /// Non-blocking check for a completed listing request.
    pub(crate) fn try_recv_listing(&self) -> Option<ListingResult> {
        self.listing_receiver.try_recv().ok()
    }
}

/// Fetches the archive listing for a site/date.
async fn fetch_archive_listing(site_id: &str, date: NaiveDate) -> ListingResult {
    use nexrad::data::aws::archive;

    log::debug!("Fetching archive listing for {}/{}", site_id, date);

    let site_owned = site_id.to_string();
    let files = match with_retry(&DEFAULT_POLICY, "archive_list", |_attempt| {
        let s = site_owned.clone();
        async move { classify_nexrad_result(archive::list_files(&s, &date).await) }
    })
    .await
    {
        Ok(files) => files,
        Err(msg) => {
            return ListingResult::Error {
                site_id: site_owned,
                date,
                message: format!("Failed to list files: {}", msg),
            };
        }
    };

    let mut file_metas: Vec<ArchiveFileMeta> = files
        .iter()
        .filter_map(|f| {
            let name = f.name().to_string();
            let timestamp = ArchiveFileMeta::parse_timestamp_from_name(&name, &date)?;
            Some(ArchiveFileMeta { name, timestamp })
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

/// Map a `nexrad-data` archive result to a retry [`Verdict`].
///
/// Transport-layer S3 errors (network failures, 5xx, mid-stream errors,
/// truncated lists) are retryable. Everything else — including `S3ObjectNotFound`
/// (404), which means the file genuinely does not exist in the archive — is
/// terminal.
fn classify_nexrad_result<T>(result: nexrad_data::result::Result<T>) -> Verdict<T> {
    use nexrad_data::result::aws::AWSError;
    use nexrad_data::result::Error;
    match result {
        Ok(v) => Verdict::Ok(v),
        Err(Error::AWS(
            AWSError::S3GetObjectRequest(_)
            | AWSError::S3GetObject(_)
            | AWSError::S3Streaming(_)
            | AWSError::S3ListObjects(_)
            | AWSError::TruncatedListObjectsResponse,
        )) => Verdict::Retry { after: None },
        Err(e) => Verdict::Terminal(format!("{}", e)),
    }
}

/// Downloads a specific file from the archive.
async fn download_specific_file(
    site_id: &str,
    date: NaiveDate,
    file_name: &str,
    timestamp: i64,
    facade: DataFacade,
    stats: NetworkStats,
    elevation_filter: Option<u8>,
) -> DownloadResult {
    use nexrad::data::aws::archive;

    // Check cache first (no network call). The dedup granularity depends on
    // scope: a filter-scoped fetch only needs its one elevation present, while
    // an unfiltered fetch is a hit only when the whole volume is complete.
    let scan_key = ScanKey::from_secs(site_id, timestamp);
    if let Ok(Some(entry)) = facade.scan_availability(&scan_key).await {
        let cache_hit = match elevation_filter {
            Some(elev) => entry.has_elevation(elev),
            None => entry.completeness() == ScanCompleteness::Complete,
        };
        if cache_hit {
            log::debug!("Cache hit for {}", scan_key);
            let cached = CachedScan::new(site_id, timestamp, file_name.to_string(), vec![]);
            return DownloadResult::CacheHit(cached);
        }
    }

    log::debug!("Cache miss, downloading: {}", file_name);

    // Request 1: List files to find the one we want
    stats.request_started();
    let site_owned = site_id.to_string();
    let files = match with_retry(&DEFAULT_POLICY, "archive_list", |_attempt| {
        let s = site_owned.clone();
        async move { classify_nexrad_result(archive::list_files(&s, &date).await) }
    })
    .await
    {
        Ok(files) => {
            stats.request_completed(0);
            files
        }
        Err(msg) => {
            stats.request_completed(0);
            return DownloadResult::Error {
                message: format!("Failed to list files: {}", msg),
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
    let file = match with_retry(&DEFAULT_POLICY, "archive_download", |_attempt| {
        let id = file_meta.clone();
        async move { classify_nexrad_result(archive::download_file(id).await) }
    })
    .await
    {
        Ok(file) => file,
        Err(msg) => {
            stats.request_completed(0);
            return DownloadResult::Error {
                message: format!("Download failed: {}", msg),
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn network_stats_default_is_zero() {
        let stats = NetworkStats::default();
        assert_eq!(stats.active_count(), 0);
        assert_eq!(stats.total_count(), 0);
        assert_eq!(stats.bytes_transferred(), 0);
    }

    #[wasm_bindgen_test]
    fn network_stats_new_matches_default() {
        let stats = NetworkStats::new();
        assert_eq!(stats.active_count(), 0);
        assert_eq!(stats.total_count(), 0);
        assert_eq!(stats.bytes_transferred(), 0);
    }

    #[wasm_bindgen_test]
    fn network_stats_request_started_increments_active_and_total() {
        let stats = NetworkStats::new();
        stats.request_started();
        assert_eq!(stats.active_count(), 1);
        assert_eq!(stats.total_count(), 1);
        assert_eq!(stats.bytes_transferred(), 0);

        stats.request_started();
        assert_eq!(stats.active_count(), 2);
        assert_eq!(stats.total_count(), 2);
    }

    #[wasm_bindgen_test]
    fn network_stats_request_completed_decrements_active_and_adds_bytes() {
        let stats = NetworkStats::new();
        stats.request_started();
        stats.request_started();
        stats.request_completed(1500);
        // total_requests stays at 2 (only started bumps it); active drops to 1.
        assert_eq!(stats.active_count(), 1);
        assert_eq!(stats.total_count(), 2);
        assert_eq!(stats.bytes_transferred(), 1500);

        stats.request_completed(500);
        assert_eq!(stats.active_count(), 0);
        assert_eq!(stats.bytes_transferred(), 2000);
    }

    #[wasm_bindgen_test]
    fn network_stats_completed_saturates_active_at_zero() {
        let stats = NetworkStats::new();
        // No request started; completing must not underflow active count.
        stats.request_completed(42);
        assert_eq!(stats.active_count(), 0);
        assert_eq!(stats.total_count(), 0);
        assert_eq!(stats.bytes_transferred(), 42);
    }

    #[wasm_bindgen_test]
    fn network_stats_clone_shares_underlying_counters() {
        let a = NetworkStats::new();
        let b = a.clone();
        a.request_started();
        // Clones share the same Rc<RefCell>, so b observes a's mutation.
        assert_eq!(b.active_count(), 1);
        assert_eq!(b.total_count(), 1);
        b.request_completed(10);
        assert_eq!(a.active_count(), 0);
        assert_eq!(a.bytes_transferred(), 10);
    }

    #[wasm_bindgen_test]
    fn download_channel_new_starts_empty() {
        let chan = DownloadChannel::new();
        assert!(chan.try_recv().is_none());
        assert!(chan.try_recv_listing().is_none());
    }

    #[wasm_bindgen_test]
    fn download_channel_default_starts_empty() {
        let chan = DownloadChannel::default();
        assert!(chan.try_recv().is_none());
        assert!(chan.try_recv_listing().is_none());
    }

    #[wasm_bindgen_test]
    fn download_channel_stats_fresh_are_zero() {
        let chan = DownloadChannel::new();
        let stats = chan.stats();
        assert_eq!(stats.active_count(), 0);
        assert_eq!(stats.total_count(), 0);
        assert_eq!(stats.bytes_transferred(), 0);
    }

    #[wasm_bindgen_test]
    fn download_channel_no_pending_initially() {
        let chan = DownloadChannel::new();
        assert!(!chan.is_download_pending("KDMX", 1_700_000_000));
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        assert!(!chan.is_listing_pending("KDMX", &date));
    }

    #[wasm_bindgen_test]
    fn listing_result_error_variant_fields() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let res = ListingResult::Error {
            site_id: "KDMX".to_string(),
            date,
            message: "boom".to_string(),
        };
        match res {
            ListingResult::Error {
                site_id,
                date: d,
                message,
            } => {
                assert_eq!(site_id, "KDMX");
                assert_eq!(d, date);
                assert_eq!(message, "boom");
            }
            ListingResult::Success { .. } => panic!("expected Error variant"),
        }
    }

    #[wasm_bindgen_test]
    fn listing_result_success_carries_sorted_listing() {
        let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
        let listing = ArchiveListing {
            files: vec![ArchiveFileMeta {
                name: "KDMX20240501_120000_V06".to_string(),
                timestamp: 1_714_564_800,
            }],
            fetched_at: 0.0,
        };
        let res = ListingResult::Success {
            site_id: "KDMX".to_string(),
            date,
            listing,
        };
        match res {
            ListingResult::Success {
                site_id,
                date: d,
                listing,
            } => {
                assert_eq!(site_id, "KDMX");
                assert_eq!(d, date);
                assert_eq!(listing.files.len(), 1);
                assert_eq!(listing.files[0].timestamp, 1_714_564_800);
            }
            ListingResult::Error { .. } => panic!("expected Success variant"),
        }
    }
}
