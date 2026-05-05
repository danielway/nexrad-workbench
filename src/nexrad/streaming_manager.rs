//! Streaming manager: encapsulates live streaming lifecycle.
//!
//! Wraps the realtime channel and provides a unified polling API.

use super::realtime::{RealtimeChannel, RealtimeResult, StreamingFilter};
use crate::data::DataFacade;
use crate::state::ElevationSelection;

/// Events produced by the streaming manager for the main update loop.
pub enum StreamingEvent {
    /// A realtime streaming result to process.
    Realtime(RealtimeResult),
}

/// Manages the live streaming channel.
pub struct StreamingManager {
    realtime_channel: RealtimeChannel,
    /// Last filter pushed to the realtime channel. Tracked here so the
    /// per-frame UI sync can diff against it and avoid bumping the channel's
    /// filter epoch when nothing changed.
    last_pushed_filter: StreamingFilter,
}

impl StreamingManager {
    pub fn new(realtime_channel: RealtimeChannel) -> Self {
        Self {
            realtime_channel,
            last_pushed_filter: StreamingFilter::default(),
        }
    }

    /// Start live mode streaming for the given site.
    pub fn start_live(&mut self, ctx: eframe::egui::Context, site_id: String, facade: DataFacade) {
        self.realtime_channel.start(ctx, site_id, facade);
    }

    /// Stop the realtime streaming channel.
    pub fn stop_realtime(&mut self) {
        self.realtime_channel.stop();
    }

    /// Whether the realtime channel is actively streaming.
    pub fn is_realtime_active(&self) -> bool {
        self.realtime_channel.is_active()
    }

    /// Time until the next expected chunk from the realtime channel.
    pub fn time_until_next(&self) -> Option<std::time::Duration> {
        self.realtime_channel.time_until_next()
    }

    /// Push the latest radial collection time (Unix seconds) of the chunk
    /// just ingested into the streaming loop, so the next projection
    /// anchors on the current chunk's true collection time.
    pub fn record_chunk_collection_end_secs(&self, secs: f64) {
        self.realtime_channel.record_chunk_collection_end_secs(secs);
    }

    /// Push the empirical per-chunk availability lag (S3 upload − chunk
    /// collection time, seconds) from the most recent worker ingest down
    /// into the streaming loop.
    pub fn record_availability_lag_secs(&self, lag_secs: f64) {
        self.realtime_channel.record_availability_lag_secs(lag_secs);
    }

    /// Push the user's elevation selection down to the realtime channel as
    /// a [`StreamingFilter`]. Called once per frame; only forwards when the
    /// derived filter differs from what was last pushed so the channel's
    /// filter epoch only bumps on real changes.
    pub fn sync_filter(&mut self, selection: &ElevationSelection) {
        let new_filter = StreamingFilter::from(selection);
        if new_filter == self.last_pushed_filter {
            return;
        }
        self.realtime_channel.set_filter(new_filter);
        self.last_pushed_filter = new_filter;
    }

    /// Drain all pending results from the realtime channel into events.
    pub fn poll(&mut self) -> Vec<StreamingEvent> {
        let mut events = Vec::new();
        while let Some(result) = self.realtime_channel.try_recv() {
            events.push(StreamingEvent::Realtime(result));
        }
        events
    }
}
