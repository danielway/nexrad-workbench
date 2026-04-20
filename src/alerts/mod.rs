//! NWS Alerts integration.
//!
//! Polls the public National Weather Service alerts API
//! (`https://api.weather.gov/alerts/active`) and exposes currently active
//! alerts to the UI layer. Alerts intersecting the current viewing area
//! are surfaced in the top bar; a toggleable canvas overlay renders the
//! polygon footprints on the 2D map; and a modal displays full alert
//! details on click.

mod api;
mod channel;
mod geometry;
mod manager;
mod parse;
mod types;

pub use geometry::{bbox_intersects, contains_point};
pub use manager::AlertsManager;
pub use types::{Alert, AlertSeverity};
