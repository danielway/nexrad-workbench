//! Geographic layer rendering.
//!
//! Renders geographic features to the egui canvas. Lines and point markers
//! repaint every frame using the projection cache; labels are measured and
//! globally selected once per settled viewport, then reprojected each frame.

use super::layer::{
    CityTier, FeatureProjection, GeoLabelCandidate, GeoLabelClass, GeoLayerType, LabelEntry,
    ScreenBounds,
};
use super::{GeoFeature, GeoLayer, GeoLayerSet, MapProjection};
use crate::geo::GeoLayerVisibility;
use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Stroke, Vec2};
use eframe::epaint::Galley;
use geo_types::Coord;
use std::sync::Arc;

/// Which NEXRAD-site rendering pass to execute.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum GeoPass {
    Lines,
    Labels,
}

const HALO_OFFSETS: [Vec2; 8] = [
    Vec2::new(-1.0, -1.0),
    Vec2::new(0.0, -1.0),
    Vec2::new(1.0, -1.0),
    Vec2::new(-1.0, 0.0),
    Vec2::new(1.0, 0.0),
    Vec2::new(-1.0, 1.0),
    Vec2::new(0.0, 1.0),
    Vec2::new(1.0, 1.0),
];

/// Draws `text` with a 1px black outline by repeating the draw call at eight
/// surrounding offsets before drawing the colored text on top. Cheap enough
/// for the label counts we render and keeps labels legible against any
/// underlying radar color.
///
/// This is the immediate-mode entry point used outside the geo layer cache
/// (sweep overlay, alerts, etc.). Cached label rendering uses the lower-
/// level [`paint_galley_with_halo`] which avoids re-laying-out text every
/// frame.
pub(crate) fn text_with_halo(
    painter: &Painter,
    pos: Pos2,
    align: Align2,
    text: &str,
    font: FontId,
    color: Color32,
) {
    for offset in HALO_OFFSETS {
        painter.text(pos + offset, align, text, font.clone(), Color32::BLACK);
    }
    painter.text(pos, align, text, font, color);
}

/// Paint a pre-laid-out galley at `pos` (pre-aligned), with the same 8-way
/// halo as [`text_with_halo`]. Reuses the same `Arc<Galley>` for both the
/// halo passes and the foreground pass so text layout never repeats.
fn paint_galley_with_halo(
    painter: &Painter,
    pos: Pos2,
    galley: &Arc<Galley>,
    color: Color32,
    halo_color: Color32,
) {
    for offset in HALO_OFFSETS {
        painter.galley_with_override_text_color(pos + offset, galley.clone(), halo_color);
    }
    painter.galley_with_override_text_color(pos, galley.clone(), color);
}

/// Translate a galley's natural top-left position into the position needed
/// for a given alignment around `pos`. Mirrors `Painter::text`'s alignment
/// behavior so cached galleys land where the immediate-mode path would.
fn aligned_galley_pos(pos: Pos2, galley_size: Vec2, align: Align2) -> Pos2 {
    let x = match align.x() {
        eframe::egui::Align::Min => pos.x,
        eframe::egui::Align::Center => pos.x - galley_size.x / 2.0,
        eframe::egui::Align::Max => pos.x - galley_size.x,
    };
    let y = match align.y() {
        eframe::egui::Align::Min => pos.y,
        eframe::egui::Align::Center => pos.y - galley_size.y / 2.0,
        eframe::egui::Align::Max => pos.y - galley_size.y,
    };
    Pos2::new(x, y)
}

/// Renders lines and markers for all visible geographic layers.
///
/// Visibility is passed in separately (rather than via a cloned
/// [`GeoLayerSet`]) so the large coord data never gets cloned per
/// frame. Each layer holds a projection-keyed cache of screen points.
pub(crate) fn render_geo_layers(
    painter: &Painter,
    layers: &GeoLayerSet,
    visibility: &GeoLayerVisibility,
    projection: &MapProjection,
    zoom: f32,
) {
    for (layer, visible) in layers_with_visibility(layers, visibility) {
        if visible && layer.visible && zoom >= layer.layer_type.min_zoom() {
            render_lines_pass(painter, layer, projection, zoom);
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

/// Lines pass: draw line geometry and point markers using the projection
/// cache. Repaints every frame; per-feature visibility is rejected by
/// bounding-box checks.
fn render_lines_pass(painter: &Painter, layer: &GeoLayer, projection: &MapProjection, zoom: f32) {
    let color = layer.effective_color();
    let line_width = layer.effective_line_width();
    let stroke = Stroke::new(line_width, color);

    layer.refresh_projection_cache(projection);
    let entries = layer.cached_entries();

    for (feature, entry) in layer.features.iter().zip(entries.iter()) {
        match (feature, entry) {
            (GeoFeature::Point { coord, .. }, _) => {
                render_point_marker(painter, coord, projection, color, zoom);
            }
            (GeoFeature::LineString(coords), FeatureProjection::Single(points)) => {
                render_projected_line(painter, coords, points, projection, stroke);
            }
            (GeoFeature::MultiLineString(lines), FeatureProjection::Multi(parts)) => {
                for (coords, points) in lines.iter().zip(parts.iter()) {
                    render_projected_line(painter, coords, points, projection, stroke);
                }
            }
            (GeoFeature::Polygon { exterior, .. }, FeatureProjection::Single(points)) => {
                render_projected_line(painter, exterior, points, projection, stroke);
            }
            (GeoFeature::MultiPolygon { polygons, .. }, FeatureProjection::Multi(parts)) => {
                for ((exterior, _holes), points) in polygons.iter().zip(parts.iter()) {
                    render_projected_line(painter, exterior, points, projection, stroke);
                }
            }
            _ => {}
        }
    }
}

/// Collect and measure all eligible labels for one global placement decision.
pub(crate) fn build_geo_label_candidates(
    painter: &Painter,
    layers: &GeoLayerSet,
    visibility: &GeoLayerVisibility,
    projection: &MapProjection,
    zoom: f32,
    dark: bool,
) -> Vec<GeoLabelCandidate> {
    if !visibility.labels {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    let mut source_order = 0;
    for (layer, visible) in layers_with_visibility(layers, visibility) {
        if !visible
            || !layer.visible
            || zoom < layer.layer_type.min_zoom()
            || zoom < layer.layer_type.min_label_zoom()
        {
            source_order += layer.features.len();
            continue;
        }

        for feature in &layer.features {
            let current_source_order = source_order;
            source_order += 1;

            let Some(text) = feature.label_text() else {
                continue;
            };
            let Some(anchor) = feature.label_anchor() else {
                continue;
            };
            if !projection.is_visible(anchor, 0.5) {
                continue;
            }

            let class = label_class(layer.layer_type, feature);
            let entry = match feature {
                GeoFeature::Polygon { .. } | GeoFeature::MultiPolygon { .. } => {
                    build_polygon_label_entry(anchor, text, zoom, layer.layer_type, class, dark)
                }
                GeoFeature::Point { .. } => {
                    Some(build_point_label_entry(anchor, text, zoom, class, dark))
                }
                _ => None,
            };
            let Some(entry) = entry else {
                continue;
            };

            let galley = painter.layout_no_wrap(
                entry.text.clone(),
                FontId::proportional(entry.font_size),
                entry.color,
            );
            let anchor_screen = projection.geo_to_screen(entry.anchor) + entry.pixel_offset;
            let pos = aligned_galley_pos(anchor_screen, galley.size(), entry.align);
            candidates.push(GeoLabelCandidate {
                entry,
                bounds: ScreenBounds {
                    min_x: pos.x,
                    min_y: pos.y,
                    max_x: pos.x + galley.size().x,
                    max_y: pos.y + galley.size().y,
                },
                source_order: current_source_order,
            });
        }
    }

    candidates
}

fn label_class(layer_type: GeoLayerType, feature: &GeoFeature) -> GeoLabelClass {
    match layer_type {
        GeoLayerType::States => GeoLabelClass::State,
        GeoLayerType::Counties => GeoLabelClass::County,
        GeoLayerType::Highways => GeoLabelClass::Highway,
        GeoLayerType::Lakes => GeoLabelClass::Lake,
        GeoLayerType::Cities => match feature {
            GeoFeature::Point {
                city_tier: Some(CityTier::Major),
                ..
            } => GeoLabelClass::CityMajor,
            GeoFeature::Point {
                city_tier: Some(CityTier::Medium),
                ..
            } => GeoLabelClass::CityMedium,
            _ => GeoLabelClass::CitySmall,
        },
    }
}

fn build_polygon_label_entry(
    anchor: Coord<f64>,
    text: &str,
    zoom: f32,
    layer_type: GeoLayerType,
    class: GeoLabelClass,
    _dark: bool,
) -> Option<LabelEntry> {
    let (base_size, color) = polygon_label_style(layer_type);
    let font_size = (base_size * zoom).clamp(base_size * 0.7, base_size * 1.5);
    Some(LabelEntry {
        anchor,
        align: Align2::CENTER_CENTER,
        pixel_offset: Vec2::ZERO,
        text: text.to_string(),
        font_size,
        color,
        class,
    })
}

fn build_point_label_entry(
    anchor: Coord<f64>,
    text: &str,
    zoom: f32,
    class: GeoLabelClass,
    _dark: bool,
) -> LabelEntry {
    let radius = (2.5 * zoom.sqrt()).clamp(2.0, 5.0);
    let font_size = (9.0 * zoom.sqrt()).clamp(8.0, 13.0);
    let color = Color32::from_rgb(180, 180, 200);
    LabelEntry {
        anchor,
        align: Align2::LEFT_CENTER,
        pixel_offset: Vec2::new(radius + 2.0, -2.0),
        text: text.to_string(),
        font_size,
        color,
        class,
    }
}

fn polygon_label_style(layer_type: GeoLayerType) -> (f32, Color32) {
    match layer_type {
        GeoLayerType::States => (12.0, Color32::from_rgb(220, 220, 240)),
        GeoLayerType::Counties => (8.0, Color32::from_rgb(210, 210, 225)),
        GeoLayerType::Cities => (10.0, Color32::from_rgb(200, 200, 220)),
        GeoLayerType::Highways => (8.0, Color32::from_rgb(130, 110, 90)),
        GeoLayerType::Lakes => (9.0, Color32::from_rgb(100, 130, 180)),
    }
}

/// Per-frame label paint: lay out each cached entry's text (memoized by
/// egui's internal galley cache, which self-invalidates on font-atlas
/// recreate) and project its anchor through the current projection before
/// emitting one halo'd galley.
pub(crate) fn paint_geo_labels(
    painter: &Painter,
    projection: &MapProjection,
    visibility: &GeoLayerVisibility,
    entries: &[LabelEntry],
) {
    if !visibility.labels {
        return;
    }

    for entry in entries {
        if !label_class_enabled(entry.class, visibility) {
            continue;
        }
        let galley = painter.layout_no_wrap(
            entry.text.clone(),
            FontId::proportional(entry.font_size),
            entry.color,
        );
        let anchor_screen = projection.geo_to_screen(entry.anchor);
        let anchor_with_offset = anchor_screen + entry.pixel_offset;
        let pos = aligned_galley_pos(anchor_with_offset, galley.size(), entry.align);
        paint_galley_with_halo(painter, pos, &galley, entry.color, Color32::BLACK);
    }
}

fn label_class_enabled(class: GeoLabelClass, visibility: &GeoLayerVisibility) -> bool {
    match class {
        GeoLabelClass::State => visibility.states,
        GeoLabelClass::CityMajor | GeoLabelClass::CityMedium | GeoLabelClass::CitySmall => {
            visibility.cities
        }
        GeoLabelClass::Highway => visibility.highways,
        GeoLabelClass::Lake => visibility.lakes,
        GeoLabelClass::County => visibility.counties,
    }
}

/// Renders a point feature's marker dot. Point labels use the global label pass.
fn render_point_marker(
    painter: &Painter,
    coord: &Coord<f64>,
    projection: &MapProjection,
    color: Color32,
    zoom: f32,
) {
    if !projection.is_visible(*coord, 0.5) {
        return;
    }
    let pos = projection.geo_to_screen(*coord);
    let radius = (2.5 * zoom.sqrt()).clamp(2.0, 5.0);
    painter.circle_filled(pos, radius, color);
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
