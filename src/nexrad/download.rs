//! AWS download pipeline for NEXRAD archive data.
//!
//! Uses channel-based communication to bridge async downloads
//! with egui's synchronous update loop.

use super::cache::NexradCache;
use super::types::{CachedScan, DownloadResult, ScanKey};
use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};

/// Channel-based downloader for async NEXRAD data retrieval.
///
/// Downloads are async but egui's update() is synchronous.
/// This struct provides a channel to pass results from the async
/// download task back to the UI thread.
pub struct DownloadChannel {
    sender: Sender<DownloadResult>,
    receiver: Receiver<DownloadResult>,
}

impl Default for DownloadChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadChannel {
    pub fn new() -> Self {
        let (sender, receiver) = channel();
        Self { sender, receiver }
    }

    /// Spawns an async download task for NEXRAD data.
    ///
    /// The download pipeline:
    /// 1. Check cache for existing data
    /// 2. If not cached, download from AWS S3 using nexrad-data
    /// 3. Cache the result
    /// 4. Send through channel
    #[cfg(target_arch = "wasm32")]
    pub fn download(
        &self,
        ctx: egui::Context,
        site_id: String,
        date: chrono::NaiveDate,
        cache: NexradCache,
    ) {
        let sender = self.sender.clone();

        wasm_bindgen_futures::spawn_local(async move {
            let result = download_nexrad_data(&site_id, date, cache).await;
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
        _cache: NexradCache,
    ) {
        let sender = self.sender.clone();

        std::thread::spawn(move || {
            let result = pollster::block_on(download_nexrad_data_native(&site_id, date));
            let _ = sender.send(result);
            ctx.request_repaint();
        });
    }

    /// Non-blocking check for a completed download.
    ///
    /// Returns Some(result) if a download completed,
    /// None if no result is ready yet.
    pub fn try_recv(&self) -> Option<DownloadResult> {
        self.receiver.try_recv().ok()
    }
}

/// Performs the actual NEXRAD download using nexrad-data crate.
#[cfg(target_arch = "wasm32")]
async fn download_nexrad_data(
    site_id: &str,
    date: chrono::NaiveDate,
    cache: NexradCache,
) -> DownloadResult {
    use nexrad::data::aws::archive;

    // Generate a timestamp for the scan key (use start of day)
    let timestamp = date
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);

    let key = ScanKey::new(site_id, timestamp);

    // Check cache first
    match cache.get(&key).await {
        Ok(Some(cached)) => {
            log::info!("Cache hit for {}", key.to_storage_key());
            return DownloadResult::CacheHit(cached);
        }
        Ok(None) => {
            log::info!("Cache miss for {}", key.to_storage_key());
        }
        Err(e) => {
            log::warn!("Cache lookup failed: {}", e);
        }
    }

    // List available files for this site/date
    let files = match archive::list_files(site_id, &date).await {
        Ok(files) => files,
        Err(e) => {
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

    // Create cached scan with raw data
    let cached = CachedScan::new(key, file_name, data);

    // Store in cache
    if let Err(e) = cache.put(&cached).await {
        log::warn!("Failed to cache scan: {}", e);
    }

    DownloadResult::Success(cached)
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

    DownloadResult::Success(cached)
}
