//! UI modules for the NEXRAD Workbench application.
//!
//! The UI is split into distinct panels:
//! - Top bar: Site context, status, and mode indicators
//! - Left panel: Radar operations (read-only state)
//! - Central canvas: Radar visualization
//! - Bottom panel: Timeline, playback controls, and session stats
//! - Right panel: Product selection, layers, and rendering controls

pub(crate) mod acquisition_drawer;
mod alerts_modal;
mod bottom_panel;
mod canvas;
mod canvas_inspector;
mod canvas_interaction;
mod canvas_overlays;
pub(crate) mod colors;
mod event_modal;
mod left_panel;
mod mobile;
mod modal_helper;
mod network_panel;
mod playback_controls;
mod right_panel;
mod shortcuts;
mod site_modal;
mod stats_modal;
mod timeline;
mod top_bar;
mod vcp_forecast_modal;
mod wipe_modal;

pub use alerts_modal::render_alerts_modals;
pub use bottom_panel::render_bottom_panel;
pub use canvas::render_canvas_with_geo;
pub use event_modal::{render_event_modal, EventModalState};
pub use left_panel::render_left_panel;
pub(crate) use mobile::{render_mobile_chrome, render_mobile_top_bar};
pub use network_panel::render_network_log;
pub use right_panel::render_right_panel;
pub use shortcuts::{handle_shortcuts, render_shortcuts_help};
pub use site_modal::{render_site_modal, SiteModalState};
pub use stats_modal::render_stats_modal;
pub use top_bar::render_top_bar;
pub use vcp_forecast_modal::render_vcp_forecast_modal;
pub use wipe_modal::render_wipe_modal;
