//! UI modules for the NEXRAD Workbench application.
//!
//! The UI is split into distinct panels:
//! - Top bar: site, product and tilt pickers and the frame readout
//! - Central canvas: radar visualization, with the map toolbar in its corner
//! - Bottom panel: timeline and transport
//! - Right panel on a rail: View (product row and tilt ladder), Layers,
//!   Events, Settings
//!
//! Chrome panels + modals dispatch through a typed [`Layer`](layout::Layer)
//! registry (see [`layout`]). Each panel and modal is a zero-sized marker
//! type that lives in its own module; [`render_layout`] walks the
//! mobile-or-desktop layout slice in z-order and calls each layer's
//! `visible`/`render` impls. The corner-chrome canvas overlays use a
//! parallel [`Overlay`](canvas_overlays::Overlay) registry inside the
//! canvas's central panel.

mod activity_chip;
mod activity_sheet;
mod alerts_modal;
mod bottom_panel;
mod canvas;
mod canvas_data_probe;
mod canvas_interaction;
mod canvas_overlays;
mod data_colors;
pub(crate) mod dialog;
mod event_modal;
mod ladder;
mod layout;
mod mobile;
mod modal_helper;
mod modal_states;
mod mping_modal;
mod overflow_menu;
mod playback_controls;
mod range_download_modal;
mod reset_settings_modal;
mod right_panel;
mod scan_inspector;
pub(crate) mod selection_chip;
mod shortcuts;
mod site_modal;
pub(crate) mod theme;
pub(crate) mod time_field;
pub(crate) mod time_format;
mod timeline;
mod toast;
mod top_bar;
mod vcp_forecast_modal;
mod vcp_forecast_serialize;

pub(crate) use canvas::render_canvas_with_geo;
pub(crate) use event_modal::EventModalState;
pub(crate) use layout::{render_layout, LayoutCtx};
pub(crate) use mobile::{resolve_mobile_auto_hide, touch_screen};
pub(crate) use modal_states::ModalStates;
pub(crate) use mping_modal::MpingModalState;
pub(crate) use shortcuts::handle_shortcuts;
pub(crate) use site_modal::SiteModalState;
