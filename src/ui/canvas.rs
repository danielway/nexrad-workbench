//! Central canvas UI: radar visualization area.

use super::colors::{canvas as canvas_colors, radar, sites as site_colors};
use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::{GeoLayerSet, MapProjection};
use crate::nexrad::{
    radar_coverage_range_km, render_sweep_to_image, RadarCacheKey, RadarTextureCache,
};
use crate::state::{AlertsState, AppState, GeoLayerVisibility, NwsAlert};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use geo_types::Coord;
use nexrad::prelude::Volume;
use std::f32::consts::PI;

/// Render canvas with optional geographic layers and NEXRAD data.
///
/// Radar data is rendered using the `nexrad-render` crate which produces
/// images that are cached as textures for efficient display.
pub fn render_canvas_with_geo(
    ctx: &egui::Context,
    state: &mut AppState,
    geo_layers: Option<&GeoLayerSet>,
    decoded_volume: Option<&Volume>,
    texture_cache: &mut RadarTextureCache,
) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let available_size = ui.available_size();

        // Allocate the full available space for the canvas
        let (response, painter) = ui.allocate_painter(available_size, Sense::click_and_drag());

        let rect = response.rect;

        // Draw background
        painter.rect_filled(rect, 0.0, canvas_colors::BACKGROUND);

        // Create projection for geo layers
        let mut projection =
            MapProjection::new(state.viz_state.center_lat, state.viz_state.center_lon);
        projection.update(state.viz_state.zoom, state.viz_state.pan_offset, rect);

        // Draw geographic layers BEFORE radar (so radar appears on top)
        if let Some(layers) = geo_layers {
            // Create a filtered layer set based on visibility settings
            let filtered = filter_geo_layers(layers, &state.layer_state.geo);
            crate::geo::render_geo_layers(
                &painter,
                &filtered,
                &projection,
                state.viz_state.zoom,
                state.layer_state.geo.labels,
            );
        }

        // Draw NEXRAD sites layer (always show current site, optionally show all)
        render_nexrad_sites(
            &painter,
            &projection,
            &state.viz_state.site_id,
            &state.layer_state.geo,
        );

        // Draw NEXRAD radar data if available (texture-based rendering)
        if let Some(volume) = decoded_volume {
            render_radar_texture(
                ctx,
                &painter,
                &projection,
                volume,
                texture_cache,
                &rect,
                state.viz_state.center_lat,
                state.viz_state.center_lon,
            );
        }

        // Draw NWS alerts layer if enabled
        if state.layer_state.nws_alerts {
            let current_time = state
                .playback_state
                .selected_timestamp
                .unwrap_or(1714564800.0);
            render_nws_alerts(&painter, &projection, &state.alerts_state, current_time);
        }

        // Query current azimuth from radar timeline (only show sweep line in real-time mode)
        let azimuth = if state.playback_state.speed == crate::state::PlaybackSpeed::Realtime {
            state
                .playback_state
                .selected_timestamp
                .and_then(|ts| state.radar_timeline.find_scan_at_timestamp(ts))
                .and_then(|scan| {
                    scan.find_sweep_at_timestamp(state.playback_state.selected_timestamp.unwrap())
                })
                .and_then(|(_, sweep)| {
                    sweep.interpolate_azimuth(state.playback_state.selected_timestamp.unwrap())
                })
        } else {
            None
        };

        // Draw the radar sweep visualization
        render_radar_sweep(&painter, &rect, state, azimuth);

        // Draw overlay info in top-left corner
        draw_overlay_info(ui, &rect, state);

        // Handle zoom/pan interactions
        handle_canvas_interaction(&response, &rect, state);
    });
}

/// Render radar data as a cached texture.
///
/// This function:
/// 1. Checks if the cached texture is still valid for the current data
/// 2. If not, renders the radar data to an image using nexrad-render
/// 3. Uploads the image as an egui texture
/// 4. Draws the texture as an image overlay on the map
#[allow(clippy::too_many_arguments)]
fn render_radar_texture(
    ctx: &egui::Context,
    painter: &Painter,
    projection: &MapProjection,
    volume: &Volume,
    cache: &mut RadarTextureCache,
    rect: &Rect,
    radar_lat: f64,
    radar_lon: f64,
) {
    // Build cache key - use a simple identifier based on volume coverage pattern
    let data_id = format!("vcp_{}", volume.coverage_pattern_number());

    // Use a fixed render size for the texture
    let render_size: (usize, usize) = (800, 800);
    let cache_key = RadarCacheKey::new(data_id, 0, render_size);

    // Check if we need to re-render
    if !cache.is_valid(&cache_key) {
        log::info!("Rendering radar texture...");
        match render_sweep_to_image(volume, 0, render_size) {
            Ok(image) => {
                cache.update(ctx, cache_key, image);
                log::info!("Radar texture updated successfully");
            }
            Err(e) => {
                log::error!("Failed to render radar texture: {}", e);
                return;
            }
        }
    }

    // Draw the cached texture
    if let Some(texture) = cache.texture() {
        // Get the radar coverage range
        let range_km = radar_coverage_range_km();

        // Convert geographic bounds to screen coordinates
        // Radar coverage is a circle of `range_km` radius centered on the radar site
        let km_to_deg = 1.0 / 111.0;
        let lat_correction = radar_lat.to_radians().cos();

        // Calculate the bounding box in geographic coordinates
        let lat_range = range_km * km_to_deg;
        let lon_range = range_km * km_to_deg / lat_correction;

        let top_left = projection.geo_to_screen(Coord {
            x: radar_lon - lon_range,
            y: radar_lat + lat_range,
        });
        let bottom_right = projection.geo_to_screen(Coord {
            x: radar_lon + lon_range,
            y: radar_lat - lat_range,
        });

        // Create the screen rect for the texture
        let texture_rect = Rect::from_min_max(top_left, bottom_right);

        // Clip to canvas bounds
        let clipped_rect = texture_rect.intersect(*rect);

        if clipped_rect.width() > 0.0 && clipped_rect.height() > 0.0 {
            // Calculate UV coordinates for the clipped portion
            let full_width = texture_rect.width();
            let full_height = texture_rect.height();

            let uv_min_x = (clipped_rect.min.x - texture_rect.min.x) / full_width;
            let uv_min_y = (clipped_rect.min.y - texture_rect.min.y) / full_height;
            let uv_max_x = (clipped_rect.max.x - texture_rect.min.x) / full_width;
            let uv_max_y = (clipped_rect.max.y - texture_rect.min.y) / full_height;

            let clipped_uv = egui::Rect::from_min_max(
                egui::pos2(uv_min_x, uv_min_y),
                egui::pos2(uv_max_x, uv_max_y),
            );

            painter.image(texture.id(), clipped_rect, clipped_uv, Color32::WHITE);
        }
    }
}

/// Filter geo layers based on visibility settings.
fn filter_geo_layers(
    layers: &GeoLayerSet,
    visibility: &crate::state::GeoLayerVisibility,
) -> GeoLayerSet {
    let mut filtered = layers.clone();

    // Apply visibility settings
    if let Some(ref mut layer) = filtered.states {
        layer.visible = visibility.states;
    }
    if let Some(ref mut layer) = filtered.counties {
        layer.visible = visibility.counties;
    }

    filtered
}

fn draw_overlay_info(ui: &mut egui::Ui, rect: &Rect, state: &AppState) {
    let overlay_pos = rect.left_top() + Vec2::new(10.0, 10.0);

    // Create a small overlay area
    let overlay_rect = Rect::from_min_size(overlay_pos, Vec2::new(150.0, 70.0));

    ui.scope_builder(egui::UiBuilder::new().max_rect(overlay_rect), |ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new(format!("Site: {}", state.viz_state.site_id))
                    .monospace()
                    .size(12.0)
                    .color(Color32::from_rgb(200, 200, 220)),
            );
            ui.label(
                RichText::new(format!("Time: {}", state.viz_state.timestamp))
                    .monospace()
                    .size(12.0)
                    .color(Color32::from_rgb(200, 200, 220)),
            );
            ui.label(
                RichText::new(format!("Elev: {}", state.viz_state.elevation))
                    .monospace()
                    .size(12.0)
                    .color(Color32::from_rgb(200, 200, 220)),
            );
        });
    });
}

fn handle_canvas_interaction(response: &egui::Response, rect: &Rect, state: &mut AppState) {
    // Handle dragging for panning
    if response.dragged() {
        state.viz_state.pan_offset += response.drag_delta();
    }

    // Handle scroll for zooming relative to cursor position
    if response.hovered() {
        let scroll_delta = response.ctx.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = 1.0 + scroll_delta.y * 0.001;
            let old_zoom = state.viz_state.zoom;
            let new_zoom = (old_zoom * zoom_factor).clamp(0.1, 10.0);

            // Adjust pan offset to keep the point under cursor stationary
            if let Some(cursor_pos) = response.hover_pos() {
                let cursor_rel = cursor_pos - rect.center();
                let ratio = new_zoom / old_zoom;
                state.viz_state.pan_offset =
                    cursor_rel * (1.0 - ratio) + state.viz_state.pan_offset * ratio;
            }

            state.viz_state.zoom = new_zoom;
        }
    }

    // Reset view on double-click
    if response.double_clicked() {
        state.viz_state.zoom = 1.0;
        state.viz_state.pan_offset = Vec2::ZERO;
    }
}

/// Render the radar sweep visualization
fn render_radar_sweep(painter: &Painter, rect: &Rect, state: &AppState, azimuth: Option<f32>) {
    let center = rect.center() + state.viz_state.pan_offset;
    let base_radius = rect.width().min(rect.height()) * 0.4;
    let radius = base_radius * state.viz_state.zoom;

    // Draw range rings (every 50km nominal, with major rings at 100km)
    let ring_color = canvas_colors::ring();
    let ring_major_color = canvas_colors::ring_major();
    let num_rings = 6;
    for i in 1..=num_rings {
        let ring_radius = radius * (i as f32 / num_rings as f32);
        let is_major = i % 2 == 0;
        let color = if is_major {
            ring_major_color
        } else {
            ring_color
        };
        let width = if is_major { 1.5 } else { 1.0 };
        painter.circle_stroke(center, ring_radius, Stroke::new(width, color));
    }

    // Draw radial lines (every 30 degrees)
    let radial_color = canvas_colors::radial();
    for i in 0..12 {
        let angle = (i as f32) * 30.0 * PI / 180.0 - PI / 2.0; // Start from North
        let end_x = center.x + radius * angle.cos();
        let end_y = center.y + radius * angle.sin();
        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(0.5, radial_color),
        );
    }

    // Draw cardinal direction labels
    let label_offset = radius + 15.0;
    let font_id = egui::FontId::proportional(12.0);
    let cardinal_color = canvas_colors::cardinal_label();

    painter.text(
        center + Vec2::new(0.0, -label_offset),
        egui::Align2::CENTER_BOTTOM,
        "N",
        font_id.clone(),
        cardinal_color,
    );
    painter.text(
        center + Vec2::new(label_offset, 0.0),
        egui::Align2::LEFT_CENTER,
        "E",
        font_id.clone(),
        cardinal_color,
    );
    painter.text(
        center + Vec2::new(0.0, label_offset),
        egui::Align2::CENTER_TOP,
        "S",
        font_id.clone(),
        cardinal_color,
    );
    painter.text(
        center + Vec2::new(-label_offset, 0.0),
        egui::Align2::RIGHT_CENTER,
        "W",
        font_id,
        cardinal_color,
    );

    // Draw center marker (radar site)
    painter.circle_filled(center, 4.0, canvas_colors::CENTER_MARKER);
    painter.circle_stroke(
        center,
        4.0,
        Stroke::new(1.0, canvas_colors::CENTER_MARKER_STROKE),
    );

    // Draw the sweep line if we have azimuth data
    if let Some(az) = azimuth {
        // Draw a fading "trail" behind the sweep line to show recent coverage
        let trail_length = 30.0; // degrees of trail
        let num_trail_segments = 15;
        for i in 0..num_trail_segments {
            let trail_az = az - (i as f32) * (trail_length / num_trail_segments as f32);
            let alpha = ((num_trail_segments - i) as f32 / num_trail_segments as f32 * 60.0) as u8;
            let trail_color = Color32::from_rgba_unmultiplied(
                radar::SWEEP_LINE.r(),
                radar::SWEEP_LINE.g(),
                radar::SWEEP_LINE.b(),
                alpha,
            );

            let angle_rad = (trail_az - 90.0) * PI / 180.0;
            let end_x = center.x + radius * angle_rad.cos();
            let end_y = center.y + radius * angle_rad.sin();

            painter.line_segment(
                [center, Pos2::new(end_x, end_y)],
                Stroke::new(2.0, trail_color),
            );
        }

        // Draw the main sweep line
        let angle_rad = (az - 90.0) * PI / 180.0;
        let end_x = center.x + radius * angle_rad.cos();
        let end_y = center.y + radius * angle_rad.sin();

        // Bright sweep line
        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(3.0, radar::SWEEP_LINE),
        );

        // Glow effect
        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(
                6.0,
                Color32::from_rgba_unmultiplied(
                    radar::SWEEP_LINE.r(),
                    radar::SWEEP_LINE.g(),
                    radar::SWEEP_LINE.b(),
                    40,
                ),
            ),
        );
    }
}

/// Render NWS alert polygons on the canvas.
fn render_nws_alerts(
    painter: &Painter,
    projection: &MapProjection,
    alerts_state: &AlertsState,
    current_time: f64,
) {
    // Get alerts active at the current time, sorted by severity (lowest first so highest draws on top)
    let mut active_alerts: Vec<&NwsAlert> = alerts_state.active_alerts(current_time);
    active_alerts.sort_by_key(|a| a.severity());

    for alert in active_alerts {
        render_alert_polygon(painter, projection, alert);
    }
}

/// Render a single alert polygon.
fn render_alert_polygon(painter: &Painter, projection: &MapProjection, alert: &NwsAlert) {
    if alert.polygon.is_empty() {
        return;
    }

    // Convert lat/lon vertices to screen coordinates
    let screen_points: Vec<Pos2> = alert
        .polygon
        .iter()
        .map(|&(lat, lon)| projection.geo_to_screen(Coord { x: lon, y: lat }))
        .collect();

    if screen_points.len() < 3 {
        return;
    }

    let severity = alert.severity();
    let fill_color = severity.fill_color();
    let stroke_color = severity.stroke_color();

    // Draw filled polygon
    painter.add(egui::Shape::convex_polygon(
        screen_points.clone(),
        fill_color,
        Stroke::NONE,
    ));

    // Draw polygon outline
    let mut stroke_points = screen_points.clone();
    stroke_points.push(screen_points[0]); // Close the polygon

    for i in 0..stroke_points.len() - 1 {
        painter.line_segment(
            [stroke_points[i], stroke_points[i + 1]],
            Stroke::new(2.0, stroke_color),
        );
    }

    // Draw alert type label at centroid
    if let Some(centroid) = polygon_centroid(&screen_points) {
        painter.text(
            centroid,
            egui::Align2::CENTER_CENTER,
            alert.alert_type.short_label(),
            egui::FontId::proportional(11.0),
            stroke_color,
        );
    }
}

/// Calculate the centroid of a polygon.
fn polygon_centroid(points: &[Pos2]) -> Option<Pos2> {
    if points.is_empty() {
        return None;
    }

    let sum = points.iter().fold(Vec2::ZERO, |acc, p| acc + p.to_vec2());
    Some(Pos2::new(
        sum.x / points.len() as f32,
        sum.y / points.len() as f32,
    ))
}

/// Render NEXRAD radar site markers on the map.
/// Always shows the current site; optionally shows all other sites.
fn render_nexrad_sites(
    painter: &Painter,
    projection: &MapProjection,
    current_site_id: &str,
    visibility: &GeoLayerVisibility,
) {
    let current_site_id_upper = current_site_id.to_uppercase();

    // Get visible bounds to cull off-screen sites
    let (min_lon, min_lat, max_lon, max_lat) = projection.visible_bounds();

    // Render other sites if the layer is enabled
    if visibility.nexrad_sites {
        for site in NEXRAD_SITES.iter() {
            // Skip current site (we'll draw it on top)
            if site.id == current_site_id_upper {
                continue;
            }

            // Cull sites outside visible bounds (with some padding)
            let padding = 2.0;
            if site.lat < min_lat - padding
                || site.lat > max_lat + padding
                || site.lon < min_lon - padding
                || site.lon > max_lon + padding
            {
                continue;
            }

            let screen_pos = projection.geo_to_screen(Coord {
                x: site.lon,
                y: site.lat,
            });

            painter.circle_filled(screen_pos, 4.0, site_colors::OTHER);
            painter.circle_stroke(screen_pos, 4.0, Stroke::new(1.0, site_colors::OTHER_STROKE));

            if visibility.labels {
                painter.text(
                    screen_pos + Vec2::new(6.0, -2.0),
                    egui::Align2::LEFT_CENTER,
                    site.id,
                    egui::FontId::proportional(10.0),
                    site_colors::LABEL,
                );
            }
        }
    }

    // Always render the current site (on top of others)
    if let Some(site) = get_site(&current_site_id_upper) {
        let screen_pos = projection.geo_to_screen(Coord {
            x: site.lon,
            y: site.lat,
        });

        painter.circle_filled(screen_pos, 6.0, site_colors::CURRENT);
        painter.circle_stroke(
            screen_pos,
            6.0,
            Stroke::new(1.5, site_colors::CURRENT_STROKE),
        );

        painter.text(
            screen_pos + Vec2::new(8.0, -2.0),
            egui::Align2::LEFT_CENTER,
            site.id,
            egui::FontId::proportional(11.0),
            site_colors::CURRENT_LABEL,
        );
    }
}
