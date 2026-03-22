//! Streaming manager: encapsulates live streaming and backfill lifecycle.
//!
//! Owns the realtime and backfill channels and provides a unified polling API.

use super::realtime::{BackfillChannel, RealtimeChannel, RealtimeResult};
use crate::data::DataFacade;

/// Events produced by the streaming manager for the main update loop.
pub enum StreamingEvent {
    /// A realtime streaming result to process.
    Realtime(RealtimeResult),
    /// A backfill result to process.
    Backfill(RealtimeResult),
    /// The backfill has completed (channel drained and no longer active).
    BackfillComplete,
}

/// Manages live streaming and one-shot backfill channels.
pub struct StreamingManager {
    realtime_channel: RealtimeChannel,
    backfill_channel: BackfillChannel,
    backfill_in_progress: bool,
}

impl StreamingManager {
    pub fn new(realtime_channel: RealtimeChannel, backfill_channel: BackfillChannel) -> Self {
        Self {
            realtime_channel,
            backfill_channel,
            backfill_in_progress: false,
        }
    }

    /// Start live mode streaming for the given site.
    pub fn start_live(&mut self, ctx: eframe::egui::Context, site_id: String, facade: DataFacade) {
        self.realtime_channel.start(ctx, site_id, facade);
    }

    /// Start a one-shot backfill for the latest data.
    pub fn start_backfill(
        &mut self,
        ctx: eframe::egui::Context,
        site_id: String,
        facade: DataFacade,
    ) {
        self.backfill_in_progress = true;
        self.backfill_channel.start(ctx, site_id, facade);
    }

    /// Stop the realtime streaming channel.
    pub fn stop_realtime(&mut self) {
        self.realtime_channel.stop();
    }

    /// Cancel an in-progress backfill (e.g. on site change).
    pub fn cancel_backfill(&mut self) {
        self.backfill_in_progress = false;
    }

    /// Whether the realtime channel is actively streaming.
    pub fn is_realtime_active(&self) -> bool {
        self.realtime_channel.is_active()
    }

    /// Time until the next expected chunk from the realtime channel.
    pub fn time_until_next(&self) -> Option<std::time::Duration> {
        self.realtime_channel.time_until_next()
    }

    /// Drain all pending results from both channels into events.
    pub fn poll(&mut self) -> Vec<StreamingEvent> {
        let mut events = Vec::new();

        // Drain backfill results
        while let Some(result) = self.backfill_channel.try_recv() {
            events.push(StreamingEvent::Backfill(result));
        }
        // Clear backfill flag once the channel has finished and all results drained
        if self.backfill_in_progress && !self.backfill_channel.is_active() {
            self.backfill_in_progress = false;
            events.push(StreamingEvent::BackfillComplete);
        }

        // Drain realtime results
        while let Some(result) = self.realtime_channel.try_recv() {
            events.push(StreamingEvent::Realtime(result));
        }

        events
    }
}
