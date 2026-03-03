//! Central canvas UI: radar visualization area.

use super::colors::{canvas as canvas_colors, radar, sites as site_colors};
use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::{GeoLayerSet, MapProjection};
use crate::nexrad::{RadarGpuRenderer, RADAR_COVERAGE_RANGE_KM};
use crate::state::{AppState, GeoLayerVisibility, RenderProcessing, StormCellInfo};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use geo_types::Coord;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

/// Render canvas with optional geographic layers and NEXRAD data.
pub fn render_canvas_with_geo(
    ctx: &egui::Context,
    state: &mut AppState,
    geo_layers: Option<&GeoLayerSet>,
    gpu_renderer: Option<&Arc<Mutex<RadarGpuRenderer>>>,
) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let available_size = ui.available_size();

        let (response, painter) = ui.allocate_painter(available_size, Sense::click_and_drag());

        let rect = response.rect;

        let dark = state.is_dark;

        // Draw background
        painter.rect_filled(rect, 0.0, canvas_colors::background(dark));

        // Create projection for geo layers
        let mut projection =
            MapProjection::new(state.viz_state.center_lat, state.viz_state.center_lon);
        projection.update(state.viz_state.zoom, state.viz_state.pan_offset, rect);

        // Draw geographic layers BEFORE radar (so radar appears on top)
        if let Some(layers) = geo_layers {
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

        // Render radar data via GPU shader
        if let Some(renderer) = gpu_renderer {
            draw_radar_gpu(
                ui,
                &projection,
                renderer,
                &rect,
                state.viz_state.center_lat,
                state.viz_state.center_lon,
                &state.render_processing,
            );
        }

        // Draw storm cell overlays (before sweep lines, after radar data)
        if state.storm_cells_visible && !state.detected_storm_cells.is_empty() {
            render_storm_cells(&painter, &projection, &state.detected_storm_cells, dark);
        }

        // Draw the radar sweep visualization (range rings, radials, labels, sweep line)
        let sweep_azimuth = compute_sweep_line_azimuth(state);
        render_radar_sweep(&painter, &projection, state, sweep_azimuth);

        // Draw distance measurement line
        if state.distance_tool_active || state.distance_start.is_some() {
            render_distance_measurement(
                &painter,
                &projection,
                state.distance_start,
                state.distance_end,
            );
        }

        // Draw inspector tooltip
        if state.inspector_enabled {
            if let Some(hover_pos) = response.hover_pos() {
                render_inspector(
                    ui,
                    &painter,
                    &projection,
                    hover_pos,
                    state.viz_state.center_lat,
                    state.viz_state.center_lon,
                    gpu_renderer,
                );
            }
        }

        // Draw overlay info in top-left corner
        draw_overlay_info(ui, &rect, state);

        // Handle zoom/pan interactions
        handle_canvas_interaction(&response, &rect, state, &projection);
    });
}

/// Draw radar data using a GPU shader via egui PaintCallback.
fn draw_radar_gpu(
    ui: &mut egui::Ui,
    projection: &MapProjection,
    renderer: &Arc<Mutex<RadarGpuRenderer>>,
    rect: &Rect,
    radar_lat: f64,
    radar_lon: f64,
    processing: &RenderProcessing,
) {
    // Check if renderer has data and get the actual data range
    let max_range_km = {
        let r = renderer.lock().unwrap();
        if !r.has_data() {
            return;
        }
        r.max_range_km()
    };

    // Use the actual data max range (not a fixed constant) so the shader's
    // pixel-to-km mapping matches the geographic projection exactly.
    let range_km = max_range_km;

    let km_to_deg = 1.0 / 111.0;
    let lat_correction = radar_lat.to_radians().cos();
    let lon_range = range_km * km_to_deg / lat_correction;

    // Compute radar center in screen coordinates
    let center_screen = projection.geo_to_screen(Coord {
        x: radar_lon,
        y: radar_lat,
    });

    // Compute radius: distance from center to edge of coverage in screen pixels
    let edge_screen = projection.geo_to_screen(Coord {
        x: radar_lon + lon_range,
        y: radar_lat,
    });
    let radius_px = (edge_screen.x - center_screen.x).abs();

    let renderer = renderer.clone();
    let center = [center_screen.x, center_screen.y];
    let canvas_min = rect.min;
    let processing = processing.clone();

    let callback = egui::PaintCallback {
        rect: *rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
            let gl = painter.gl();
            let r = renderer.lock().unwrap();
            if r.has_data() {
                let px_per_point = info.pixels_per_point;
                let viewport = info.viewport_in_pixels();
                // Convert from screen points to physical pixels relative to viewport
                let adjusted_center = [
                    (center[0] - canvas_min.x) * px_per_point,
                    (center[1] - canvas_min.y) * px_per_point,
                ];
                r.paint(
                    gl,
                    adjusted_center,
                    radius_px * px_per_point,
                    [viewport.width_px as f32, viewport.height_px as f32],
                    &processing,
                );
            }
        })),
    };

    ui.painter().add(callback);
}

/// Format an age in seconds as a human-readable string.
fn format_age(secs: f64) -> String {
    if secs < 60.0 {
        format!("{}s", secs as u32)
    } else if secs < 3600.0 {
        let m = (secs / 60.0) as u32;
        let s = (secs % 60.0) as u32;
        format!("{}m{}s", m, s)
    } else {
        let h = (secs / 3600.0) as u32;
        let m = ((secs % 3600.0) / 60.0) as u32;
        format!("{}h{}m", h, m)
    }
}

/// Color for age label based on data age.
fn age_color(secs: f64) -> Color32 {
    if secs > 300.0 {
        Color32::from_rgb(255, 80, 80)
    } else if secs > 60.0 {
        Color32::from_rgb(255, 200, 60)
    } else {
        Color32::from_rgb(80, 220, 100)
    }
}

/// Filter geo layers based on visibility settings.
fn filter_geo_layers(layers: &GeoLayerSet, visibility: &GeoLayerVisibility) -> GeoLayerSet {
    let mut filtered = layers.clone();

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
            if state.viz_state.render_mode == crate::state::RenderMode::FixedTilt {
                if let Some(secs) = state.viz_state.data_staleness_secs {
                    let color = age_color(secs);
                    ui.label(
                        RichText::new(format!("Age: {}", format_age(secs)))
                            .monospace()
                            .size(12.0)
                            .color(color),
                    );
                }
            }
        });
    });
}

fn handle_canvas_interaction(
    response: &egui::Response,
    rect: &Rect,
    state: &mut AppState,
    projection: &MapProjection,
) {
    // Distance tool: click to place points
    if state.distance_tool_active && response.clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            let geo = projection.screen_to_geo(click_pos);
            if state.distance_start.is_none() || state.distance_end.is_some() {
                // First click or restart: set start, clear end
                state.distance_start = Some((geo.y, geo.x));
                state.distance_end = None;
            } else {
                // Second click: set end
                state.distance_end = Some((geo.y, geo.x));
            }
        }
    }

    if response.dragged() {
        state.viz_state.pan_offset += response.drag_delta();
    }

    if response.hovered() {
        let scroll_delta = response.ctx.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = 1.0 + scroll_delta.y * 0.001;
            let old_zoom = state.viz_state.zoom;
            let new_zoom = (old_zoom * zoom_factor).clamp(0.1, 25.0);

            if let Some(cursor_pos) = response.hover_pos() {
                let cursor_rel = cursor_pos - rect.center();
                let ratio = new_zoom / old_zoom;
                state.viz_state.pan_offset =
                    cursor_rel * (1.0 - ratio) + state.viz_state.pan_offset * ratio;
            }

            state.viz_state.zoom = new_zoom;
        }
    }

    if response.double_clicked() {
        state.viz_state.zoom = 1.0;
        state.viz_state.pan_offset = Vec2::ZERO;
    }
}

/// Render the radar sweep visualization (range rings, radial lines, cardinal labels).
///
/// Uses the same MapProjection as the GPU radar and geo layers so everything
/// pans and zooms together.
/// Compute the sweep line azimuth for the current playback position.
///
/// Returns `Some(azimuth_degrees)` when playing at slow speeds (< 1 min/s)
/// and the playback position falls within a sweep.
fn compute_sweep_line_azimuth(state: &AppState) -> Option<f32> {
    if !state.playback_state.playing {
        return None;
    }
    // Only show sweep line at slow playback speeds (< 1 min/s)
    if state.playback_state.speed.timeline_seconds_per_real_second() > 60.0 {
        return None;
    }

    let ts = state.playback_state.playback_position();
    let scan = state.radar_timeline.find_scan_at_timestamp(ts)?;
    let (_, sweep) = scan.find_sweep_at_timestamp(ts)?;

    let duration = sweep.end_time - sweep.start_time;
    if duration <= 0.0 {
        return None;
    }

    let progress = (ts - sweep.start_time) / duration;
    Some(((progress * 360.0) as f32) % 360.0)
}

fn render_radar_sweep(
    painter: &Painter,
    projection: &MapProjection,
    state: &AppState,
    azimuth: Option<f32>,
) {
    let radar_lat = state.viz_state.center_lat;
    let radar_lon = state.viz_state.center_lon;
    let dark = state.is_dark;

    // Compute center from geographic projection (same as GPU renderer)
    let center = projection.geo_to_screen(Coord {
        x: radar_lon,
        y: radar_lat,
    });

    // Compute radius in screen pixels for the coverage range
    let range_km = RADAR_COVERAGE_RANGE_KM;
    let km_to_deg = 1.0 / 111.0;
    let lat_correction = radar_lat.to_radians().cos();
    let lon_range = range_km * km_to_deg / lat_correction;

    let edge = projection.geo_to_screen(Coord {
        x: radar_lon + lon_range,
        y: radar_lat,
    });
    let radius = (edge.x - center.x).abs();

    // Draw range rings
    let ring_color = canvas_colors::ring(dark);
    let ring_major_color = canvas_colors::ring_major(dark);
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
    let radial_color = canvas_colors::radial(dark);
    for i in 0..12 {
        let angle = (i as f32) * 30.0 * PI / 180.0 - PI / 2.0;
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
    let cardinal_color = canvas_colors::cardinal_label(dark);

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
    painter.circle_filled(center, 4.0, canvas_colors::center_marker(dark));
    painter.circle_stroke(
        center,
        4.0,
        Stroke::new(1.0, canvas_colors::center_marker_stroke(dark)),
    );

    // Draw the sweep line if we have azimuth data
    if let Some(az) = azimuth {
        let angle_rad = (az - 90.0) * PI / 180.0;
        let end_x = center.x + radius * angle_rad.cos();
        let end_y = center.y + radius * angle_rad.sin();

        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(3.0, radar::SWEEP_LINE),
        );
    }
}

/// Render NEXRAD radar site markers on the map.
fn render_nexrad_sites(
    painter: &Painter,
    projection: &MapProjection,
    current_site_id: &str,
    visibility: &GeoLayerVisibility,
) {
    let current_site_id_upper = current_site_id.to_uppercase();
    let (min_lon, min_lat, max_lon, max_lat) = projection.visible_bounds();

    if visibility.nexrad_sites {
        for site in NEXRAD_SITES.iter() {
            if site.id == current_site_id_upper {
                continue;
            }

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

/// Render inspector tooltip showing lat/lon and data value at hover position.
fn render_inspector(
    ui: &mut egui::Ui,
    painter: &Painter,
    projection: &MapProjection,
    hover_pos: Pos2,
    radar_lat: f64,
    radar_lon: f64,
    gpu_renderer: Option<&Arc<Mutex<RadarGpuRenderer>>>,
) {
    let geo = projection.screen_to_geo(hover_pos);
    let lat = geo.y;
    let lon = geo.x;

    // Compute polar coordinates relative to radar site
    let dlat = lat - radar_lat;
    let dlon = (lon - radar_lon) * radar_lat.to_radians().cos();
    let range_km = (dlat * dlat + dlon * dlon).sqrt() * 111.0;
    let azimuth_deg = (dlon.atan2(dlat).to_degrees() + 360.0) % 360.0;

    // Look up data value
    let value = gpu_renderer.and_then(|r| {
        let renderer = r.lock().unwrap();
        renderer.value_at_polar(azimuth_deg as f32, range_km)
    });

    // Build tooltip text
    let mut lines = vec![
        format!("{:.4}\u{00B0}N {:.4}\u{00B0}W", lat, -lon),
        format!("Az: {:.1}\u{00B0}  Rng: {:.1} km", azimuth_deg, range_km),
    ];
    if let Some(v) = value {
        lines.push(format!("Value: {:.1}", v));
    }
    let text = lines.join("\n");

    // Draw tooltip background
    let font_id = egui::FontId::monospace(11.0);
    let galley = painter.layout_no_wrap(text.clone(), font_id.clone(), Color32::WHITE);
    let tooltip_size = galley.size();
    let padding = Vec2::new(6.0, 4.0);
    let tooltip_pos = hover_pos + Vec2::new(16.0, -tooltip_size.y - 8.0);
    let bg_rect = Rect::from_min_size(
        tooltip_pos - padding,
        tooltip_size + padding * 2.0,
    );

    painter.rect_filled(bg_rect, 4.0, Color32::from_rgba_unmultiplied(20, 20, 30, 220));
    painter.rect_stroke(bg_rect, 4.0, Stroke::new(1.0, Color32::from_rgb(80, 80, 100)), StrokeKind::Outside);
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

/// Render distance measurement line between two points.
fn render_distance_measurement(
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
    painter.circle_stroke(
        start_screen,
        5.0,
        Stroke::new(1.5, Color32::WHITE),
    );

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
        painter.circle_stroke(
            end_screen,
            5.0,
            Stroke::new(1.5, Color32::WHITE),
        );

        // Compute great-circle distance using Haversine formula
        let distance_km = haversine_km(start_lat, start_lon, end_lat, end_lon);
        let distance_nm = distance_km * 0.539957; // nautical miles
        let distance_mi = distance_km * 0.621371; // statute miles

        // Draw label at midpoint
        let mid = Pos2::new(
            (start_screen.x + end_screen.x) / 2.0,
            (start_screen.y + end_screen.y) / 2.0,
        );
        let label = format!("{:.1} km / {:.1} nm / {:.1} mi", distance_km, distance_nm, distance_mi);

        let font_id = egui::FontId::monospace(11.0);
        let galley = painter.layout_no_wrap(label, font_id, Color32::WHITE);
        let label_size = galley.size();
        let padding = Vec2::new(5.0, 3.0);
        let label_pos = mid - Vec2::new(label_size.x / 2.0, label_size.y + 8.0);
        let bg_rect = Rect::from_min_size(
            label_pos - padding,
            label_size + padding * 2.0,
        );

        painter.rect_filled(bg_rect, 3.0, Color32::from_rgba_unmultiplied(30, 20, 20, 220));
        painter.rect_stroke(bg_rect, 3.0, Stroke::new(1.0, Color32::from_rgb(255, 100, 100)), StrokeKind::Outside);
        painter.galley(label_pos, galley, Color32::WHITE);
    }
}

/// Haversine distance between two lat/lon points in km.
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0; // Earth radius in km
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    r * c
}

/// Render storm cell markers and bounding boxes on the canvas.
fn render_storm_cells(
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
        painter.rect_stroke(bounds_rect, 2.0, Stroke::new(1.5, color), StrokeKind::Outside);

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
