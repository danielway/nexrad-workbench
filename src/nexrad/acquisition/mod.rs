//! Archive data acquisition: everything that brings scan bytes into the app.
//! Covers AWS S3 listings and downloads ([`download`]), the pure download-queue
//! state machine ([`download_queue`]), per-day archive listing indexes
//! ([`archive_index`]), IndexedDB cache-load channels ([`cache_channel`]),
//! and the shared result types ([`types`]) — all coordinated by
//! [`acquisition_coordinator::AcquisitionCoordinator`].

pub(crate) mod acquisition_coordinator;
pub(crate) mod archive_index;
pub(crate) mod cache_channel;
pub(crate) mod download;
pub(crate) mod download_queue;
pub(crate) mod types;
