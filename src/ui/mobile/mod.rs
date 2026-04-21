//! Mobile / touch UI.
//!
//! Phase 1: multi-touch gesture digestion for the 2D canvas (see [`gestures`]).
//! Phase 3: mobile top bar + tab-bar chrome that replaces the desktop panels
//! when [`AppState::is_mobile`](crate::state::AppState::is_mobile) is true.

pub mod gestures;
mod scrubber;
mod tabs;
mod top_bar;

pub(crate) use tabs::render_mobile_chrome;
pub(crate) use top_bar::render_mobile_top_bar;
