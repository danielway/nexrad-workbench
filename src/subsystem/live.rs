//! Live subsystem: real-time NEXRAD streaming.
//!
//! Currently owns just the [`RealtimeChannel`] (the worker-driven
//! streaming-loop handle + observation queue). Slated to grow to
//! include [`LiveModeState`](crate::state::LiveModeState),
//! [`LiveRadarModel`](crate::state::LiveRadarModel), and `app_mode` as
//! future PRs migrate them off [`AppState`]; consolidating those into
//! one owner is what gives the S2 async cleanup (`Rc<RefCell>` →
//! channels in `RealtimeChannel`) somewhere clean to land.

use crate::nexrad::RealtimeChannel;

/// Owner of the real-time streaming pipeline.
pub struct Live {
    /// Worker-driven streaming-loop handle. Drains observation +
    /// chunk results each frame.
    pub channel: RealtimeChannel,
}

impl Live {
    pub fn new(channel: RealtimeChannel) -> Self {
        Self { channel }
    }
}
