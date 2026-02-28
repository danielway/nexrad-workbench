//! Central canvas UI: radar visualization area.

use super::colors::{canvas as canvas_colors, radar, sites as site_colors};
use super::left_panel::RadarPosition;
use crate::data::{get_site, NEXRAD_SITES};
use crate::geo::{GeoLayerSet, MapProjection};
use crate::nexrad::{
    radar_coverage_range_km, render_radials_to_image, render_sweep_field_to_image, RadarCacheKey,
    RadarTextureCache, RenderSweep, VolumeRing,
};
use crate::state::{AppState, GeoLayerVisibility, SmoothingMode};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use geo_types::Coord;
use std::f32::consts::PI;

/// Render canvas with optional geographic layers and NEXRAD data.
pub fn render_canvas_with_geo(
    ctx: &egui::Context,
    state: &mut AppState,
    geo_layers: Option<&GeoLayerSet>,
    volume_ring: &VolumeRing,
    texture_cache: &mut RadarTextureCache,
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

        // Build dynamic render sweep and render radar data
        let radar_position = if !volume_ring.is_empty() {
            let playback_ts_ms = (state.playback_state.playback_position() * 1000.0) as i64;

            let render_sweep = RenderSweep::from_volume_ring(
                volume_ring,
                state.viz_state.target_elevation,
                playback_ts_ms,
            );

            // Compute staleness for fixed-tilt mode
            {
                let now_ms = js_sys::Date::now() as i64;
                state.viz_state.data_staleness_secs = render_sweep.staleness_seconds(now_ms);
            }

            // Render if we have radials
            if !render_sweep.is_empty() {
                let render_product = state.viz_state.product.to_render_product();
                let render_interp = state.viz_state.interpolation.to_render_interpolation();
                let processing = state.viz_state.processing;
                if let Some(render_time_ms) = render_dynamic_sweep(
                    ctx,
                    &painter,
                    &projection,
                    &render_sweep,
                    texture_cache,
                    &rect,
                    state.viz_state.center_lat,
                    state.viz_state.center_lon,
                    render_product,
                    render_interp,
                    processing,
                ) {
                    state.session_stats.record_render_time(render_time_ms);
                }

                // Render data age timestamp markers at cardinal azimuths
                render_age_markers(&painter, &rect, state, &render_sweep);
            }

            // Get radar position from most recent radial in the sweep
            render_sweep
                .most_recent_radial()
                .map(|radial| RadarPosition {
                    azimuth: radial.azimuth_angle_degrees(),
                    elevation: radial.elevation_angle_degrees(),
                })
        } else {
            None
        };

        // Show sweep line only in real-time playback mode
        let azimuth = if state.playback_state.speed == crate::state::PlaybackSpeed::Realtime {
            radar_position.as_ref().map(|pos| pos.azimuth)
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

/// Render a dynamic sweep as a cached texture.
#[allow(clippy::too_many_arguments)]
fn render_dynamic_sweep(
    ctx: &egui::Context,
    painter: &Painter,
    projection: &MapProjection,
    render_sweep: &RenderSweep,
    cache: &mut RadarTextureCache,
    rect: &Rect,
    radar_lat: f64,
    radar_lon: f64,
    product: nexrad_render::Product,
    interpolation: nexrad_render::Interpolation,
    processing: crate::state::ProcessingConfig,
) -> Option<f64> {
    let render_size: (usize, usize) = (800, 800);

    let content_signature = render_sweep.cache_signature();
    let cache_key = RadarCacheKey::for_dynamic_sweep(content_signature, 0, render_size)
        .with_product(product as u8)
        .with_interpolation(interpolation as u8)
        .with_processing(processing.cache_hash());

    let render_time_ms = if !cache.is_valid(&cache_key) {
        let radials = render_sweep.radials();

        let render_result = if processing.enabled {
            // Processing path: build SweepField, apply pipeline, render via render_sweep
            render_with_processing(&radials, product, interpolation, render_size, processing)
        } else {
            // Direct path: render radials without processing
            render_radials_to_image(&radials, product, interpolation, render_size)
        };

        match render_result {
            Ok(result) => {
                cache.update(ctx, cache_key, result.image);
                Some(result.render_time_ms)
            }
            Err(e) => {
                log::error!("Failed to render dynamic sweep: {}", e);
                return None;
            }
        }
    } else {
        None
    };

    // Draw the cached texture
    if let Some(texture) = cache.texture() {
        let range_km = radar_coverage_range_km();

        let km_to_deg = 1.0 / 111.0;
        let lat_correction = radar_lat.to_radians().cos();

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

        let texture_rect = Rect::from_min_max(top_left, bottom_right);
        let clipped_rect = texture_rect.intersect(*rect);

        if clipped_rect.width() > 0.0 && clipped_rect.height() > 0.0 {
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

    render_time_ms
}

/// Apply processing pipeline to radials and render via SweepField.
fn render_with_processing(
    radials: &[&::nexrad::model::data::Radial],
    product: nexrad_render::Product,
    interpolation: nexrad_render::Interpolation,
    dimensions: (usize, usize),
    processing: crate::state::ProcessingConfig,
) -> Result<crate::nexrad::RenderResult, String> {
    use ::nexrad::model::data::SweepField;
    use nexrad_process::filter::{GaussianSmooth, MedianFilter, ThresholdFilter};
    use nexrad_process::SweepPipeline;

    // Clone radials into owned Vec for SweepField construction
    let owned_radials: Vec<::nexrad::model::data::Radial> =
        radials.iter().map(|r| (*r).clone()).collect();

    // Build SweepField from radials
    let field = SweepField::from_radials(&owned_radials, product)
        .ok_or_else(|| "Failed to build SweepField from radials".to_string())?;

    // Build and apply processing pipeline
    let mut pipeline = SweepPipeline::new();

    // Threshold filter
    if processing.threshold_min.is_some() || processing.threshold_max.is_some() {
        pipeline = pipeline.then(ThresholdFilter {
            min: processing.threshold_min,
            max: processing.threshold_max,
        });
    }

    // Smoothing
    match processing.smoothing {
        SmoothingMode::Median => {
            let kernel = processing.smoothing_strength as usize;
            // Ensure odd kernel size
            let kernel = if kernel.is_multiple_of(2) {
                kernel + 1
            } else {
                kernel
            };
            pipeline = pipeline.then(MedianFilter {
                azimuth_kernel: kernel,
                range_kernel: kernel,
            });
        }
        SmoothingMode::Gaussian => {
            let sigma = processing.smoothing_strength as f32 * 0.5;
            let sigma = sigma.max(0.5);
            pipeline = pipeline.then(GaussianSmooth {
                sigma_azimuth: sigma,
                sigma_range: sigma,
            });
        }
        SmoothingMode::None => {}
    }

    // Execute pipeline
    let processed = pipeline
        .execute(&field)
        .map_err(|e| format!("Processing pipeline failed: {}", e))?;

    // Render the processed field
    render_sweep_field_to_image(&processed, product, interpolation, dimensions)
}

/// Render data age timestamp markers at cardinal azimuth positions (N, E, S, W).
fn render_age_markers(
    painter: &Painter,
    rect: &Rect,
    state: &AppState,
    render_sweep: &RenderSweep,
) {
    let center = rect.center() + state.viz_state.pan_offset;
    let base_radius = rect.width().min(rect.height()) * 0.4;
    let radius = base_radius * state.viz_state.zoom;
    let dark = state.is_dark;

    // Place labels at ~75% of radius to avoid overlapping with range rings/cardinal labels
    let label_radius = radius * 0.75;
    let font_id = egui::FontId::proportional(11.0);
    let playback_ts_ms = (state.playback_state.playback_position() * 1000.0) as i64;

    // Cardinal azimuths: N=0, E=90, S=180, W=270
    let azimuths = [
        (0.0_f32, egui::Align2::CENTER_BOTTOM),
        (90.0, egui::Align2::LEFT_CENTER),
        (180.0, egui::Align2::CENTER_TOP),
        (270.0, egui::Align2::RIGHT_CENTER),
    ];

    for (az, align) in &azimuths {
        if let Some(radial_time_ms) = render_sweep.radial_time_at_azimuth(*az) {
            let age_secs = ((playback_ts_ms - radial_time_ms) as f64 / 1000.0).max(0.0);
            let label = format_age(age_secs);

            // Convert azimuth to screen position (0=N, clockwise)
            let angle_rad = (*az - 90.0) * PI / 180.0;
            let pos = Pos2::new(
                center.x + label_radius * angle_rad.cos(),
                center.y + label_radius * angle_rad.sin(),
            );

            let text_color = age_color(age_secs);
            let bg_color = if dark {
                Color32::from_rgba_unmultiplied(20, 20, 35, 180)
            } else {
                Color32::from_rgba_unmultiplied(240, 240, 245, 200)
            };

            // Draw background rect behind text for readability
            let galley = painter.layout_no_wrap(label.clone(), font_id.clone(), text_color);
            let text_rect = align.anchor_size(pos, galley.size());
            let padded = text_rect.expand(2.0);
            painter.rect_filled(padded, 2.0, bg_color);

            painter.text(pos, *align, label, font_id.clone(), text_color);
        }
    }
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
            let new_zoom = (old_zoom * zoom_factor).clamp(0.1, 10.0);

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

/// Render the radar sweep visualization
fn render_radar_sweep(painter: &Painter, rect: &Rect, state: &AppState, azimuth: Option<f32>) {
    let center = rect.center() + state.viz_state.pan_offset;
    let base_radius = rect.width().min(rect.height()) * 0.4;
    let radius = base_radius * state.viz_state.zoom;
    let dark = state.is_dark;

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
