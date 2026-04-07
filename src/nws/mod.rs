//! NWS (National Weather Service) alerts integration.
//!
//! Polls the NWS alerts API for active weather warnings and watches,
//! providing data for status bar badges and canvas polygon overlays.

pub mod alerts;
pub mod fetch;

pub use alerts::{event_abbreviation, event_color, NwsAlert};
pub use fetch::{NwsAlertPoller, NwsAlertResult};
