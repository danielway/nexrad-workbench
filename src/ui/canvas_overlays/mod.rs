//! Canvas overlay components drawn on top of the radar image.
//!
//! Each overlay is a self-contained drawing function that paints into the
//! canvas `Rect` using the egui `Painter`. Overlays are drawn in painter
//! order after the radar texture and geographic layers.

mod color_scale;
mod compass;
mod globe;
mod info;
mod national_mosaic;
mod sites;
mod sweep;

pub(crate) use color_scale::draw_color_scale;
pub(crate) use compass::draw_compass;
pub(crate) use globe::draw_globe;
pub(crate) use info::draw_overlay_info;
pub(crate) use national_mosaic::draw_national_mosaic;
pub(crate) use sites::render_nexrad_sites;
pub(crate) use sweep::render_radar_sweep;
