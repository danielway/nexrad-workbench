//! Canvas overlay for NWS weather alert polygons.

use crate::geo::MapProjection;
use crate::nws::{self, NwsAlert};
use eframe::egui::{self, Color32, Painter, Stroke};

/// Render NWS alert polygons on the 2D canvas.
pub(crate) fn render_nws_alerts(
    painter: &Painter,
    projection: &MapProjection,
    alerts: &[NwsAlert],
) {
    for alert in alerts {
        let polygon = match alert.polygon {
            Some(ref p) if p.len() >= 3 => p,
            _ => continue,
        };

        // Cull alerts outside the visible area
        if let Some((min_lon, min_lat, max_lon, max_lat)) = alert.bbox {
            if !projection.bbox_visible(min_lon, min_lat, max_lon, max_lat) {
                continue;
            }
        }

        let color = nws::event_color(&alert.event, alert.severity);
        let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 50);
        let stroke = Stroke::new(2.0, color);

        let points: Vec<egui::Pos2> = polygon
            .iter()
            .map(|coord| projection.geo_to_screen(*coord))
            .collect();

        // Use PathShape for potentially non-convex polygons
        painter.add(egui::Shape::Path(egui::epaint::PathShape {
            points,
            closed: true,
            fill,
            stroke: stroke.into(),
        }));
    }
}
