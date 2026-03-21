use super::colors::{canvas as canvas_colors, radar, sites as site_colors};
use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::{GeoLineRenderer, GlobeCamera, GlobeRenderer, MapProjection};
use crate::nexrad::{RadarGpuRenderer, RADAR_COVERAGE_RANGE_KM};
use crate::state::{AppState, GeoLayerVisibility};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Vec2};
use eframe::egui::{Stroke, StrokeKind};
use geo_types::Coord;
use glow::HasContext;
use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

use super::canvas::{
    age_color, format_age, format_age_compact, format_time_short, format_unix_timestamp_with_date,
    AGE_RANGE_COLLAPSE_SECS, ARCHIVE_AGE_THRESHOLD_SECS,
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_globe(
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

pub(crate) fn draw_overlay_info(ui: &mut egui::Ui, rect: &Rect, state: &AppState) {
    let has_prev = state.viz_state.prev_sweep_overlay.is_some();
    let overlay_pos = rect.left_top() + Vec2::new(10.0, 10.0);
    let overlay_height = if has_prev { 130.0 } else { 90.0 };
    let overlay_rect = Rect::from_min_size(overlay_pos, Vec2::new(290.0, overlay_height));

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
                let prev_time = format_unix_timestamp_with_date(
                    (prev_start + prev_end) / 2.0,
                    state.use_local_time,
                );
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

pub(crate) fn draw_compass(ui: &mut egui::Ui, rect: &Rect, camera: &GlobeCamera) {
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

pub(crate) fn draw_color_scale(
    ui: &mut egui::Ui,
    rect: &Rect,
    product: &crate::state::RadarProduct,
) {
    use crate::nexrad::color_table::{build_reflectivity_lut, product_value_range};
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
        let color_scale = crate::nexrad::color_table::continuous_color_scale(product_nr);
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

pub(crate) fn render_radar_sweep(
    painter: &Painter,
    projection: &MapProjection,
    state: &AppState,
    sweep_info: Option<(f32, f32)>,
    stale: bool,
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
        let (start_line_color, sweep_line_color, sweep_line_width) = if stale {
            (
                radar::sweep_start_line_stale(),
                radar::sweep_line_stale(),
                2.0,
            )
        } else {
            (radar::sweep_start_line(), radar::SWEEP_LINE, 3.0)
        };

        // Thin line at sweep start boundary
        let start_angle_rad = (start_az - 90.0) * PI / 180.0;
        let start_end = Pos2::new(
            center.x + radius * start_angle_rad.cos(),
            center.y + radius * start_angle_rad.sin(),
        );
        painter.line_segment([center, start_end], Stroke::new(1.5, start_line_color));

        // Main sweep line
        let angle_rad = (az - 90.0) * PI / 180.0;
        let end_x = center.x + radius * angle_rad.cos();
        let end_y = center.y + radius * angle_rad.sin();
        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(sweep_line_width, sweep_line_color),
        );

        // Draw chunk boundary lines across the radar render during live streaming
        if state.live_mode_state.is_active() {
            let chunks = &state.live_mode_state.current_elev_chunks;
            let boundary_line_color = Color32::from_rgba_unmultiplied(200, 200, 220, 100);
            for &(_, last_az, _) in chunks.iter().take(chunks.len().saturating_sub(1)) {
                let a = (last_az - 90.0) * PI / 180.0;
                let p_end = Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin());
                painter.line_segment([center, p_end], Stroke::new(1.0, boundary_line_color));
            }
        }

        // Donut chart showing current vs previous sweep regions
        if state.live_mode_state.is_active() {
            draw_live_sweep_donut(painter, center, radius, az, start_az, state);
        } else if state.effective_sweep_animation() {
            if stale {
                draw_sweep_donut_stale(painter, center, radius);
            } else {
                draw_sweep_donut(painter, center, radius, az, start_az, state);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_boundary_label(
    painter: &Painter,
    center: Pos2,
    label_radius: f32,
    azimuth_deg: f32,
    left_text: &str,
    right_text: Option<&str>,
    left_color: Color32,
    right_color: Color32,
    font: &egui::FontId,
) {
    let label_angle = (azimuth_deg - 90.0) * PI / 180.0;
    let label_pos = Pos2::new(
        center.x + label_radius * label_angle.cos(),
        center.y + label_radius * label_angle.sin(),
    );
    let align = sweep_label_align(azimuth_deg);

    // Build a LayoutJob with colored segments
    let mut job = egui::text::LayoutJob::default();
    job.append(
        left_text,
        0.0,
        egui::TextFormat {
            font_id: font.clone(),
            color: left_color,
            ..Default::default()
        },
    );
    if let Some(right) = right_text {
        job.append(
            " | ",
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: Color32::from_rgb(120, 120, 140),
                ..Default::default()
            },
        );
        job.append(
            right,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: right_color,
                ..Default::default()
            },
        );
    }

    let galley = painter.layout_job(job);
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

fn draw_sweep_donut_stale(painter: &Painter, center: Pos2, radius: f32) {
    let donut_inner = radius + 4.0;
    let donut_outer = radius + 10.0;
    let donut_mid = (donut_inner + donut_outer) / 2.0;
    let donut_width = donut_outer - donut_inner;

    let color = Color32::from_rgba_unmultiplied(100, 100, 110, 80);
    let segments = 180;

    for i in 0..segments {
        let frac_start = i as f32 / segments as f32;
        let frac_end = (i + 1) as f32 / segments as f32;

        let a1 = (frac_start * 360.0 - 90.0) * PI / 180.0;
        let a2 = (frac_end * 360.0 - 90.0) * PI / 180.0;

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
}

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

    // Color constants for boundary label text
    let current_text_color = Color32::from_rgb(100, 220, 140); // green for current sweep
    let prev_text_color = Color32::from_rgb(160, 160, 220); // purple for previous sweep

    // Time labels at both discontinuity boundaries
    let label_radius = donut_outer + 14.0;
    let label_font = egui::FontId::monospace(10.0);
    let use_local = state.use_local_time;

    // Boundary 1: sweep line (sweep_az) — playback time | prev sweep time at this azimuth
    let playback_ts = state.playback_state.playback_position();
    let mut playback_time_str = format_time_short(playback_ts, use_local);
    if let Some(age) = format_age_compact(playback_ts) {
        playback_time_str.push(' ');
        playback_time_str.push_str(&age);
    }

    let prev_at_az_str = state
        .viz_state
        .prev_sweep_overlay
        .map(|(_, prev_start, prev_end)| {
            let frac = (swept_arc_deg / 360.0).clamp(0.0, 1.0) as f64;
            let prev_time_at_az = prev_start + frac * (prev_end - prev_start);
            let mut s = format_time_short(prev_time_at_az, use_local);
            if let Some(age) = format_age_compact(prev_time_at_az) {
                s.push(' ');
                s.push_str(&age);
            }
            s
        });

    draw_boundary_label(
        painter,
        center,
        label_radius,
        sweep_az,
        &playback_time_str,
        prev_at_az_str.as_deref(),
        current_text_color,
        prev_text_color,
        &label_font,
    );

    // Boundary 2: sweep start (sweep_start) — current sweep start | prev sweep end
    // Look up the current sweep's actual start time from the timeline
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

    if swept_arc_deg >= 30.0 {
        if let Some((_, _, prev_end)) = state.viz_state.prev_sweep_overlay {
            // Both sweeps: current start (green) | prev end (purple)
            let start_time_str = current_sweep_start_secs.map(|s| {
                let mut t = format_time_short(s, use_local);
                if let Some(age) = format_age_compact(s) {
                    t.push(' ');
                    t.push_str(&age);
                }
                t
            });
            let mut prev_end_str = format_time_short(prev_end, use_local);
            if let Some(age) = format_age_compact(prev_end) {
                prev_end_str.push(' ');
                prev_end_str.push_str(&age);
            }

            let (left, right, left_c, right_c) = match start_time_str {
                Some(ref start) => (
                    start.as_str(),
                    Some(prev_end_str.as_str()),
                    current_text_color,
                    prev_text_color,
                ),
                None => (
                    prev_end_str.as_str(),
                    None,
                    prev_text_color,
                    prev_text_color,
                ),
            };
            draw_boundary_label(
                painter,
                center,
                label_radius,
                sweep_start,
                left,
                right,
                left_c,
                right_c,
                &label_font,
            );
        } else if let Some(start_secs) = current_sweep_start_secs {
            // Single sweep only: show start time in green (no separator)
            let mut start_str = format_time_short(start_secs, use_local);
            if let Some(age) = format_age_compact(start_secs) {
                start_str.push(' ');
                start_str.push_str(&age);
            }
            draw_boundary_label(
                painter,
                center,
                label_radius,
                sweep_start,
                &start_str,
                None,
                current_text_color,
                current_text_color,
                &label_font,
            );
        }
    }
}

fn draw_live_sweep_donut(
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

    let live = &state.live_mode_state;
    let chunks = &live.current_elev_chunks;

    // Compute the angular extent of actual received chunk data (from sweep_start).
    // This is independent of the extrapolated sweep line — chunks stay visible until
    // the elevation actually completes, even after the sweep line wraps past 360°.
    let data_arc_deg = if chunks.is_empty() {
        0.0
    } else {
        chunks
            .iter()
            .map(|&(_, last_az, _)| (last_az - sweep_start).rem_euclid(360.0))
            .fold(0.0f32, f32::max)
    };

    // Distinct hues for chunk wedges (up to 8 before cycling)
    let chunk_colors = [
        Color32::from_rgba_unmultiplied(70, 200, 110, 160), // green
        Color32::from_rgba_unmultiplied(80, 180, 220, 160), // cyan
        Color32::from_rgba_unmultiplied(220, 180, 70, 160), // amber
        Color32::from_rgba_unmultiplied(180, 100, 220, 160), // purple
        Color32::from_rgba_unmultiplied(220, 110, 80, 160), // coral
        Color32::from_rgba_unmultiplied(100, 220, 180, 160), // teal
        Color32::from_rgba_unmultiplied(220, 140, 180, 160), // pink
        Color32::from_rgba_unmultiplied(160, 220, 80, 160), // lime
    ];
    let prev_color = Color32::from_rgba_unmultiplied(120, 120, 180, 120);
    let boundary_color = Color32::from_rgba_unmultiplied(200, 200, 220, 180);

    // Draw arcs segment by segment
    let seg_count = 360;
    for i in 0..seg_count {
        let deg = (i as f32 + 0.5) * 360.0 / seg_count as f32;
        let abs_deg = (sweep_start + deg).rem_euclid(360.0);

        // Color by actual chunk coverage, not sweep line position.
        // This way chunks persist visually until the elevation completes,
        // even when the sweep line extrapolation wraps past 360°.
        let color = {
            let chunk_match = chunks.iter().position(|&(first, last, _)| {
                let arc = (last - first).rem_euclid(360.0);
                let from_first = (abs_deg - first).rem_euclid(360.0);
                from_first <= arc
            });
            if let Some(idx) = chunk_match {
                chunk_colors[idx % chunk_colors.len()]
            } else {
                prev_color
            }
        };

        let a1 = ((sweep_start + deg - 0.5) - 90.0) * PI / 180.0;
        let a2 = ((sweep_start + deg + 0.5) - 90.0) * PI / 180.0;
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

    // Draw thin boundary lines between chunks
    for &(_, last_az, _) in chunks.iter().take(chunks.len().saturating_sub(1)) {
        let a = (last_az - 90.0) * PI / 180.0;
        let p_inner = Pos2::new(
            center.x + donut_inner * a.cos(),
            center.y + donut_inner * a.sin(),
        );
        let p_outer = Pos2::new(
            center.x + donut_outer * a.cos(),
            center.y + donut_outer * a.sin(),
        );
        painter.line_segment([p_inner, p_outer], Stroke::new(1.5, boundary_color));
    }

    // Labels
    let label_radius = donut_outer + 14.0;
    let label_font = egui::FontId::monospace(10.0);
    let current_label_color = Color32::from_rgb(100, 220, 140);
    let prev_label_color = Color32::from_rgb(160, 160, 220);

    // Helper: look up elevation angle from VCP data by elevation number
    let elev_angle_str = |elev_num: u8| -> String {
        live.current_vcp_pattern
            .as_ref()
            .and_then(|vcp| {
                vcp.elevations
                    .get(elev_num.saturating_sub(1) as usize)
                    .map(|el| format!("{:.1}\u{00B0}", el.angle))
            })
            .unwrap_or_default()
    };

    // Per-chunk labels (only when chunks have enough angular separation)
    if chunks.len() > 1 {
        for (i, &(first_az, last_az, radial_count)) in chunks.iter().enumerate() {
            let arc = (last_az - first_az).rem_euclid(360.0);
            if arc < 15.0 {
                continue; // too narrow for a label
            }
            let mid_az = first_az + arc / 2.0;
            let label = format!("C{} \u{00B7} {}r", i + 1, radial_count);
            draw_boundary_label(
                painter,
                center,
                label_radius,
                mid_az,
                &label,
                None,
                current_label_color,
                current_label_color,
                &label_font,
            );
        }
    }

    // Overall sweep info label at the midpoint of all current data
    {
        let elev_num = live
            .current_in_progress_elevation
            .map(|e| format!("{}", e))
            .unwrap_or_else(|| "?".to_string());
        let elev_angle = live
            .current_in_progress_elevation
            .map(&elev_angle_str)
            .unwrap_or_default();
        let radials = live.current_in_progress_radials.unwrap_or(0);
        let completed = live.elevations_received.len();
        let expected = live
            .expected_elevation_count
            .map(|n| format!("/{}", n))
            .unwrap_or_default();

        // Place at sweep line position (the leading edge)
        let label = format!(
            "Sweep {} {} \u{00B7} {}r \u{00B7} {}{} elev",
            elev_num, elev_angle, radials, completed, expected
        );
        draw_boundary_label(
            painter,
            center,
            label_radius,
            sweep_az,
            &label,
            None,
            current_label_color,
            current_label_color,
            &label_font,
        );
    }

    // Previous sweep label at the midpoint of the purple (non-chunk) arc
    let prev_arc_deg = 360.0 - data_arc_deg;
    if prev_arc_deg > 30.0 {
        let prev_elev = live.elevations_received.last().copied();
        if let Some(pe) = prev_elev {
            let label = format!("Prev sweep {} {}", pe, elev_angle_str(pe));
            let angle = sweep_start + data_arc_deg + prev_arc_deg / 2.0;
            draw_boundary_label(
                painter,
                center,
                label_radius,
                angle,
                &label,
                None,
                prev_label_color,
                prev_label_color,
                &label_font,
            );
        }
    }
}

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

pub(crate) fn render_nexrad_sites(
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
