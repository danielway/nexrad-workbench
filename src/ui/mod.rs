//! UI modules for the NEXRAD Workbench application.
//!
//! The UI is split into distinct panels:
//! - Top bar: Title, status, and data source mode selector
//! - Left panel: Data source controls (varies by mode)
//! - Central canvas: Radar visualization
//! - Bottom panel: Playback controls
//! - Right panel: Layer and processing controls

mod bottom_panel;
mod canvas;
mod left_panel;
mod right_panel;
mod top_bar;

pub use bottom_panel::render_bottom_panel;
pub use canvas::render_canvas;
pub use left_panel::render_left_panel;
pub use right_panel::render_right_panel;
pub use top_bar::render_top_bar;
