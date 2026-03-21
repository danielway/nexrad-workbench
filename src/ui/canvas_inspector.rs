use crate::geo::MapProjection;
use crate::nexrad::RadarGpuRenderer;
use crate::state::StormCellInfo;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};
use geo_types::Coord;
use std::sync::{Arc, Mutex};

use super::canvas::format_unix_timestamp;

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_inspector(
    ui: &mut egui::Ui,
    painter: &Painter,
    projection: &MapProjection,
    hover_pos: Pos2,
    radar_lat: f64,
    radar_lon: f64,
    gpu_renderer: Option<&Arc<Mutex<RadarGpuRenderer>>>,
    product: &crate::state::RadarProduct,
    use_local_time: bool,
    sweep_params: Option<(f32, f32)>,
) {
    let geo = projection.screen_to_geo(hover_pos);
    let lat = geo.y;
    let lon = geo.x;

    // Compute polar coordinates relative to radar site
    let dlat = lat - radar_lat;
    let dlon = (lon - radar_lon) * radar_lat.to_radians().cos();
    let range_km = (dlat * dlat + dlon * dlon).sqrt() * 111.0;
    let azimuth_deg = (dlon.atan2(dlat).to_degrees() + 360.0) % 360.0;

    // Look up data value and collection time (sweep-aware when animating)
    let (value, collection_time) = gpu_renderer
        .map(|r| {
            let renderer = r.lock().expect("renderer mutex poisoned");
            let v = renderer.value_at_polar(azimuth_deg as f32, range_km, sweep_params);
            let t = renderer.collection_time_at_polar(azimuth_deg as f32, sweep_params);
            (v, t)
        })
        .unwrap_or((None, None));

    // Build tooltip text
    let mut lines = vec![
        format!("{:.4}\u{00B0}N {:.4}\u{00B0}W", lat, -lon),
        format!("Az: {:.1}\u{00B0}  Rng: {:.1} km", azimuth_deg, range_km),
    ];
    if let Some(v) = value {
        let unit = product.unit();
        if unit.is_empty() {
            lines.push(format!("{}: {:.3}", product.short_code(), v));
        } else {
            lines.push(format!("{}: {:.1} {}", product.short_code(), v, unit));
        }
    }
    if let Some(ts) = collection_time {
        lines.push(format_unix_timestamp(ts, use_local_time));
    }
    let text = lines.join("\n");

    // Draw tooltip background
    let font_id = egui::FontId::monospace(11.0);
    let galley = painter.layout_no_wrap(text.clone(), font_id.clone(), Color32::WHITE);
    let tooltip_size = galley.size();
    let padding = Vec2::new(6.0, 4.0);
    let tooltip_pos = hover_pos + Vec2::new(16.0, -tooltip_size.y - 8.0);
    let bg_rect = Rect::from_min_size(tooltip_pos - padding, tooltip_size + padding * 2.0);

    painter.rect_filled(
        bg_rect,
        4.0,
        Color32::from_rgba_unmultiplied(20, 20, 30, 220),
    );
    painter.rect_stroke(
        bg_rect,
        4.0,
        Stroke::new(1.0, Color32::from_rgb(80, 80, 100)),
        StrokeKind::Outside,
    );
    painter.galley(tooltip_pos, galley, Color32::WHITE);

    // Draw crosshair at hover position
    let cross_size = 8.0;
    let cross_color = Color32::from_rgba_unmultiplied(255, 255, 255, 160);
    painter.line_segment(
        [
            hover_pos - Vec2::new(cross_size, 0.0),
            hover_pos + Vec2::new(cross_size, 0.0),
        ],
        Stroke::new(1.0, cross_color),
    );
    painter.line_segment(
        [
            hover_pos - Vec2::new(0.0, cross_size),
            hover_pos + Vec2::new(0.0, cross_size),
        ],
        Stroke::new(1.0, cross_color),
    );

    // Request repaint for continuous hover updates
    ui.ctx().request_repaint();
}

pub(crate) fn render_distance_measurement(
    painter: &Painter,
    projection: &MapProjection,
    start: Option<(f64, f64)>,
    end: Option<(f64, f64)>,
) {
    let Some((start_lat, start_lon)) = start else {
        return;
    };

    let start_screen = projection.geo_to_screen(Coord {
        x: start_lon,
        y: start_lat,
    });

    // Draw start marker
    painter.circle_filled(start_screen, 5.0, Color32::from_rgb(255, 100, 100));
    painter.circle_stroke(start_screen, 5.0, Stroke::new(1.5, Color32::WHITE));

    if let Some((end_lat, end_lon)) = end {
        let end_screen = projection.geo_to_screen(Coord {
            x: end_lon,
            y: end_lat,
        });

        // Draw line
        painter.line_segment(
            [start_screen, end_screen],
            Stroke::new(2.0, Color32::from_rgb(255, 100, 100)),
        );

        // Draw end marker
        painter.circle_filled(end_screen, 5.0, Color32::from_rgb(255, 100, 100));
        painter.circle_stroke(end_screen, 5.0, Stroke::new(1.5, Color32::WHITE));

        // Compute great-circle distance using Haversine formula
        let distance_km = haversine_km(start_lat, start_lon, end_lat, end_lon);
        let distance_nm = distance_km * 0.539957; // nautical miles
        let distance_mi = distance_km * 0.621371; // statute miles

        // Draw label at midpoint
        let mid = Pos2::new(
            (start_screen.x + end_screen.x) / 2.0,
            (start_screen.y + end_screen.y) / 2.0,
        );
        let label = format!(
            "{:.1} km / {:.1} nm / {:.1} mi",
            distance_km, distance_nm, distance_mi
        );

        let font_id = egui::FontId::monospace(11.0);
        let galley = painter.layout_no_wrap(label, font_id, Color32::WHITE);
        let label_size = galley.size();
        let padding = Vec2::new(5.0, 3.0);
        let label_pos = mid - Vec2::new(label_size.x / 2.0, label_size.y + 8.0);
        let bg_rect = Rect::from_min_size(label_pos - padding, label_size + padding * 2.0);

        painter.rect_filled(
            bg_rect,
            3.0,
            Color32::from_rgba_unmultiplied(30, 20, 20, 220),
        );
        painter.rect_stroke(
            bg_rect,
            3.0,
            Stroke::new(1.0, Color32::from_rgb(255, 100, 100)),
            StrokeKind::Outside,
        );
        painter.galley(label_pos, galley, Color32::WHITE);
    }
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0; // Earth radius in km
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    r * c
}

pub(crate) fn render_storm_cells(
    painter: &Painter,
    projection: &MapProjection,
    cells: &[StormCellInfo],
    _dark: bool,
) {
    for cell in cells {
        let center = projection.geo_to_screen(Coord {
            x: cell.lon,
            y: cell.lat,
        });

        // Color based on max dBZ intensity
        let color = if cell.max_dbz >= 60.0 {
            Color32::from_rgb(255, 50, 50) // Severe
        } else if cell.max_dbz >= 50.0 {
            Color32::from_rgb(255, 150, 50) // Strong
        } else {
            Color32::from_rgb(255, 220, 80) // Moderate
        };

        // Draw bounding box
        let (min_lat, min_lon, max_lat, max_lon) = cell.bounds;
        let tl = projection.geo_to_screen(Coord {
            x: min_lon,
            y: max_lat,
        });
        let br = projection.geo_to_screen(Coord {
            x: max_lon,
            y: min_lat,
        });
        let bounds_rect = Rect::from_two_pos(tl, br);
        painter.rect_stroke(
            bounds_rect,
            2.0,
            Stroke::new(1.5, color),
            StrokeKind::Outside,
        );

        // Draw centroid marker
        painter.circle_stroke(center, 6.0, Stroke::new(2.0, color));

        // Label with max dBZ
        let label = format!("{:.0}", cell.max_dbz);
        painter.text(
            center + Vec2::new(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            egui::FontId::proportional(10.0),
            color,
        );
    }
}
