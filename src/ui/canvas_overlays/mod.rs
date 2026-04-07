//! Canvas overlay components drawn on top of the radar image.
//!
//! Each overlay is a self-contained drawing function that paints into the
//! canvas `Rect` using the egui `Painter`. Overlays are drawn in painter
//! order after the radar texture and geographic layers.

mod alerts;
mod color_scale;
mod compass;
mod globe;
mod info;
mod sites;
mod sweep;

pub(crate) use alerts::render_nws_alerts;
pub(crate) use color_scale::draw_color_scale;
pub(crate) use compass::draw_compass;
pub(crate) use globe::draw_globe;
pub(crate) use info::draw_overlay_info;
pub(crate) use sites::render_nexrad_sites;
pub(crate) use sweep::render_radar_sweep;
