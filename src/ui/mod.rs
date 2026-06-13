//! UI modules for the NEXRAD Workbench application.
//!
//! The UI is split into distinct panels:
//! - Top bar: Site context, status, and mode indicators
//! - Left panel: Radar operations (read-only state)
//! - Central canvas: Radar visualization
//! - Bottom panel: Timeline, playback controls, and session stats
//! - Right panel: Product selection, layers, and rendering controls
//!
//! Chrome panels + modals dispatch through a typed [`Layer`](layout::Layer)
//! registry (see [`layout`]). Each panel and modal is a zero-sized marker
//! type that lives in its own module; [`render_layout`] walks the
//! mobile-or-desktop layout slice in z-order and calls each layer's
//! `visible`/`render` impls. The corner-chrome canvas overlays use a
//! parallel [`Overlay`](canvas_overlays::Overlay) registry inside the
//! canvas's central panel.

pub(crate) mod acquisition_drawer;
mod alerts_modal;
mod bottom_panel;
mod canvas;
mod canvas_inspector;
mod canvas_interaction;
mod canvas_overlays;
pub(crate) mod colors;
mod event_modal;
mod layout;
mod left_panel;
mod mobile;
mod modal_helper;
mod modal_states;
mod mping_modal;
mod network_panel;
mod overflow_menu;
mod playback_controls;
mod queue_sheet;
mod range_download_modal;
mod right_panel;
mod shortcuts;
mod site_modal;
mod stats_modal;
mod timeline;
mod top_bar;
mod transport;
mod vcp_forecast_modal;
mod vcp_forecast_serialize;
mod wipe_modal;

pub use canvas::render_canvas_with_geo;
pub use event_modal::EventModalState;
pub use layout::{render_layout, LayoutCtx};
pub use modal_states::ModalStates;
pub use mping_modal::MpingModalState;
pub use shortcuts::handle_shortcuts;
pub(crate) use site_modal::{start_geolocation, LocationResult};
pub use site_modal::{trigger_geolocation, SiteModalState};
