//! NWS alerts canvas overlay.
//!
//! Draws the polygon footprints of every alert whose bounding box intersects
//! the currently visible area. Each polygon is filled with a transparent
//! severity color and outlined with a solid stroke.
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

    for alert in ordered {
        let (r, g, b) = alert.severity.color();
        let fill = Color32::from_rgba_unmultiplied(r, g, b, 48);
        let stroke_color = Color32::from_rgba_unmultiplied(r, g, b, 220);
        let stroke = Stroke::new(1.5, stroke_color);

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

            let Some(outer) = projected_rings.first() else {
                continue;
            };
            if outer.len() < 3 {
                continue;
            }

            // Filled outline — use a closed path with a tinted fill. egui's
            // convex_polygon handles non-convex rings reasonably well for
            // visualization purposes; self-intersecting rings are very rare
            // in NWS alert data and if they occur, the fill will still give
            // a meaningful visual indication.
            painter.add(Shape::convex_polygon(outer.clone(), fill, Stroke::NONE));
            painter.add(Shape::closed_line(outer.clone(), stroke));

            // Hole rings: draw outline only so the user can see the cutout
            // even though we don't actually subtract it from the fill.
            for hole in projected_rings.iter().skip(1) {
                if hole.len() < 3 {
                    continue;
                }
                painter.add(Shape::closed_line(hole.clone(), stroke));
            }
        }
    }
}
