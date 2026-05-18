//! Canvas overlay components drawn on top of the radar image.
//!
//! Each overlay is a self-contained drawing function that paints into the
//! canvas `Rect` using the egui `Painter`. Overlays are drawn in painter
//! order after the radar texture and geographic layers.

mod alerts;
mod color_scale;
mod compass;
mod globe;
mod gps_location;
mod info;
mod mping;
mod national_mosaic;
mod scale_bar;
mod sites;
mod sweep;

pub(crate) use alerts::render_alerts;
pub(crate) use color_scale::draw_color_scale;
pub(crate) use compass::draw_compass;
pub(crate) use globe::draw_globe;
pub(crate) use gps_location::render_gps_location;
pub(crate) use info::draw_overlay_info;
pub(crate) use mping::{render_mping_detail, render_mping_reports};
pub(crate) use national_mosaic::{draw_national_mosaic, RadarCutout};
pub(crate) use scale_bar::draw_scale_bar;
pub(crate) use sites::render_nexrad_sites;
pub(crate) use sweep::render_radar_sweep;
