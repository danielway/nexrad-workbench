//! "You are here" GPS marker overlay.
//!
//! Draws a single styled dot at the user's last-known GPS coordinates when
//! the corresponding layer toggle is enabled. Coordinates come from a
//! one-shot `navigator.geolocation.get_current_position` call kicked off
//! by the layer checkbox.

use crate::geo::MapProjection;
use eframe::egui::{Color32, Painter, Stroke};
use geo_types::Coord;

pub(crate) fn render_gps_location(
    painter: &Painter,
    projection: &MapProjection,
    coords: (f64, f64),
) {
    let (lat, lon) = coords;
    let screen_pos = projection.geo_to_screen(Coord { x: lon, y: lat });
    painter.circle_filled(screen_pos, 6.0, Color32::from_rgb(33, 150, 243));
    painter.circle_stroke(screen_pos, 6.0, Stroke::new(2.0, Color32::WHITE));
}
