//! Acquisition coordinator: owns the download pipeline and archive index.
//!
//! Consolidates download channel, cache load channel, download queue,
//! archive index, and current scan into a single owner.

use super::archive_index::ArchiveIndex;
use super::cache_channel::CacheLoadChannel;
use super::download::{DownloadChannel, NetworkStats};
use super::download_queue::DownloadQueueManager;
use crate::data::MainThreadStore;

/// Owns the download pipeline: channels, queue, archive index.
pub(crate) struct AcquisitionCoordinator {
    /// Channel for async NEXRAD download operations.
    pub(crate) download_channel: DownloadChannel,
    /// Channel for async cache metadata loading.
    pub(crate) cache_load_channel: CacheLoadChannel,
    /// Manages the queue of files to download.
    pub(crate) download_queue: DownloadQueueManager,
    /// Cache for archive file listings (by site/date).
    pub(crate) archive_index: ArchiveIndex,
    /// Record-based data facade.
    pub(crate) data_facade: MainThreadStore,
}

impl AcquisitionCoordinator {
    pub(crate) fn new(data_facade: MainThreadStore) -> Self {
        let download_channel = DownloadChannel::new();
        let cache_load_channel = CacheLoadChannel::new();

        Self {
            download_channel,
            cache_load_channel,
            download_queue: DownloadQueueManager::new(),
            archive_index: ArchiveIndex::new(),
            data_facade,
        }
    }

    /// Get the download channel stats (for realtime/backfill channel init).
    pub(crate) fn download_stats(&self) -> NetworkStats {
        self.download_channel.stats()
    }

    /// Get the data facade (for worker ingest, downloads, etc.).
    pub(crate) fn facade(&self) -> &MainThreadStore {
        &self.data_facade
    }
}
