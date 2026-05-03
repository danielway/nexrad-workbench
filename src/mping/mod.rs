//! mPING storm reports integration.
//!
//! Fetches crowd-sourced storm reports from the mPING v2 API
//! (`https://mping.ou.edu`) within ±30 min of the current playback
//! position and 300 km of the active radar site, and renders them as
//! colored markers on the 2D canvas. The user supplies their own API
//! key (registered separately at https://mping.ou.edu/registration/);
//! the key is persisted via `UserPreferences`.

mod api;
mod channel;
mod manager;
mod parse;
mod types;

pub use manager::MpingManager;
pub use types::{ReportCategory, StormReport};
