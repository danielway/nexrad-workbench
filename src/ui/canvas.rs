//! Central canvas UI: radar visualization area.

use super::colors::{canvas as canvas_colors, radar, sites as site_colors};
use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::{GeoLayerSet, MapProjection};
use crate::nexrad::{RadarGpuRenderer, RADAR_COVERAGE_RANGE_KM};
use crate::state::{AppState, GeoLayerVisibility, RenderProcessing};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, Vec2};
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

        // Draw the radar sweep visualization (range rings, radials, labels)
        render_radar_sweep(&painter, &projection, state, None);

        // Draw overlay info in top-left corner
        draw_overlay_info(ui, &rect, state);

        // Handle zoom/pan interactions
        handle_canvas_interaction(&response, &rect, state);
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

fn handle_canvas_interaction(response: &egui::Response, rect: &Rect, state: &mut AppState) {
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
