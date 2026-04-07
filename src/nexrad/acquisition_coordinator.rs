//! Acquisition coordinator: owns the download pipeline and archive index.
//!
//! Consolidates download channel, cache load channel, download queue,
//! archive index, and current scan into a single owner.

use crate::data::DataFacade;
use crate::nexrad::archive_index::ArchiveIndex;
use crate::nexrad::cache_channel::{CacheLoadChannel, CacheLoadResult};
use crate::nexrad::download::{DownloadChannel, NetworkStats};
use crate::nexrad::download_queue::DownloadQueueManager;
use crate::nexrad::types::{CachedScan, DownloadResult};
use crate::nexrad::ListingResult;
use crate::nexrad::ScanBoundary;

/// A deferred download waiting for one or more archive listings to arrive.
#[derive(Clone, Debug)]
pub(crate) struct PendingDownload {
    /// `true` = position download, `false` = selection download.
    pub is_position: bool,
    /// Dates whose listings have already been re-fetched (stale-listing retry).
    /// Each date is allowed at most one re-fetch to avoid infinite loops.
    pub refetched_dates: std::collections::HashSet<chrono::NaiveDate>,
}

/// Owns the download pipeline: channels, queue, archive index, and current scan.
pub struct AcquisitionCoordinator {
    /// Channel for async NEXRAD download operations.
    pub(crate) download_channel: DownloadChannel,
    /// Channel for async cache metadata loading.
    pub(crate) cache_load_channel: CacheLoadChannel,
    /// Manages the queue of files to download.
    pub(crate) download_queue: DownloadQueueManager,
    /// Cache for archive file listings (by site/date).
    pub(crate) archive_index: ArchiveIndex,
    /// Currently loaded NEXRAD scan.
    pub(crate) current_scan: Option<CachedScan>,
    /// Record-based data facade.
    pub(crate) data_facade: DataFacade,
    /// A download waiting for archive listings before it can build its queue.
    /// Set when a listing is missing or stale; cleared when queue building
    /// completes or is abandoned.
    pub(crate) pending_download: Option<PendingDownload>,
}

#[allow(dead_code)]
impl AcquisitionCoordinator {
    pub fn new(data_facade: DataFacade) -> Self {
        let download_channel = DownloadChannel::new();
        let cache_load_channel = CacheLoadChannel::new();

        Self {
            download_channel,
            cache_load_channel,
            download_queue: DownloadQueueManager::new(),
            archive_index: ArchiveIndex::new(),
            current_scan: None,
            data_facade,
            pending_download: None,
        }
    }

    /// Get the download channel stats (for realtime/backfill channel init).
    pub fn download_stats(&self) -> NetworkStats {
        self.download_channel.stats()
    }

    /// Get network stats for session stat updates.
    pub fn network_stats(&self) -> NetworkStats {
        self.download_channel.stats()
    }

    /// Try to receive a cache load result.
    pub fn try_recv_cache_load(&mut self) -> Option<CacheLoadResult> {
        self.cache_load_channel.try_recv()
    }

    /// Try to receive a download result.
    pub fn try_recv_download(&mut self) -> Option<DownloadResult> {
        self.download_channel.try_recv()
    }

    /// Try to receive a listing result.
    pub fn try_recv_listing(&mut self) -> Option<ListingResult> {
        self.download_channel.try_recv_listing()
    }

    /// Whether the cache load channel is currently loading.
    pub fn is_cache_loading(&self) -> bool {
        self.cache_load_channel.is_loading()
    }

    /// Load site timeline from cache.
    pub fn load_site_timeline(&self, ctx: eframe::egui::Context, site_id: String) {
        self.cache_load_channel
            .load_site_timeline(ctx, self.data_facade.clone(), site_id);
    }

    /// Clear the record cache.
    pub fn clear_cache(&self, ctx: eframe::egui::Context) {
        self.cache_load_channel
            .clear_cache(ctx, self.data_facade.clone());
    }

    /// Get the data facade (for worker ingest, downloads, etc.).
    pub fn facade(&self) -> &DataFacade {
        &self.data_facade
    }

    /// Store the current scan.
    pub fn set_current_scan(&mut self, scan: CachedScan) {
        self.current_scan = Some(scan);
    }

    /// Get all scan boundaries for a site from the archive index.
    pub fn all_boundaries_for_site(&self, site_id: &str) -> Vec<ScanBoundary> {
        self.archive_index.all_boundaries_for_site(site_id)
    }

    /// Insert a listing into the archive index.
    pub fn insert_listing(
        &mut self,
        site_id: &str,
        date: chrono::NaiveDate,
        listing: crate::nexrad::archive_index::ArchiveListing,
    ) {
        self.archive_index.insert(site_id, date, listing);
    }
}
