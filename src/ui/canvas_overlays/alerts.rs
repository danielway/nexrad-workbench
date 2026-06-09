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
///
/// `show_warnings` / `show_other` gate the two product classes independently so
/// the layers panel can toggle warnings and watches/advisories separately.
pub(crate) fn render_alerts(
    painter: &Painter,
    projection: &MapProjection,
    alerts: &[Alert],
    show_warnings: bool,
    show_other: bool,
) {
    let bounds = projection.visible_bounds();

    let mut ordered: Vec<&Alert> = alerts
        .iter()
        .filter(|a| {
            if a.is_warning() {
                show_warnings
            } else {
                show_other
            }
        })
        .filter(|a| bbox_intersects(a, bounds))
        .collect();
    // Warnings paint on top of watches, and within a class higher severity wins
    // (false < true, so non-warnings sort first and draw underneath).
    ordered.sort_by_key(|a| (a.is_warning(), a.severity.rank()));

    // Slightly wider black halo underneath so colored strokes stay legible
    // against bright radar fills. Only warnings get it — watches stay subdued
    // so the urgent warnings stand out.
    let halo_stroke = Stroke::new(4.5, Color32::BLACK);

    for alert in ordered {
        let warning = alert.is_warning();
        let (r, g, b) = alert.color();
        // Watches render thinner and more transparent than warnings.
        let (width, alpha) = if warning { (2.5, 220) } else { (1.5, 110) };
        let stroke = Stroke::new(width, Color32::from_rgba_unmultiplied(r, g, b, alpha));

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
                if warning {
                    painter.add(Shape::closed_line(ring.clone(), halo_stroke));
                }
                painter.add(Shape::closed_line(ring.clone(), stroke));
            }
        }
    }
}
