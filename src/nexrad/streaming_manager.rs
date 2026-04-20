//! Streaming manager: encapsulates live streaming lifecycle.
//!
//! Wraps the realtime channel and provides a unified polling API.

use super::realtime::{RealtimeChannel, RealtimeResult};
use crate::data::DataFacade;

/// Events produced by the streaming manager for the main update loop.
pub enum StreamingEvent {
    /// A realtime streaming result to process.
    Realtime(RealtimeResult),
}

/// Manages the live streaming channel.
pub struct StreamingManager {
    realtime_channel: RealtimeChannel,
}

impl StreamingManager {
    pub fn new(realtime_channel: RealtimeChannel) -> Self {
        Self { realtime_channel }
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

    /// Drain all pending results from the realtime channel into events.
    pub fn poll(&mut self) -> Vec<StreamingEvent> {
        let mut events = Vec::new();
        while let Some(result) = self.realtime_channel.try_recv() {
            events.push(StreamingEvent::Realtime(result));
        }
        events
    }
}
