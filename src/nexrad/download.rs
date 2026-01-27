//! AWS download pipeline for NEXRAD archive data.
//!
//! Uses channel-based communication to bridge async downloads
//! with egui's synchronous update loop.

use super::archive_index::{current_timestamp_secs, ArchiveFileMeta, ArchiveListing};
use super::types::{CachedScan, DownloadResult, ScanKey};
use crate::data::{DataFacade, ScanCompleteness, ScanKey as DataScanKey, process_archive_download, reassemble_records};
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

    /// Spawns an async download task for NEXRAD data.
    ///
    /// The download pipeline:
    /// 1. Check v4 cache for existing data
    /// 2. If not cached, download from AWS S3 using nexrad-data
    /// 3. Cache the result in v4 format
    /// 4. Send through channel
    #[cfg(target_arch = "wasm32")]
    pub fn download(
        &self,
        ctx: egui::Context,
        site_id: String,
        date: chrono::NaiveDate,
        facade: DataFacade,
    ) {
        let sender = self.sender.clone();
        let stats = self.stats.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let result = download_nexrad_data(&site_id, date, facade, stats).await;
            let _ = sender.send(result);
            ctx.request_repaint();
        });
    }

    /// Native download using nexrad's built-in AWS support.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn download(
        &self,
        ctx: egui::Context,
        site_id: String,
        date: chrono::NaiveDate,
        _facade: DataFacade,
    ) {
        let sender = self.sender.clone();

        std::thread::spawn(move || {
            let result = pollster::block_on(download_nexrad_data_native(&site_id, date));
            let _ = sender.send(result);
            ctx.request_repaint();
        });
    }

    /// Download a specific file from the archive by name.
    ///
    /// Returns false if the download is already pending.
    #[cfg(target_arch = "wasm32")]
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

    #[cfg(not(target_arch = "wasm32"))]
    pub fn download_file(
        &self,
        _ctx: egui::Context,
        _site_id: String,
        _date: NaiveDate,
        _file_name: String,
        _timestamp: i64,
        _facade: DataFacade,
    ) -> bool {
        // Not implemented for native
        false
    }

    /// Check if a download is pending for the given storage key.
    pub fn is_download_pending(&self, site_id: &str, timestamp: i64) -> bool {
        let storage_key = format!("{}_{}", site_id, timestamp);
        self.pending_downloads.borrow().contains(&storage_key)
    }

    /// Fetch archive listing for a site/date.
    ///
    /// Returns false if the request is already pending.
    #[cfg(target_arch = "wasm32")]
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

    #[cfg(not(target_arch = "wasm32"))]
    pub fn fetch_listing(&self, _ctx: egui::Context, _site_id: String, _date: NaiveDate) -> bool {
        // Not implemented for native
        false
    }

    /// Check if a listing request is pending.
    pub fn is_listing_pending(&self, site_id: &str, date: &NaiveDate) -> bool {
        let listing_key = format!("{}_{}", site_id, date);
        self.pending_listings.borrow().contains(&listing_key)
    }

    /// Non-blocking check for a completed download.
    ///
    /// Returns Some(result) if a download completed,
    /// None if no result is ready yet.
    pub fn try_recv(&self) -> Option<DownloadResult> {
        self.receiver.try_recv().ok()
    }

    /// Non-blocking check for a completed listing request.
    pub fn try_recv_listing(&self) -> Option<ListingResult> {
        self.listing_receiver.try_recv().ok()
    }
}

/// Fetches the archive listing for a site/date.
#[cfg(target_arch = "wasm32")]
async fn fetch_archive_listing(site_id: &str, date: NaiveDate) -> ListingResult {
    use nexrad::data::aws::archive;

    log::info!("Fetching archive listing for {}/{}", site_id, date);

    let files = match archive::list_files(site_id, &date).await {
        Ok(files) => files,
        Err(e) => {
            return ListingResult::Error(format!("Failed to list files: {}", e));
        }
    };

    let mut file_metas: Vec<ArchiveFileMeta> = files
        .iter()
        .filter_map(|f| {
            let name = f.name().to_string();
            let timestamp = ArchiveFileMeta::parse_timestamp_from_name(&name, &date)?;
            Some(ArchiveFileMeta {
                name,
                size: 0, // Size not available from listing API
                timestamp,
            })
        })
        .collect();

    // Sort by timestamp
    file_metas.sort_by_key(|f| f.timestamp);

    log::info!(
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

/// Downloads a specific file from the archive.
#[cfg(target_arch = "wasm32")]
async fn download_specific_file(
    site_id: &str,
    date: NaiveDate,
    file_name: &str,
    timestamp: i64,
    facade: DataFacade,
    stats: NetworkStats,
) -> DownloadResult {
    use nexrad::data::aws::archive;

    let key = ScanKey::new(site_id, timestamp);

    // Check v4 cache first (no network call)
    let scan_key = DataScanKey::from_legacy(site_id, timestamp);
    if let Ok(Some(entry)) = facade.cache().scan_availability(&scan_key).await {
        if entry.completeness() == ScanCompleteness::Complete {
            // Reassemble from v4 cache
            match facade.cache().list_records_for_scan(&scan_key).await {
                Ok(record_keys) => {
                    let mut records = Vec::with_capacity(record_keys.len());
                    for rkey in record_keys {
                        if let Ok(Some(record)) = facade.get_record(&rkey).await {
                            records.push(record);
                        }
                    }
                    if !records.is_empty() {
                        records.sort_by_key(|r| r.key.record_id);
                        let data = reassemble_records(&records);
                        log::info!("V4 cache hit for {} ({} bytes)", key.to_storage_key(), data.len());
                        let cached = CachedScan::new(key, file_name.to_string(), data);
                        return DownloadResult::CacheHit(cached);
                    }
                }
                Err(e) => {
                    log::warn!("V4 cache lookup failed: {}", e);
                }
            }
        }
    }

    log::info!("Cache miss for {}", key.to_storage_key());

    // Request 1: List files to find the one we want
    stats.request_started();
    let files = match archive::list_files(site_id, &date).await {
        Ok(files) => {
            stats.request_completed(0); // Listing doesn't count toward bytes transferred
            files
        }
        Err(e) => {
            stats.request_completed(0);
            return DownloadResult::Error(format!("Failed to list files: {}", e));
        }
    };

    // Find the specific file
    let file_meta = match files.iter().find(|f| f.name() == file_name) {
        Some(f) => f.clone(),
        None => {
            return DownloadResult::Error(format!("File not found: {}", file_name));
        }
    };

    log::info!("Downloading: {}", file_name);

    // Request 2: Download the file (with timing)
    stats.request_started();
    let fetch_start = web_time::Instant::now();
    let file = match archive::download_file(file_meta).await {
        Ok(file) => file,
        Err(e) => {
            stats.request_completed(0);
            return DownloadResult::Error(format!("Download failed: {}", e));
        }
    };
    let fetch_ms = fetch_start.elapsed().as_secs_f64() * 1000.0;

    let data = file.data().to_vec();
    let bytes_downloaded = data.len() as u64;
    log::info!("Downloaded {} bytes in {:.0}ms", bytes_downloaded, fetch_ms);

    let cached = CachedScan::new(key, file_name.to_string(), data.clone());

    // Store as records in v4 cache only (with timing)
    let decode_start = web_time::Instant::now();
    match process_archive_download(&facade, site_id, file_name, timestamp, &data).await {
        Ok((scan_key, records_stored)) => {
            log::info!(
                "Stored {} records for scan {} in v4 cache",
                records_stored,
                scan_key
            );
        }
        Err(e) => {
            log::warn!("Failed to store records in v4 cache: {}", e);
        }
    }
    let decode_ms = decode_start.elapsed().as_secs_f64() * 1000.0;

    stats.request_completed(bytes_downloaded);
    DownloadResult::Success {
        scan: cached,
        fetch_latency_ms: fetch_ms,
        decode_latency_ms: decode_ms,
    }
}

/// Performs the actual NEXRAD download using nexrad-data crate.
#[cfg(target_arch = "wasm32")]
async fn download_nexrad_data(
    site_id: &str,
    date: chrono::NaiveDate,
    facade: DataFacade,
    stats: NetworkStats,
) -> DownloadResult {
    use nexrad::data::aws::archive;

    // Generate a timestamp for the scan key (use start of day)
    let timestamp = date
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);

    let key = ScanKey::new(site_id, timestamp);

    // Check v4 cache first (no network call)
    let scan_key = DataScanKey::from_legacy(site_id, timestamp);
    if let Ok(Some(entry)) = facade.cache().scan_availability(&scan_key).await {
        if entry.completeness() == ScanCompleteness::Complete {
            // Reassemble from v4 cache
            match facade.cache().list_records_for_scan(&scan_key).await {
                Ok(record_keys) => {
                    let mut records = Vec::with_capacity(record_keys.len());
                    for rkey in record_keys {
                        if let Ok(Some(record)) = facade.get_record(&rkey).await {
                            records.push(record);
                        }
                    }
                    if !records.is_empty() {
                        records.sort_by_key(|r| r.key.record_id);
                        let data = reassemble_records(&records);
                        log::info!("V4 cache hit for {} ({} bytes)", key.to_storage_key(), data.len());
                        let file_name = entry.file_name.unwrap_or_default();
                        let cached = CachedScan::new(key, file_name, data);
                        return DownloadResult::CacheHit(cached);
                    }
                }
                Err(e) => {
                    log::warn!("V4 cache lookup failed: {}", e);
                }
            }
        }
    }

    log::info!("Cache miss for {}", key.to_storage_key());

    // Request 1: List available files for this site/date
    stats.request_started();
    let files = match archive::list_files(site_id, &date).await {
        Ok(files) => {
            stats.request_completed(0); // Listing doesn't count toward bytes transferred
            files
        }
        Err(e) => {
            stats.request_completed(0);
            return DownloadResult::Error(format!("Failed to list files: {}", e));
        }
    };

    if files.is_empty() {
        return DownloadResult::Error(format!("No files available for {} on {}", site_id, date));
    }

    // Get the first file (typically the first volume scan of the day)
    let file_meta = files[0].clone();
    let file_name = file_meta.name().to_string();
    log::info!("Downloading: {}", file_name);

    // Request 2: Download the file
    stats.request_started();
    let fetch_start = web_time::Instant::now();
    let file = match archive::download_file(file_meta).await {
        Ok(file) => file,
        Err(e) => {
            stats.request_completed(0);
            return DownloadResult::Error(format!("Download failed: {}", e));
        }
    };
    let fetch_ms = fetch_start.elapsed().as_secs_f64() * 1000.0;

    // Get the raw compressed data from the file
    let data = file.data().to_vec();
    let bytes_downloaded = data.len() as u64;
    log::info!("Downloaded {} bytes in {:.0}ms", bytes_downloaded, fetch_ms);

    // Create cached scan with raw data
    let cached = CachedScan::new(key.clone(), file_name.clone(), data.clone());

    // Store in v4 cache only
    let decode_start = web_time::Instant::now();
    match process_archive_download(&facade, site_id, &file_name, timestamp, &data).await {
        Ok((scan_key, records_stored)) => {
            log::info!(
                "Stored {} records for scan {} in v4 cache",
                records_stored,
                scan_key
            );
        }
        Err(e) => {
            log::warn!("Failed to store records in v4 cache: {}", e);
        }
    }
    let decode_ms = decode_start.elapsed().as_secs_f64() * 1000.0;

    stats.request_completed(bytes_downloaded);
    DownloadResult::Success {
        scan: cached,
        fetch_latency_ms: fetch_ms,
        decode_latency_ms: decode_ms,
    }
}

/// Native download implementation using nexrad's built-in support.
#[cfg(not(target_arch = "wasm32"))]
async fn download_nexrad_data_native(site_id: &str, date: chrono::NaiveDate) -> DownloadResult {
    use nexrad::data::aws::archive;

    // List available files
    let files = match archive::list_files(site_id, &date).await {
        Ok(files) => files,
        Err(e) => {
            return DownloadResult::Error(format!("Failed to list files: {}", e));
        }
    };

    if files.is_empty() {
        return DownloadResult::Error(format!("No files available for {} on {}", site_id, date));
    }

    let file_meta = files[0].clone();
    let file_name = file_meta.name().to_string();
    log::info!("Downloading: {}", file_name);

    // Download the file
    let file = match archive::download_file(file_meta).await {
        Ok(file) => file,
        Err(e) => {
            return DownloadResult::Error(format!("Download failed: {}", e));
        }
    };

    // Get the raw compressed data from the file
    let data = file.data().to_vec();
    log::info!("Downloaded {} bytes", data.len());

    // Create scan key from date
    let timestamp = date
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);

    let key = ScanKey::new(site_id, timestamp);
    let cached = CachedScan::new(key, file_name, data);

    DownloadResult::Success {
        scan: cached,
        fetch_latency_ms: 0.0,
        decode_latency_ms: 0.0,
    }
}
