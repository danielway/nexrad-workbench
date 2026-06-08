//! NWS alerts canvas overlay.
//!
//! Draws the polygon footprints of every alert whose bounding box intersects
//! the currently visible area as an event-type-colored outline.
//!
//! Only runs in 2D flat mode.

use crate::alerts::{bbox_intersects, Alert};
use crate::geo::MapProjection;
use eframe::egui::{Color32, Painter, Pos2, Shape, Stroke};
use geo_types::Coord;

/// Render alert polygons on top of the radar view.
pub(crate) fn render_alerts(painter: &Painter, projection: &MapProjection, alerts: &[Alert]) {
    let bounds = projection.visible_bounds();

    // Sort lowest → highest severity so highest draws on top. We iterate in
    // that order without mutating the caller's slice.
    let mut ordered: Vec<&Alert> = alerts
        .iter()
        .filter(|a| bbox_intersects(a, bounds))
        .collect();
    ordered.sort_by_key(|a| a.severity.rank());

    // Slightly wider black halo underneath so colored strokes stay legible
    // against bright radar fills.
    let halo_stroke = Stroke::new(4.5, Color32::BLACK);

    for alert in ordered {
        let (r, g, b) = alert.color();
        let stroke_color = Color32::from_rgba_unmultiplied(r, g, b, 220);
        let stroke = Stroke::new(2.5, stroke_color);

        for polygon in &alert.geometry.polygons {
            // Project all rings once.
            let projected_rings: Vec<Vec<Pos2>> = polygon
                .iter()
                .map(|ring| {
                    ring.iter()
                        .map(|&(lon, lat)| projection.geo_to_screen(Coord { x: lon, y: lat }))
                        .collect()
                })
                .collect();

            for ring in &projected_rings {
                if ring.len() < 3 {
                    continue;
                }
                painter.add(Shape::closed_line(ring.clone(), halo_stroke));
                painter.add(Shape::closed_line(ring.clone(), stroke));
            }
        }
    }
}
