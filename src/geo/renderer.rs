//! Geographic layer rendering.
//!
//! Renders geographic features to the egui canvas.

use super::layer::FeatureProjection;
use super::{GeoFeature, GeoLayer, GeoLayerSet, MapProjection};
use crate::state::GeoLayerVisibility;
use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Stroke, Vec2};
use geo_types::Coord;

/// Which geo rendering pass to execute. Split so lines/markers can draw
/// below the radar texture while labels draw on top of it for legibility.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GeoPass {
    Lines,
    Labels,
}

/// Draws `text` with a 1px black outline by repeating the draw call at eight
/// surrounding offsets before drawing the colored text on top. Cheap enough
/// for the label counts we render and keeps labels legible against any
/// underlying radar color.
pub fn text_with_halo(
    painter: &Painter,
    pos: Pos2,
    align: Align2,
    text: &str,
    font: FontId,
    color: Color32,
) {
    const OFFSETS: [(f32, f32); 8] = [
        (-1.0, -1.0),
        (0.0, -1.0),
        (1.0, -1.0),
        (-1.0, 0.0),
        (1.0, 0.0),
        (-1.0, 1.0),
        (0.0, 1.0),
        (1.0, 1.0),
    ];
    for (dx, dy) in OFFSETS {
        painter.text(
            pos + Vec2::new(dx, dy),
            align,
            text,
            font.clone(),
            Color32::BLACK,
        );
    }
    painter.text(pos, align, text, font, color);
}

/// Renders all visible geographic layers to the canvas.
///
/// Visibility is passed in separately (rather than via a cloned
/// [`GeoLayerSet`]) so the large coord data never gets cloned per
/// frame. Each layer holds a projection-keyed cache of screen points
/// that is refreshed lazily here.
pub fn render_geo_layers(
    painter: &Painter,
    layers: &GeoLayerSet,
    visibility: &GeoLayerVisibility,
    projection: &MapProjection,
    zoom: f32,
    show_labels: bool,
    pass: GeoPass,
) {
    // Render layers in order (back to front)
    for (layer, visible) in layers_with_visibility(layers, visibility) {
        if visible && layer.visible && zoom >= layer.layer_type.min_zoom() {
            render_layer(painter, layer, projection, show_labels, zoom, pass);
        }
    }
}

fn layers_with_visibility<'a>(
    layers: &'a GeoLayerSet,
    visibility: &'a GeoLayerVisibility,
) -> impl Iterator<Item = (&'a GeoLayer, bool)> {
    [
        (layers.states.as_ref(), visibility.states),
        (layers.counties.as_ref(), visibility.counties),
        (layers.lakes.as_ref(), visibility.lakes),
        (layers.highways.as_ref(), visibility.highways),
        (layers.cities.as_ref(), visibility.cities),
    ]
    .into_iter()
    .filter_map(|(layer, vis)| layer.map(|l| (l, vis)))
}

/// Renders a single geographic layer.
fn render_layer(
    painter: &Painter,
    layer: &GeoLayer,
    projection: &MapProjection,
    show_labels: bool,
    zoom: f32,
    pass: GeoPass,
) {
    let color = layer.effective_color();
    let line_width = layer.effective_line_width();
    let stroke = Stroke::new(line_width, color);

    layer.refresh_projection_cache(projection);
    let entries = layer.cached_entries();

    for (feature, entry) in layer.features.iter().zip(entries.iter()) {
        render_feature(
            painter,
            feature,
            entry,
            projection,
            stroke,
            color,
            show_labels,
            zoom,
            layer.layer_type,
            pass,
        );
    }
}

/// Renders a single geographic feature.
#[allow(clippy::too_many_arguments)]
fn render_feature(
    painter: &Painter,
    feature: &GeoFeature,
    entry: &FeatureProjection,
    projection: &MapProjection,
    stroke: Stroke,
    color: Color32,
    show_labels: bool,
    zoom: f32,
    layer_type: super::GeoLayerType,
    pass: GeoPass,
) {
    let draw_lines = pass == GeoPass::Lines;
    let draw_labels = pass == GeoPass::Labels && show_labels;
    match (feature, entry) {
        (GeoFeature::Point(coord, label), _) => {
            // Pass the label through only when labels are enabled so the
            // marker still draws even with the user's label toggle off.
            let effective_label = if show_labels { label.as_deref() } else { None };
            render_point(
                painter,
                coord,
                projection,
                color,
                effective_label,
                zoom,
                pass,
            );
        }
        (GeoFeature::LineString(coords), FeatureProjection::Single(points)) if draw_lines => {
            render_projected_line(painter, coords, points, projection, stroke);
        }
        (GeoFeature::MultiLineString(lines), FeatureProjection::Multi(parts)) if draw_lines => {
            for (coords, points) in lines.iter().zip(parts.iter()) {
                render_projected_line(painter, coords, points, projection, stroke);
            }
        }
        (
            GeoFeature::Polygon {
                exterior,
                holes: _,
                label,
            },
            FeatureProjection::Single(points),
        ) => {
            if draw_lines {
                render_projected_line(painter, exterior, points, projection, stroke);
            }
            if draw_labels {
                if let Some(text) = label {
                    render_polygon_label(painter, exterior, projection, text, zoom, layer_type);
                }
            }
        }
        (GeoFeature::MultiPolygon { polygons, label }, FeatureProjection::Multi(parts)) => {
            if draw_lines {
                for ((exterior, _holes), points) in polygons.iter().zip(parts.iter()) {
                    render_projected_line(painter, exterior, points, projection, stroke);
                }
            }
            if draw_labels {
                if let Some(text) = label {
                    if let Some((largest_exterior, _)) = polygons.iter().max_by(|(a, _), (b, _)| {
                        let area_a = polygon_bbox_area(a);
                        let area_b = polygon_bbox_area(b);
                        area_a
                            .partial_cmp(&area_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                        render_polygon_label(
                            painter,
                            largest_exterior,
                            projection,
                            text,
                            zoom,
                            layer_type,
                        );
                    }
                }
            }
        }
        // Cache/feature type mismatch — shouldn't happen in practice, but
        // skip rather than reproject.
        _ => {}
    }
}

/// Computes the true geometric centroid of a polygon using the shoelace formula.
/// This gives better results than a simple vertex average for irregular shapes.
fn compute_polygon_centroid(coords: &[Coord<f64>]) -> Coord<f64> {
    if coords.is_empty() {
        return Coord { x: 0.0, y: 0.0 };
    }
    if coords.len() < 3 {
        // For degenerate cases, fall back to simple average
        let (sum_x, sum_y) = coords
            .iter()
            .fold((0.0, 0.0), |(sx, sy), c| (sx + c.x, sy + c.y));
        return Coord {
            x: sum_x / coords.len() as f64,
            y: sum_y / coords.len() as f64,
        };
    }

    let mut signed_area = 0.0;
    let mut cx = 0.0;
    let mut cy = 0.0;

    for i in 0..coords.len() {
        let j = (i + 1) % coords.len();
        let cross = coords[i].x * coords[j].y - coords[j].x * coords[i].y;
        signed_area += cross;
        cx += (coords[i].x + coords[j].x) * cross;
        cy += (coords[i].y + coords[j].y) * cross;
    }

    signed_area *= 0.5;

    // Avoid division by zero for degenerate polygons
    if signed_area.abs() < 1e-10 {
        let (sum_x, sum_y) = coords
            .iter()
            .fold((0.0, 0.0), |(sx, sy), c| (sx + c.x, sy + c.y));
        return Coord {
            x: sum_x / coords.len() as f64,
            y: sum_y / coords.len() as f64,
        };
    }

    Coord {
        x: cx / (6.0 * signed_area),
        y: cy / (6.0 * signed_area),
    }
}

/// Computes the bounding box area of a polygon (for finding largest polygon).
fn polygon_bbox_area(coords: &[Coord<f64>]) -> f64 {
    if coords.is_empty() {
        return 0.0;
    }
    let (min_x, max_x, min_y, max_y) = coords.iter().fold(
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
    (max_x - min_x) * (max_y - min_y)
}

/// Renders a label at the centroid of a polygon.
fn render_polygon_label(
    painter: &Painter,
    coords: &[Coord<f64>],
    projection: &MapProjection,
    text: &str,
    zoom: f32,
    layer_type: super::GeoLayerType,
) {
    use super::GeoLayerType;

    if coords.is_empty() {
        return;
    }

    // Check if zoom level is sufficient for labels on this layer type
    if zoom < layer_type.min_label_zoom() {
        return;
    }

    // Compute centroid using proper polygon centroid formula
    let centroid = compute_polygon_centroid(coords);

    // Skip if centroid is outside visible bounds
    if !projection.is_visible(centroid, 0.5) {
        return;
    }

    let pos = projection.geo_to_screen(centroid);

    // Style based on layer type
    let (base_size, color) = match layer_type {
        GeoLayerType::States => (12.0, Color32::from_rgb(220, 220, 240)),
        GeoLayerType::Counties => (8.0, Color32::from_rgb(210, 210, 225)),
        GeoLayerType::Cities => (10.0, Color32::from_rgb(200, 200, 220)),
        GeoLayerType::Highways => (8.0, Color32::from_rgb(130, 110, 90)),
        GeoLayerType::Lakes => (9.0, Color32::from_rgb(100, 130, 180)),
    };

    // Scale font size with zoom, clamped to reasonable range
    let font_size = (base_size * zoom).clamp(base_size * 0.7, base_size * 1.5);

    text_with_halo(
        painter,
        pos,
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(font_size),
        color,
    );
}

/// Renders a point feature (city marker, etc.). The marker circle is drawn in
/// the `Lines` pass and its label in the `Labels` pass so text can sit over
/// the radar while markers stay underneath.
fn render_point(
    painter: &Painter,
    coord: &Coord<f64>,
    projection: &MapProjection,
    color: Color32,
    label: Option<&str>,
    zoom: f32,
    pass: GeoPass,
) {
    // Skip if outside visible bounds
    if !projection.is_visible(*coord, 0.5) {
        return;
    }

    let pos = projection.geo_to_screen(*coord);
    let radius = (2.5 * zoom.sqrt()).clamp(2.0, 5.0);

    match pass {
        GeoPass::Lines => {
            painter.circle_filled(pos, radius, color);
        }
        GeoPass::Labels => {
            if let Some(text) = label {
                let font_size = (9.0 * zoom.sqrt()).clamp(8.0, 13.0);
                let label_pos = Pos2::new(pos.x + radius + 2.0, pos.y - 2.0);
                text_with_halo(
                    painter,
                    label_pos,
                    Align2::LEFT_CENTER,
                    text,
                    FontId::proportional(font_size),
                    color,
                );
            }
        }
    }
}

/// Renders a line string (boundary, river, etc.) using already-projected
/// screen points from the feature cache.
fn render_projected_line(
    painter: &Painter,
    coords: &[Coord<f64>],
    points: &[Pos2],
    projection: &MapProjection,
    stroke: Stroke,
) {
    if points.len() < 2 {
        return;
    }

    // Bounding-box visibility check is still computed in lon/lat because
    // `projection.bbox_visible` works in geo space. The coord iteration
    // is cheap (min/max only) compared to projecting every point.
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

    for window in points.windows(2) {
        if let [p1, p2] = window {
            let dist_sq = (p2.x - p1.x).powi(2) + (p2.y - p1.y).powi(2);
            if dist_sq > 0.5 {
                painter.line_segment([*p1, *p2], stroke);
            }
        }
    }
}
