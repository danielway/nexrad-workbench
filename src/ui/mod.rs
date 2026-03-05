//! UI modules for the NEXRAD Workbench application.
//!
//! The UI is split into distinct panels:
//! - Top bar: Site context, status, and mode indicators
//! - Left panel: Radar operations (read-only state)
//! - Central canvas: Radar visualization
//! - Bottom panel: Timeline, playback controls, and session stats
//! - Right panel: Product selection, layers, and rendering controls

mod bottom_panel;
mod canvas;
pub(crate) mod colors;
mod left_panel;
mod right_panel;
mod shortcuts;
mod site_modal;
mod stats_modal;
mod top_bar;
mod wipe_modal;

pub use bottom_panel::render_bottom_panel;
pub use canvas::render_canvas_with_geo;
pub use left_panel::render_left_panel;
pub use right_panel::render_right_panel;
pub use shortcuts::{handle_shortcuts, render_shortcuts_help};
pub use site_modal::{render_site_modal, SiteModalState};
pub use stats_modal::render_stats_modal;
pub use top_bar::render_top_bar;
pub use wipe_modal::render_wipe_modal;
