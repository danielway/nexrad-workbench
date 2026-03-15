//! Central canvas UI: radar visualization area.

use super::colors::{canvas as canvas_colors, radar, sites as site_colors};
use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::{GeoLayerSet, GeoLineRenderer, GlobeCamera, GlobeRenderer, MapProjection};
use crate::nexrad::{RadarGpuRenderer, RADAR_COVERAGE_RANGE_KM};
use crate::state::{AppState, GeoLayerVisibility, RenderProcessing, StormCellInfo, ViewMode};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use geo_types::Coord;
use glow::HasContext;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

/// Render canvas with optional geographic layers and NEXRAD data.
#[allow(clippy::too_many_arguments)]
pub fn render_canvas_with_geo(
    ctx: &egui::Context,
    state: &mut AppState,
    geo_layers: Option<&GeoLayerSet>,
    gpu_renderer: Option<&Arc<Mutex<RadarGpuRenderer>>>,
    globe_renderer: Option<&Arc<Mutex<GlobeRenderer>>>,
    geo_line_renderer: Option<&Arc<Mutex<GeoLineRenderer>>>,
    globe_radar_renderer: Option<&Arc<Mutex<crate::nexrad::GlobeRadarRenderer>>>,
    volume_ray_renderer: Option<&Arc<Mutex<crate::nexrad::VolumeRayRenderer>>>,
) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let available_size = ui.available_size();

        let (response, painter) = ui.allocate_painter(available_size, Sense::click_and_drag());

        let rect = response.rect;

        let dark = state.is_dark;

        // Draw background
        painter.rect_filled(rect, 0.0, canvas_colors::background(dark));

        match state.viz_state.view_mode {
            ViewMode::Globe3D => {
                // Update camera aspect ratio
                state.viz_state.camera.set_aspect(rect);

                // Draw the 3D globe via PaintCallback
                draw_globe(
                    ui,
                    &rect,
                    state,
                    globe_renderer,
                    geo_line_renderer,
                    gpu_renderer,
                    globe_radar_renderer,
                    volume_ray_renderer,
                );

                // 2D overlays drawn on top after the GL callback
                draw_color_scale(ui, &rect, &state.viz_state.product);
                draw_overlay_info(ui, &rect, state);
                draw_compass(ui, &rect, &state.viz_state.camera);

                // Handle orbit/zoom interactions
                handle_globe_interaction(&response, &rect, state);
            }
            ViewMode::Flat2D => {
                // --- Existing flat 2D path ---
                let mut projection =
                    MapProjection::new(state.viz_state.center_lat, state.viz_state.center_lon);
                projection.update(state.viz_state.zoom, state.viz_state.pan_offset, rect);

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

                render_nexrad_sites(
                    &painter,
                    &projection,
                    &state.viz_state.site_id,
                    &state.layer_state.geo,
                );

                let sweep_info = compute_sweep_line_azimuth(state);

                // Compute sweep animation compositing state (used for GPU rendering
                // and sweep-aware inspector lookups).
                let gpu_sweep = if state.render_processing.sweep_animation {
                    let playback_ts = state.playback_state.playback_position();
                    let sweep_bounds = state
                        .radar_timeline
                        .find_recent_scan(playback_ts, 15.0 * 60.0)
                        .and_then(|scan| {
                            let displayed_elev = state.displayed_sweep_elevation_number;
                            scan.sweeps
                                .iter()
                                .filter(|s| Some(s.elevation_number) == displayed_elev)
                                .rfind(|s| s.start_time <= playback_ts)
                                .or_else(|| {
                                    scan.sweeps
                                        .iter()
                                        .find(|s| Some(s.elevation_number) == displayed_elev)
                                })
                                .map(|s| (s.start_time, s.end_time))
                        });
                    match sweep_bounds {
                        Some((s, _)) if playback_ts < s => Some((0.0, 0.0)),
                        Some((_, e)) if playback_ts <= e => sweep_info,
                        _ => None,
                    }
                } else {
                    None
                };

                if let Some(renderer) = gpu_renderer {
                    draw_radar_gpu(
                        ui,
                        &projection,
                        renderer,
                        &rect,
                        state.viz_state.center_lat,
                        state.viz_state.center_lon,
                        &state.render_processing,
                        gpu_sweep,
                    );
                    // Continuous repaint while sweep animation is compositing
                    if gpu_sweep.is_some() {
                        ui.ctx().request_repaint();
                    }
                }

                if state.storm_cells_visible && !state.detected_storm_cells.is_empty() {
                    render_storm_cells(&painter, &projection, &state.detected_storm_cells, dark);
                }

                // Only show sweep line when actively revealing (not before sweep starts or after it ends)
                let sweep_line_info = match gpu_sweep {
                    Some((az, start)) if az != 0.0 || start != 0.0 => Some((az, start)),
                    _ => None,
                };
                render_radar_sweep(&painter, &projection, state, sweep_line_info);

                if state.distance_tool_active || state.distance_start.is_some() {
                    render_distance_measurement(
                        &painter,
                        &projection,
                        state.distance_start,
                        state.distance_end,
                    );
                }

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
                            &state.viz_state.product,
                            state.use_local_time,
                            gpu_sweep,
                        );
                    }
                }

                draw_color_scale(ui, &rect, &state.viz_state.product);
                draw_overlay_info(ui, &rect, state);

                handle_canvas_interaction(&response, &rect, state, &projection);
            }
        }
    });
}

/// Draw the 3D globe with geo overlays via egui PaintCallback.
#[allow(clippy::too_many_arguments)]
fn draw_globe(
    ui: &mut egui::Ui,
    rect: &Rect,
    state: &AppState,
    globe_renderer: Option<&Arc<Mutex<GlobeRenderer>>>,
    geo_line_renderer: Option<&Arc<Mutex<GeoLineRenderer>>>,
    gpu_renderer: Option<&Arc<Mutex<RadarGpuRenderer>>>,
    globe_radar_renderer: Option<&Arc<Mutex<crate::nexrad::GlobeRadarRenderer>>>,
    volume_ray_renderer: Option<&Arc<Mutex<crate::nexrad::VolumeRayRenderer>>>,
) {
    let Some(globe_r) = globe_renderer.cloned() else {
        return;
    };
    let geo_r = geo_line_renderer.cloned();
    let radar_r = gpu_renderer.cloned();
    let globe_rr = globe_radar_renderer.cloned();
    let vol_r = volume_ray_renderer.cloned();

    let camera = state.viz_state.camera.clone();
    let processing = state.render_processing.clone();
    let radar_lat = state.viz_state.center_lat;
    let radar_lon = state.viz_state.center_lon;
    let volume_enabled = state.viz_state.volume_3d_enabled;
    let volume_density_cutoff = state.viz_state.volume_density_cutoff;
    let geo_vis = crate::geo::geo_line_renderer::VisibleLayers {
        states: state.layer_state.geo.states,
        counties: state.layer_state.geo.counties,
        highways: state.layer_state.geo.highways,
        lakes: state.layer_state.geo.lakes,
    };

    let callback = egui::PaintCallback {
        rect: *rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |_info, painter| {
            let gl = painter.gl();

            // 1. Draw globe sphere (sets up depth buffer)
            if let Ok(gr) = globe_r.lock() {
                gr.paint(gl, &camera);
            }

            // 2. Draw geo lines on sphere surface
            if let Some(ref glr) = geo_r {
                if let Ok(lr) = glr.lock() {
                    lr.paint(gl, &camera, &geo_vis);
                }
            }

            // 3. Draw radar data on sphere
            let mut drew_volume = false;
            if volume_enabled {
                if let (Some(ref vr), Some(ref rr)) = (&vol_r, &radar_r) {
                    if let (Ok(mut v), Ok(flat_r)) = (vr.lock(), rr.lock()) {
                        if v.has_data() {
                            // Get current viewport dimensions for FBO sizing
                            let mut vp = [0i32; 4];
                            unsafe { gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp) };
                            v.paint(
                                gl,
                                &camera,
                                flat_r.lut_texture(),
                                &processing,
                                flat_r.value_min(),
                                flat_r.value_range(),
                                volume_density_cutoff,
                                vp[2], // viewport width
                                vp[3], // viewport height
                            );
                            drew_volume = true;
                        }
                    }
                }
            }
            if !drew_volume {
                if let (Some(ref rr), Some(ref grr)) = (&radar_r, &globe_rr) {
                    if let (Ok(flat_r), Ok(mut globe_r)) = (rr.lock(), grr.lock()) {
                        if flat_r.has_data() {
                            // Rebuild mesh if site changed
                            globe_r.update_site(gl, radar_lat, radar_lon, flat_r.max_range_km());
                            // Paint radar on sphere
                            globe_r.paint(gl, &camera, &flat_r, &processing);
                        }
                    }
                }
            }

            // Restore GL state for egui
            unsafe {
                gl.disable(glow::DEPTH_TEST);
                gl.disable(glow::CULL_FACE);
                gl.depth_mask(false);
            }
        })),
    };

    ui.painter().add(callback);
}

/// Handle mouse interactions for globe mode.
///
/// Controls paradigm:
/// - Left mouse: primary navigation (orbit in orbit modes, look in free look)
/// - Right mouse: orientation adjustment (tilt/rotate in orbit, look in free look)
/// - Middle mouse / Shift+left: pan pivot
/// - Scroll: zoom (orbit) or movement speed (free look)
/// - Double-click: move pivot to clicked surface point
fn handle_globe_interaction(response: &egui::Response, rect: &Rect, state: &mut AppState) {
    use crate::geo::camera::CameraMode;

    if response.dragged() {
        let delta = response.drag_delta();
        let viewport_h = response.rect.height();
        let shift_held = response.ctx.input(|i| i.modifiers.shift);
        let right_button = response.dragged_by(egui::PointerButton::Secondary);
        let middle_button = response.dragged_by(egui::PointerButton::Middle);

        match state.viz_state.camera.mode {
            CameraMode::FreeLook => {
                if middle_button || (shift_held && !right_button) {
                    // Middle-drag or Shift+left: translate camera sideways
                    state
                        .viz_state
                        .camera
                        .free_translate(delta.x, delta.y, viewport_h);
                } else if right_button {
                    // Right-drag: look around without moving
                    state
                        .viz_state
                        .camera
                        .free_look(delta.x, delta.y, viewport_h);
                } else {
                    // Left-drag: look around (primary control in free look)
                    state
                        .viz_state
                        .camera
                        .free_look(delta.x, delta.y, viewport_h);
                }
            }
            CameraMode::PlanetOrbit | CameraMode::SiteOrbit => {
                if middle_button || (shift_held && !right_button) {
                    state
                        .viz_state
                        .camera
                        .pan_pivot(delta.x, delta.y, viewport_h);
                } else if right_button {
                    // Right-drag: horizontal rotates (heading), vertical pitches
                    state.viz_state.camera.orbit(delta.x, delta.y, viewport_h);
                } else {
                    // Left-drag: orbit
                    state.viz_state.camera.orbit(delta.x, delta.y, viewport_h);
                }
            }
        }
    }

    if response.hovered() {
        let scroll_delta = response.ctx.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            state.viz_state.camera.zoom(scroll_delta.y);
        }
    }

    // Double-click: move pivot to clicked surface point
    if response.double_clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            if let Some((lat, lon)) = state.viz_state.camera.screen_to_geo(click_pos, *rect) {
                state.viz_state.camera.move_pivot_to(lat, lon);
            } else {
                // Clicked off-globe: recenter on site
                state.viz_state.camera.recenter();
            }
        }
    }
}

/// Draw radar data using a GPU shader via egui PaintCallback.
#[allow(clippy::too_many_arguments)]
fn draw_radar_gpu(
    ui: &mut egui::Ui,
    projection: &MapProjection,
    renderer: &Arc<Mutex<RadarGpuRenderer>>,
    rect: &Rect,
    radar_lat: f64,
    radar_lon: f64,
    processing: &RenderProcessing,
    sweep_info: Option<(f32, f32)>,
) {
    // Check if renderer has data and get the actual data range
    let max_range_km = {
        let r = renderer.lock().expect("renderer mutex poisoned");
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
            let r = renderer.lock().expect("renderer mutex poisoned");
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
                    sweep_info,
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
    } else if secs < 86400.0 {
        let h = (secs / 3600.0) as u32;
        let m = ((secs % 3600.0) / 60.0) as u32;
        format!("{}h{}m", h, m)
    } else if secs < 86400.0 * 365.0 {
        let d = (secs / 86400.0) as u32;
        let h = ((secs % 86400.0) / 3600.0) as u32;
        if d == 1 {
            format!("1 day {}h", h)
        } else {
            format!("{} days", d)
        }
    } else {
        let y = (secs / (86400.0 * 365.25)) as u32;
        let remaining_days = ((secs % (86400.0 * 365.25)) / 86400.0) as u32;
        if y == 1 {
            format!("1 year {} days", remaining_days)
        } else {
            format!("{} years", y)
        }
    }
}

/// Color for age label based on data age.
fn age_color(secs: f64) -> Color32 {
    if secs > ARCHIVE_AGE_THRESHOLD_SECS {
        Color32::from_rgb(255, 160, 40) // orange for archive data
    } else if secs > 300.0 {
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
    if let Some(ref mut layer) = filtered.cities {
        layer.visible = visibility.cities;
    }
    if let Some(ref mut layer) = filtered.highways {
        layer.visible = visibility.highways;
    }
    if let Some(ref mut layer) = filtered.lakes {
        layer.visible = visibility.lakes;
    }

    filtered
}

/// Threshold in seconds above which data is considered "archive" (1 hour).
const ARCHIVE_AGE_THRESHOLD_SECS: f64 = 3600.0;

/// Threshold in seconds above which the age range collapses to a single value
/// because the sweep duration (~20-30s) is negligible compared to total age.
const AGE_RANGE_COLLAPSE_SECS: f64 = 300.0;

fn draw_overlay_info(ui: &mut egui::Ui, rect: &Rect, state: &AppState) {
    let has_prev = state.viz_state.prev_sweep_overlay.is_some();
    let overlay_pos = rect.left_top() + Vec2::new(10.0, 10.0);
    let overlay_height = if has_prev { 130.0 } else { 90.0 };
    let overlay_rect = Rect::from_min_size(overlay_pos, Vec2::new(260.0, overlay_height));

    ui.scope_builder(egui::UiBuilder::new().max_rect(overlay_rect), |ui| {
        ui.vertical(|ui| {
            // Show loud "ARCHIVE DATA" banner when data is old enough to be confusable
            let is_archive = state
                .viz_state
                .data_staleness_secs
                .is_some_and(|s| s > ARCHIVE_AGE_THRESHOLD_SECS);
            if is_archive {
                ui.label(
                    RichText::new("ARCHIVE DATA")
                        .monospace()
                        .size(14.0)
                        .strong()
                        .color(Color32::from_rgb(255, 160, 40)),
                );
            }

            let info_color = Color32::from_rgb(200, 200, 220);
            let label = if has_prev { "Current" } else { "Site" };
            ui.label(
                RichText::new(format!("{}: {}", label, state.viz_state.site_id))
                    .monospace()
                    .size(12.0)
                    .color(info_color),
            );
            ui.label(
                RichText::new(format!("Time: {}", state.viz_state.timestamp))
                    .monospace()
                    .size(12.0)
                    .color(info_color),
            );
            ui.label(
                RichText::new(format!("Elev: {}", state.viz_state.elevation))
                    .monospace()
                    .size(12.0)
                    .color(info_color),
            );
            if let Some(end_secs) = state.viz_state.data_staleness_secs {
                let color = age_color(end_secs);
                let age_text = if end_secs < AGE_RANGE_COLLAPSE_SECS {
                    if let Some(start_secs) = state.viz_state.data_staleness_start_secs {
                        format!("Age: {} – {}", format_age(start_secs), format_age(end_secs),)
                    } else {
                        format!("Age: {}", format_age(end_secs))
                    }
                } else {
                    format!("Age: {}", format_age(end_secs))
                };
                ui.label(RichText::new(age_text).monospace().size(12.0).color(color));
            }

            // Previous sweep info during sweep animation
            if let Some((prev_elev, prev_start, prev_end)) = state.viz_state.prev_sweep_overlay {
                ui.add_space(2.0);
                let prev_color = Color32::from_rgb(170, 170, 190);
                ui.label(
                    RichText::new("Previous")
                        .monospace()
                        .size(12.0)
                        .color(prev_color),
                );
                let prev_time =
                    format_unix_timestamp((prev_start + prev_end) / 2.0, state.use_local_time);
                ui.label(
                    RichText::new(format!("Time: {}", prev_time))
                        .monospace()
                        .size(12.0)
                        .color(prev_color),
                );
                ui.label(
                    RichText::new(format!("Elev: {:.1}\u{00B0}", prev_elev))
                        .monospace()
                        .size(12.0)
                        .color(prev_color),
                );
            }
        });
    });
}

/// Draw a compass rose in the bottom-left of the globe view.
/// Shows N/S/E/W with the current camera bearing so the user always knows orientation.
fn draw_compass(ui: &mut egui::Ui, rect: &Rect, camera: &GlobeCamera) {
    let painter = ui.painter();
    let radius = 28.0f32;
    let margin = 16.0f32;
    let center = Pos2::new(
        rect.left() + margin + radius,
        rect.bottom() - margin - radius,
    );

    // Background circle
    painter.circle_filled(
        center,
        radius + 4.0,
        Color32::from_rgba_unmultiplied(15, 15, 25, 180),
    );
    painter.circle_stroke(
        center,
        radius + 4.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(80, 80, 100, 160)),
    );

    // Compute compass rotation to match the camera's on-screen orientation.
    // In SiteOrbit, orbit_bearing is where the camera IS, not where it looks.
    // The camera looks FROM the bearing TOWARD the site, so the viewing direction
    // is bearing + 180°. We add π to account for this.
    let rotation_rad = match camera.mode {
        crate::geo::camera::CameraMode::SiteOrbit => {
            std::f32::consts::PI - camera.orbit_bearing.to_radians()
        }
        _ => 0.0,
    } - camera.rotation.to_radians();

    // Cardinal directions
    let cardinals = [("N", 0.0), ("E", 90.0), ("S", 180.0), ("W", 270.0)];
    for (label, bearing_deg) in cardinals {
        let angle = (bearing_deg as f32).to_radians() + rotation_rad;
        // angle=0 → up (screen -Y), rotating CW
        let dir = Vec2::new(angle.sin(), -angle.cos());
        let label_pos = center + dir * (radius - 2.0);

        let (color, size) = if label == "N" {
            (Color32::from_rgb(255, 80, 80), 13.0)
        } else {
            (Color32::from_rgba_unmultiplied(180, 180, 200, 200), 11.0)
        };

        painter.text(
            label_pos,
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(size),
            color,
        );
    }

    // Small tick marks for intercardinals
    for i in 0..8 {
        let angle = (i as f32 * 45.0).to_radians() + rotation_rad;
        if i % 2 == 0 {
            continue; // skip cardinals, already labeled
        }
        let dir = Vec2::new(angle.sin(), -angle.cos());
        let inner = center + dir * (radius - 8.0);
        let outer = center + dir * (radius - 2.0);
        painter.line_segment(
            [inner, outer],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 120, 140, 140)),
        );
    }

    // Center dot
    painter.circle_filled(
        center,
        2.0,
        Color32::from_rgba_unmultiplied(150, 150, 170, 160),
    );
}

/// Draw a vertical color scale legend on the right side of the canvas.
fn draw_color_scale(ui: &mut egui::Ui, rect: &Rect, product: &crate::state::RadarProduct) {
    use crate::nexrad::gpu_renderer::{build_reflectivity_lut, product_value_range};
    use nexrad_render::Product;

    let product_nr = match product {
        crate::state::RadarProduct::Reflectivity => Product::Reflectivity,
        crate::state::RadarProduct::Velocity => Product::Velocity,
        crate::state::RadarProduct::SpectrumWidth => Product::SpectrumWidth,
        crate::state::RadarProduct::DifferentialReflectivity => Product::DifferentialReflectivity,
        crate::state::RadarProduct::CorrelationCoefficient => Product::CorrelationCoefficient,
        crate::state::RadarProduct::DifferentialPhase => Product::DifferentialPhase,
        crate::state::RadarProduct::ClutterFilterPower => Product::ClutterFilterPower,
    };

    let (min_val, max_val) = product_value_range(product_nr);

    // Build the LUT (1024 entries) — for reflectivity uses OKLab, others use crate scale
    let lut = if matches!(product, crate::state::RadarProduct::Reflectivity) {
        build_reflectivity_lut(min_val, max_val)
    } else {
        let color_scale = super::super::nexrad::gpu_renderer::continuous_color_scale(product_nr);
        let lut_size = 1024usize;
        let mut data = Vec::with_capacity(lut_size * 4);
        for i in 0..lut_size {
            let t = i as f32 / (lut_size - 1) as f32;
            let value = min_val + t * (max_val - min_val);
            let color = color_scale.color(value);
            let rgba = color.to_rgba8();
            data.extend_from_slice(&rgba);
        }
        data
    };

    let bar_width = 16.0f32;
    let margin = 14.0f32;
    let top_margin = 20.0f32;
    let bottom_margin = 20.0f32;
    let bar_height = (rect.height() - top_margin - bottom_margin).clamp(100.0, 320.0);

    let bar_left = rect.right() - margin - bar_width;
    let bar_top = rect.top() + top_margin;

    let painter = ui.painter();
    let lut_size = 1024usize;

    // Draw the color bar as horizontal slices (bottom = low, top = high)
    let num_slices = bar_height as usize;
    for s in 0..num_slices {
        let frac = s as f32 / (num_slices - 1) as f32;
        let lut_idx = ((1.0 - frac) * (lut_size - 1) as f32) as usize; // flip: top=high
        let lut_idx = lut_idx.min(lut_size - 1);
        let r = lut[lut_idx * 4];
        let g = lut[lut_idx * 4 + 1];
        let b = lut[lut_idx * 4 + 2];
        let a = lut[lut_idx * 4 + 3];

        let y = bar_top + s as f32;
        let slice_rect = Rect::from_min_size(Pos2::new(bar_left, y), Vec2::new(bar_width, 1.5));
        painter.rect_filled(slice_rect, 0.0, Color32::from_rgba_unmultiplied(r, g, b, a));
    }

    // Outline
    let bar_rect = Rect::from_min_size(
        Pos2::new(bar_left, bar_top),
        Vec2::new(bar_width, bar_height),
    );
    painter.rect_stroke(
        bar_rect,
        0.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 120, 130, 180)),
        StrokeKind::Outside,
    );

    // Tick labels
    let range = max_val - min_val;
    let tick_step = if range > 200.0 {
        60.0
    } else if range > 60.0 {
        10.0
    } else if range > 10.0 {
        5.0
    } else if range > 2.0 {
        1.0
    } else {
        0.2
    };

    let label_x = bar_left - 4.0;
    let mut val = (min_val / tick_step).ceil() * tick_step;
    while val <= max_val {
        let frac = (val - min_val) / range;
        let y = bar_top + bar_height * (1.0 - frac);

        // Tick line
        painter.line_segment(
            [Pos2::new(bar_left - 3.0, y), Pos2::new(bar_left, y)],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(180, 180, 190, 200)),
        );

        // Label
        let label = if tick_step < 1.0 {
            format!("{:.1}", val)
        } else {
            format!("{:.0}", val)
        };
        painter.text(
            Pos2::new(label_x, y),
            egui::Align2::RIGHT_CENTER,
            label,
            egui::FontId::monospace(10.0),
            Color32::from_rgba_unmultiplied(180, 180, 190, 220),
        );
        val += tick_step;
    }

    // Unit label at top
    painter.text(
        Pos2::new(bar_left + bar_width * 0.5, bar_top - 6.0),
        egui::Align2::CENTER_BOTTOM,
        product.unit(),
        egui::FontId::monospace(10.0),
        Color32::from_rgba_unmultiplied(160, 160, 170, 200),
    );
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
/// Returns `Some(azimuth_degrees)` when playing at slow speeds (<= 30s/s)
/// and the playback position falls within a sweep.
///
/// The animation uses linear interpolation: azimuth = (sweep_progress * 360).
/// Different sweeps have different durations (higher elevations are often faster),
/// so the rotation speed naturally varies between sweeps. This accurately reflects
/// how the radar instrument operates — the antenna changes rotation speed at
/// different elevation cuts. If per-radial azimuth data is available, we use it
/// for more accurate positioning.
/// Returns `Some((current_azimuth, start_azimuth))` when the sweep line should
/// be visible. `start_azimuth` is the azimuth where the sweep began collecting
/// data (the first radial), so the "already swept" arc runs CW from
/// `start_azimuth` to `current_azimuth`.
fn compute_sweep_line_azimuth(state: &AppState) -> Option<(f32, f32)> {
    if state
        .playback_state
        .speed
        .timeline_seconds_per_real_second()
        > 30.0
    {
        return None;
    }

    let ts = state.playback_state.playback_position();

    // Try to find azimuth from persisted scan/sweep data first
    if let Some(scan) = state.radar_timeline.find_scan_at_timestamp(ts) {
        if let Some((_, sweep)) = scan.find_sweep_at_timestamp(ts) {
            let duration = sweep.end_time - sweep.start_time;
            if duration > 0.0 {
                // If per-radial azimuth data is available, interpolate from actual azimuths
                if !sweep.radials.is_empty() {
                    let start_az = sweep.radials[0].azimuth;
                    let mut last_az = start_az;
                    let mut last_time = sweep.start_time;
                    let mut next_az = start_az + 360.0;
                    let mut next_time = sweep.end_time;

                    for radial in &sweep.radials {
                        if radial.start_time <= ts {
                            last_az = radial.azimuth;
                            last_time = radial.start_time;
                        } else {
                            next_az = radial.azimuth;
                            next_time = radial.start_time;
                            break;
                        }
                    }

                    let mut delta_az = next_az - last_az;
                    if delta_az < -180.0 {
                        delta_az += 360.0;
                    } else if delta_az > 180.0 {
                        delta_az -= 360.0;
                    }

                    let dt = next_time - last_time;
                    if dt > 0.0 {
                        let frac = (ts - last_time) / dt;
                        let az = ((last_az + delta_az * frac as f32) % 360.0 + 360.0) % 360.0;
                        return Some((az, start_az));
                    }
                    return Some((last_az, start_az));
                }

                // Fallback: linear interpolation assuming uniform rotation from start azimuth
                let start_az = sweep.start_azimuth;
                let progress = (ts - sweep.start_time) / duration;
                let az = ((start_az + progress as f32 * 360.0) % 360.0 + 360.0) % 360.0;
                return Some((az, start_az));
            }
        }
    }

    // In live mode, extrapolate from the last known radial azimuth/time
    if state.live_mode_state.is_active() {
        let now = js_sys::Date::now() / 1000.0;
        if let Some(az) = state.live_mode_state.estimated_azimuth(now) {
            // Live mode doesn't track start azimuth; assume 0°
            return Some((az, 0.0));
        }
    }

    None
}

fn render_radar_sweep(
    painter: &Painter,
    projection: &MapProjection,
    state: &AppState,
    sweep_info: Option<(f32, f32)>,
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

    // Draw the sweep line and donut chart if sweep animation is active
    if let Some((az, start_az)) = sweep_info {
        let angle_rad = (az - 90.0) * PI / 180.0;
        let end_x = center.x + radius * angle_rad.cos();
        let end_y = center.y + radius * angle_rad.sin();

        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(3.0, radar::SWEEP_LINE),
        );

        // Donut chart showing current vs previous sweep regions
        if state.render_processing.sweep_animation {
            draw_sweep_donut(painter, center, radius, az, start_az, state);
        }
    }
}

/// Draw a background-boxed label at a given angle around the donut.
fn draw_boundary_label(
    painter: &Painter,
    center: Pos2,
    label_radius: f32,
    azimuth_deg: f32,
    text: &str,
    font: &egui::FontId,
) {
    let label_angle = (azimuth_deg - 90.0) * PI / 180.0;
    let label_pos = Pos2::new(
        center.x + label_radius * label_angle.cos(),
        center.y + label_radius * label_angle.sin(),
    );
    let align = sweep_label_align(azimuth_deg);
    let galley = painter.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE);
    let text_size = galley.size();
    let bg_pos = align_pos(label_pos, text_size, align);
    let padding = Vec2::new(3.0, 2.0);
    painter.rect_filled(
        Rect::from_min_size(bg_pos - padding, text_size + padding * 2.0),
        3.0,
        Color32::from_rgba_unmultiplied(15, 15, 25, 200),
    );
    painter.galley(bg_pos, galley, Color32::WHITE);
}

/// Draw a donut-chart ring around the radar showing which azimuthal regions
/// belong to the current sweep vs the previous sweep, with time labels at
/// both discontinuity boundaries.
fn draw_sweep_donut(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    sweep_az: f32,
    sweep_start: f32,
    state: &AppState,
) {
    let donut_inner = radius + 4.0;
    let donut_outer = radius + 10.0;
    let donut_mid = (donut_inner + donut_outer) / 2.0;
    let donut_width = donut_outer - donut_inner;

    // Current sweep arc: from sweep_start CW to sweep_az
    let current_color = Color32::from_rgba_unmultiplied(80, 200, 120, 160);
    // Previous sweep arc: from sweep_az CW to sweep_start (the rest)
    let prev_color = Color32::from_rgba_unmultiplied(120, 120, 180, 120);

    // Draw arcs as series of short line segments
    let segments = 180;
    let swept_arc_deg = (sweep_az - sweep_start).rem_euclid(360.0);

    for i in 0..segments {
        let frac_start = i as f32 / segments as f32;
        let frac_end = (i + 1) as f32 / segments as f32;
        let deg_start = frac_start * 360.0;
        let deg_end = frac_end * 360.0;

        // Is this segment in the current sweep region?
        let mid_deg = (deg_start + deg_end) / 2.0;
        let is_current = mid_deg < swept_arc_deg;

        let color = if is_current {
            current_color
        } else {
            prev_color
        };

        let a1 = ((sweep_start + deg_start) - 90.0) * PI / 180.0;
        let a2 = ((sweep_start + deg_end) - 90.0) * PI / 180.0;

        let p1 = Pos2::new(
            center.x + donut_mid * a1.cos(),
            center.y + donut_mid * a1.sin(),
        );
        let p2 = Pos2::new(
            center.x + donut_mid * a2.cos(),
            center.y + donut_mid * a2.sin(),
        );

        painter.line_segment([p1, p2], Stroke::new(donut_width, color));
    }

    // Time labels at both discontinuity boundaries
    let label_radius = donut_outer + 14.0;
    let label_font = egui::FontId::monospace(10.0);
    let use_local = state.use_local_time;

    // Boundary 1: sweep line (sweep_az) — playback time | prev sweep time at this azimuth
    // The data right after the sweep line is from the previous sweep at the same
    // azimuth. Interpolate into the previous sweep's time range based on where
    // the sweep line sits relative to the sweep start azimuth (both sweeps rotate
    // through the same azimuths in the same order).
    let playback_time_str = format_time_short(state.playback_state.playback_position(), use_local);
    let prev_at_az_str = state
        .viz_state
        .prev_sweep_overlay
        .map(|(_, prev_start, prev_end)| {
            // swept_arc_deg is how far the current sweep has progressed (0..360).
            // The previous sweep data at the sweep line was collected at the same
            // fractional position through its own rotation.
            let frac = (swept_arc_deg / 360.0).clamp(0.0, 1.0) as f64;
            let prev_time_at_az = prev_start + frac * (prev_end - prev_start);
            format_time_short(prev_time_at_az, use_local)
        });

    let sweep_line_label = match prev_at_az_str {
        Some(ref prev) => format!("{} | {}", playback_time_str, prev),
        None => playback_time_str,
    };
    draw_boundary_label(
        painter,
        center,
        label_radius,
        sweep_az,
        &sweep_line_label,
        &label_font,
    );

    // Boundary 2: sweep start (sweep_start) — current sweep start | prev sweep end
    // Only draw when prev_sweep_overlay exists (compositing two sweeps) and
    // the swept arc is wide enough to avoid overlapping text.
    if swept_arc_deg >= 30.0 {
        if let Some((_, _, prev_end)) = state.viz_state.prev_sweep_overlay {
            // Look up the current sweep's actual start time from the timeline
            // (rendered_sweep_start_secs tracks the last decoded sweep, which may
            // be stale or from a different elevation).
            let playback_ts = state.playback_state.playback_position();
            let displayed_elev = state.displayed_sweep_elevation_number;
            let current_sweep_start_secs = state
                .radar_timeline
                .find_recent_scan(playback_ts, 15.0 * 60.0)
                .and_then(|scan| {
                    scan.sweeps
                        .iter()
                        .filter(|s| Some(s.elevation_number) == displayed_elev)
                        .rfind(|s| s.start_time <= playback_ts)
                        .or_else(|| {
                            scan.sweeps
                                .iter()
                                .find(|s| Some(s.elevation_number) == displayed_elev)
                        })
                        .map(|s| s.start_time)
                });

            let start_time_str = current_sweep_start_secs.map(|s| format_time_short(s, use_local));
            let prev_end_str2 = format_time_short(prev_end, use_local);

            let start_label = match start_time_str {
                Some(ref start) => format!("{} | {}", start, prev_end_str2),
                None => prev_end_str2,
            };
            draw_boundary_label(
                painter,
                center,
                label_radius,
                sweep_start,
                &start_label,
                &label_font,
            );
        }
    }
}

/// Format a timestamp as HH:MM:SS for compact display.
fn format_time_short(ts: f64, use_local: bool) -> String {
    if use_local {
        let d = js_sys::Date::new_0();
        d.set_time(ts * 1000.0);
        format!(
            "{:02}:{:02}:{:02}",
            d.get_hours(),
            d.get_minutes(),
            d.get_seconds()
        )
    } else {
        use chrono::{TimeZone, Timelike, Utc};
        let secs = ts.floor() as i64;
        match Utc.timestamp_opt(secs, 0) {
            chrono::LocalResult::Single(dt) => {
                format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second())
            }
            _ => format!("{:.0}", ts),
        }
    }
}

/// Choose text alignment for a sweep boundary label based on its angle.
fn sweep_label_align(az_deg: f32) -> egui::Align2 {
    // Determine which quadrant the label falls in
    let az = az_deg.rem_euclid(360.0);
    if !(45.0..315.0).contains(&az) {
        egui::Align2::CENTER_BOTTOM // top
    } else if az < 135.0 {
        egui::Align2::LEFT_CENTER // right
    } else if az < 225.0 {
        egui::Align2::CENTER_TOP // bottom
    } else {
        egui::Align2::RIGHT_CENTER // left
    }
}

/// Convert an Align2 + position into a top-left position for rect placement.
fn align_pos(pos: Pos2, size: Vec2, align: egui::Align2) -> Pos2 {
    let x = match align.x() {
        egui::Align::Min => pos.x,
        egui::Align::Center => pos.x - size.x / 2.0,
        egui::Align::Max => pos.x - size.x,
    };
    let y = match align.y() {
        egui::Align::Min => pos.y,
        egui::Align::Center => pos.y - size.y / 2.0,
        egui::Align::Max => pos.y - size.y,
    };
    Pos2::new(x, y)
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

/// Format a Unix timestamp (seconds) as a time string.
///
/// When `use_local` is true, formats in the browser's local timezone via `js_sys::Date`.
/// Otherwise formats as UTC via chrono.
fn format_unix_timestamp(ts: f64, use_local: bool) -> String {
    if use_local {
        let d = js_sys::Date::new_0();
        d.set_time(ts * 1000.0);
        let h = d.get_hours();
        let m = d.get_minutes();
        let s = d.get_seconds();
        let ms = d.get_milliseconds();
        format!("{h:02}:{m:02}:{s:02}.{ms:03} Local")
    } else {
        use chrono::{TimeZone, Utc};
        let secs = ts.floor() as i64;
        let millis = ((ts - ts.floor()) * 1000.0).round() as u32;
        match Utc.timestamp_opt(secs, millis * 1_000_000) {
            chrono::LocalResult::Single(dt) => dt.format("%H:%M:%S%.3f UTC").to_string(),
            _ => format!("{:.3}s", ts),
        }
    }
}

/// Render inspector tooltip showing lat/lon and data value at hover position.
#[allow(clippy::too_many_arguments)]
fn render_inspector(
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
            let v = renderer.value_at_polar_sweep_aware(azimuth_deg as f32, range_km, sweep_params);
            let t = renderer.collection_time_at_polar_sweep_aware(azimuth_deg as f32, sweep_params);
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
