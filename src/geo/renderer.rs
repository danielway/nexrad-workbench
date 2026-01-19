//! Geographic layer rendering.
//!
//! Renders geographic features to the egui canvas.

use super::{GeoFeature, GeoLayer, GeoLayerSet, MapProjection};
use eframe::egui::{Color32, FontId, Painter, Pos2, Stroke};
use geo_types::Coord;

/// Renders all visible geographic layers to the canvas.
pub fn render_geo_layers(
    painter: &Painter,
    layers: &GeoLayerSet,
    projection: &MapProjection,
    zoom: f32,
) {
    // Render layers in order (back to front)
    for layer in layers.iter() {
        if layer.visible && zoom >= layer.layer_type.min_zoom() {
            render_layer(painter, layer, projection);
        }
    }
}

/// Renders a single geographic layer.
fn render_layer(painter: &Painter, layer: &GeoLayer, projection: &MapProjection) {
    let color = layer.effective_color();
    let line_width = layer.effective_line_width();
    let stroke = Stroke::new(line_width, color);

    for feature in &layer.features {
        render_feature(painter, feature, projection, stroke, color);
    }
}

/// Renders a single geographic feature.
fn render_feature(
    painter: &Painter,
    feature: &GeoFeature,
    projection: &MapProjection,
    stroke: Stroke,
    color: Color32,
) {
    match feature {
        GeoFeature::Point(coord, label) => {
            render_point(painter, coord, projection, color, label.as_deref());
        }
        GeoFeature::LineString(coords) => {
            render_line_string(painter, coords, projection, stroke);
        }
        GeoFeature::MultiLineString(lines) => {
            for coords in lines {
                render_line_string(painter, coords, projection, stroke);
            }
        }
        GeoFeature::Polygon(exterior, _holes) => {
            // For now, just render the exterior ring as a line
            // Full polygon filling would require tessellation
            render_line_string(painter, exterior, projection, stroke);
        }
        GeoFeature::MultiPolygon(polygons) => {
            for (exterior, _holes) in polygons {
                render_line_string(painter, exterior, projection, stroke);
            }
        }
    }
}

/// Renders a point feature (city marker, etc.).
fn render_point(
    painter: &Painter,
    coord: &Coord<f64>,
    projection: &MapProjection,
    color: Color32,
    label: Option<&str>,
) {
    // Skip if outside visible bounds
    if !projection.is_visible(*coord, 0.5) {
        return;
    }

    let pos = projection.geo_to_screen(*coord);

    // Draw a small circle for the point
    painter.circle_filled(pos, 3.0, color);

    // Draw label if present
    if let Some(text) = label {
        let label_pos = Pos2::new(pos.x + 5.0, pos.y - 5.0);
        painter.text(
            label_pos,
            eframe::egui::Align2::LEFT_BOTTOM,
            text,
            FontId::proportional(10.0),
            color,
        );
    }
}

/// Renders a line string (boundary, river, etc.).
fn render_line_string(
    painter: &Painter,
    coords: &[Coord<f64>],
    projection: &MapProjection,
    stroke: Stroke,
) {
    if coords.len() < 2 {
        return;
    }

    // Quick bounding box check for visibility
    let (min_lon, max_lon, min_lat, max_lat) = coords.iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(min_x, max_x, min_y, max_y), c| {
            (
                min_x.min(c.x),
                max_x.max(c.x),
                min_y.min(c.y),
                max_y.max(c.y),
            )
        },
    );

    if !projection.bbox_visible(min_lon, min_lat, max_lon, max_lat) {
        return;
    }

    // Convert all coordinates to screen positions
    let screen_points: Vec<Pos2> = coords
        .iter()
        .map(|c| projection.geo_to_screen(*c))
        .collect();

    // Draw line segments
    // Using individual segments instead of a path for simplicity
    for window in screen_points.windows(2) {
        if let [p1, p2] = window {
            // Skip very short segments (sub-pixel)
            let dist_sq = (p2.x - p1.x).powi(2) + (p2.y - p1.y).powi(2);
            if dist_sq > 0.5 {
                painter.line_segment([*p1, *p2], stroke);
            }
        }
    }
}

/// Simplified line rendering with Douglas-Peucker simplification.
/// Use this for very detailed geometries to improve performance.
pub fn render_line_string_simplified(
    painter: &Painter,
    coords: &[Coord<f64>],
    projection: &MapProjection,
    stroke: Stroke,
    tolerance: f64,
) {
    if coords.len() < 2 {
        return;
    }

    // Simple simplification: skip points that are very close together
    let mut simplified: Vec<Pos2> = Vec::with_capacity(coords.len() / 2);
    let mut last_pos: Option<Pos2> = None;

    for coord in coords {
        let pos = projection.geo_to_screen(*coord);

        if let Some(last) = last_pos {
            let dist_sq = (pos.x - last.x).powi(2) + (pos.y - last.y).powi(2);
            if dist_sq < (tolerance as f32).powi(2) {
                continue;
            }
        }

        simplified.push(pos);
        last_pos = Some(pos);
    }

    // Draw simplified line
    for window in simplified.windows(2) {
        if let [p1, p2] = window {
            painter.line_segment([*p1, *p2], stroke);
        }
    }
}
