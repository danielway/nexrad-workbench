//! AWS download pipeline for NEXRAD archive data.
//!
//! Uses channel-based communication to bridge async downloads
//! with egui's synchronous update loop.

use super::archive_index::{current_timestamp_secs, ArchiveListing};
use super::types::{CachedScan, DownloadResult};
use crate::core::acquisition::DownloadProgress;
use crate::data::{MainThreadStore, ScanCompleteness, ScanKey};
use crate::net::retry::{with_retry, Verdict, DEFAULT_POLICY};
use chrono::NaiveDate;
use eframe::egui;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// The public NEXRAD Level II archive bucket.
const ARCHIVE_BUCKET_URL: &str = "https://unidata-nexrad-level2.s3.amazonaws.com";

/// A download's byte progress, by the request's scan start (the listing's
/// file time, Unix seconds).
pub(crate) type ProgressReport = (i64, DownloadProgress);

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
    /// Byte progress of the downloads in flight, as their chunks arrive.
    progress_sender: Sender<ProgressReport>,
    progress_receiver: Receiver<ProgressReport>,
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
        let (progress_sender, progress_receiver) = channel();
        Self {
            sender,
            receiver,
            listing_sender,
            listing_receiver,
            pending_downloads: Rc::new(RefCell::new(HashSet::new())),
            pending_listings: Rc::new(RefCell::new(HashSet::new())),
            stats: NetworkStats::new(),
            progress_sender,
            progress_receiver,
        }
    }

    /// Get a clone of the network stats for UI display.
    pub(crate) fn stats(&self) -> NetworkStats {
        self.stats.clone()
    }

    /// Download a specific file from the archive by name. The archive
    /// identifier (site, date, S3 key) is derived from the file name itself.
    ///
    /// Returns false if the download is already pending.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn download_file(
        &self,
        ctx: egui::Context,
        site_id: String,
        file_name: String,
        timestamp: i64,
        facade: MainThreadStore,
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
        let progress = self.progress_sender.clone();
        let progress_ctx = ctx.clone();
        let report = move |p: DownloadProgress| {
            let _ = progress.send((timestamp, p));
            progress_ctx.request_repaint();
        };

        wasm_bindgen_futures::spawn_local(async move {
            let result = download_specific_file(
                &site_id,
                &file_name,
                timestamp,
                facade,
                stats,
                elevation_filter,
                &report,
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
    /// The next byte progress report from a download in flight, if any.
    pub(crate) fn try_recv_progress(&self) -> Option<ProgressReport> {
        self.progress_receiver.try_recv().ok()
    }

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
        async move { classify_nexrad_result(archive::list_files(&s, date).await) }
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

    let listing = ArchiveListing::from_file_names(
        files.iter().map(|f| f.name().to_string()),
        &date,
        current_timestamp_secs(),
    );

    log::debug!(
        "Archive listing for {}/{}: {} volumes",
        site_id,
        date,
        listing.files.len()
    );

    ListingResult::Success {
        site_id: site_id.to_string(),
        date,
        listing,
    }
}

/// Map a `nexrad-data` archive result to a retry [`Verdict`].
///
/// Transport-layer S3 errors (network failures, 5xx, mid-stream errors,
/// truncated lists) are retryable. Everything else — including `S3ObjectNotFound`
/// (404), which means the file genuinely does not exist in the archive — is
/// terminal.
fn classify_nexrad_result<T>(result: nexrad_data::result::Result<T>) -> Verdict<T> {
    use nexrad_data::result::Error;
    match result {
        Ok(v) => Verdict::Ok(v),
        Err(Error::Aws(e)) if e.is_transient() => Verdict::Retry { after: None },
        Err(e) => Verdict::Terminal(format!("{}", e)),
    }
}

/// Downloads a specific file from the archive.
async fn download_specific_file(
    site_id: &str,
    file_name: &str,
    timestamp: i64,
    facade: MainThreadStore,
    stats: NetworkStats,
    elevation_filter: Option<u8>,
    report: &dyn Fn(DownloadProgress),
) -> DownloadResult {
    use nexrad::data::aws::archive;

    // Check cache first (no network call). The dedup granularity depends on
    // scope: a filter-scoped fetch only needs its one elevation present, while
    // an unfiltered fetch is a hit only when the whole volume is complete.
    // `timestamp` is the archive listing's time, but stored scans are keyed
    // by decoded volume-header time — probe within the app-wide join
    // tolerance so a re-request lands on the cached volume instead of
    // re-downloading it.
    let scan_key = ScanKey::from_secs(site_id, timestamp);
    if let Ok(Some(entry)) = facade
        .scan_availability_near(&scan_key, crate::SCAN_CACHE_MATCH_TOLERANCE_SECS * 1000)
        .await
    {
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

    // The archive key is derived entirely from the file name (site, date,
    // and S3 key are parsed out of it), and the name came from an archive
    // listing in the first place — so no pre-GET LIST is needed. A name
    // that doesn't exist surfaces as a terminal 404 on the GET.
    let Some(url) = archive::Identifier::parse(file_name)
        .ok()
        .map(|id| archive_url(&id))
    else {
        return DownloadResult::Error {
            message: format!("Download failed: bad file name {file_name}"),
            site_id: site_id.to_string(),
            scan_start: timestamp,
        };
    };

    // Download the file, its body read chunk by chunk so the bytes received
    // reach the timeline as they arrive.
    stats.request_started();
    let fetch_start = web_time::Instant::now();
    let data = match with_retry(&DEFAULT_POLICY, "archive_download", |_attempt| {
        let url = url.clone();
        async move { fetch_streaming(&url, report).await }
    })
    .await
    {
        Ok(data) => data,
        Err(msg) => {
            stats.request_completed(0);
            return DownloadResult::Error {
                message: format!("Download failed: {}", msg),
                site_id: site_id.to_string(),
                scan_start: timestamp,
            };
        }
    };
    let fetch_ms = fetch_start.elapsed().as_secs_f64() * 1000.0;

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

/// The archive URL of a volume file: `<bucket>/YYYY/MM/DD/SITE/<name>`.
fn archive_url(id: &nexrad::data::aws::archive::Identifier) -> String {
    format!(
        "{}/{}/{}/{}",
        ARCHIVE_BUCKET_URL,
        id.date_time().format("%Y/%m/%d"),
        id.site(),
        id.name()
    )
}

/// One GET of an archive file, its body read as a stream: each chunk that
/// arrives is reported (bytes so far, and the response's `Content-Length`
/// when it has one). Classified for the retry loop: a 404 or another client
/// error is terminal; a dropped connection, 408, 429 or 5xx retries.
async fn fetch_streaming(url: &str, report: &dyn Fn(DownloadProgress)) -> Verdict<Vec<u8>> {
    let global = js_sys::global();
    let promise = if let Some(window) = global.dyn_ref::<web_sys::Window>() {
        window.fetch_with_str(url)
    } else if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
        worker.fetch_with_str(url)
    } else {
        return Verdict::Terminal("no fetch".into());
    };
    let response: web_sys::Response = match JsFuture::from(promise).await {
        Ok(v) => match v.dyn_into() {
            Ok(r) => r,
            Err(_) => return Verdict::Terminal("invalid response".into()),
        },
        Err(_) => return Verdict::Retry { after: None },
    };
    let status = response.status();
    if status == 408 || status == 429 || (500..=599).contains(&status) {
        return Verdict::Retry { after: None };
    }
    if status == 404 {
        return Verdict::Terminal("Object not found (404)".into());
    }
    if !response.ok() {
        return Verdict::Terminal(format!("HTTP {status}"));
    }
    let total = response
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok());
    report(DownloadProgress { received: 0, total });
    let Some(body) = response.body() else {
        // No stream to read: take the body whole.
        let Ok(buf) = response.array_buffer() else {
            return Verdict::Retry { after: None };
        };
        return match JsFuture::from(buf).await {
            Ok(buf) => {
                let data = js_sys::Uint8Array::new(&buf).to_vec();
                report(DownloadProgress {
                    received: data.len() as u64,
                    total,
                });
                Verdict::Ok(data)
            }
            Err(_) => Verdict::Retry { after: None },
        };
    };
    let reader: web_sys::ReadableStreamDefaultReader = body.get_reader().unchecked_into();
    let mut data: Vec<u8> = Vec::with_capacity(total.unwrap_or(0).min(64 << 20) as usize);
    loop {
        let chunk = match JsFuture::from(reader.read()).await {
            Ok(c) => c,
            Err(_) => return Verdict::Retry { after: None },
        };
        let done = js_sys::Reflect::get(&chunk, &"done".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if done {
            break;
        }
        let Ok(value) = js_sys::Reflect::get(&chunk, &"value".into()) else {
            return Verdict::Retry { after: None };
        };
        let bytes = js_sys::Uint8Array::new(&value);
        let start = data.len();
        data.resize(start + bytes.length() as usize, 0);
        bytes.copy_to(&mut data[start..]);
        report(DownloadProgress {
            received: data.len() as u64,
            total,
        });
    }
    if total.is_some_and(|t| t != data.len() as u64) {
        // Cut off mid-body.
        return Verdict::Retry { after: None };
    }
    Verdict::Ok(data)
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::nexrad::acquisition::archive_index::ArchiveFileMeta;
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
