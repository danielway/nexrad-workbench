//! Central canvas UI: radar visualization area.

use crate::geo::{GeoLayerSet, MapProjection};
use crate::state::AppState;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::f32::consts::PI;

/// Render canvas with optional geographic layers.
pub fn render_canvas_with_geo(
    ctx: &egui::Context,
    state: &mut AppState,
    geo_layers: Option<&GeoLayerSet>,
) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let available_size = ui.available_size();

        // Allocate the full available space for the canvas
        let (response, painter) = ui.allocate_painter(available_size, Sense::click_and_drag());

        let rect = response.rect;

        // Draw background
        painter.rect_filled(rect, 0.0, Color32::from_rgb(20, 20, 35));

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

        // Query current azimuth from radar timeline
        let azimuth = state
            .playback_state
            .selected_timestamp
            .and_then(|ts| state.radar_timeline.find_scan_at_timestamp(ts))
            .and_then(|scan| scan.find_sweep_at_timestamp(state.playback_state.selected_timestamp.unwrap()))
            .and_then(|(_, sweep)| sweep.interpolate_azimuth(state.playback_state.selected_timestamp.unwrap()));

        // Draw the radar sweep visualization
        render_radar_sweep(&painter, &rect, state, azimuth);

        // Draw overlay info in top-left corner
        draw_overlay_info(ui, &rect, state);

        // Handle zoom/pan interactions
        handle_canvas_interaction(&response, &rect, state);
    });
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

    // Range ring colors
    let ring_color = Color32::from_rgba_unmultiplied(60, 80, 60, 120);
    let ring_color_major = Color32::from_rgba_unmultiplied(80, 100, 80, 150);

    // Draw range rings (every 50km nominal, with major rings at 100km)
    let num_rings = 6;
    for i in 1..=num_rings {
        let ring_radius = radius * (i as f32 / num_rings as f32);
        let is_major = i % 2 == 0;
        let color = if is_major { ring_color_major } else { ring_color };
        let width = if is_major { 1.5 } else { 1.0 };
        painter.circle_stroke(center, ring_radius, Stroke::new(width, color));
    }

    // Draw radial lines (every 30 degrees)
    let radial_color = Color32::from_rgba_unmultiplied(50, 70, 50, 80);
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
    let label_color = Color32::from_rgba_unmultiplied(120, 140, 120, 200);
    let label_offset = radius + 15.0;
    let font_id = egui::FontId::proportional(12.0);

    painter.text(
        center + Vec2::new(0.0, -label_offset),
        egui::Align2::CENTER_BOTTOM,
        "N",
        font_id.clone(),
        label_color,
    );
    painter.text(
        center + Vec2::new(label_offset, 0.0),
        egui::Align2::LEFT_CENTER,
        "E",
        font_id.clone(),
        label_color,
    );
    painter.text(
        center + Vec2::new(0.0, label_offset),
        egui::Align2::CENTER_TOP,
        "S",
        font_id.clone(),
        label_color,
    );
    painter.text(
        center + Vec2::new(-label_offset, 0.0),
        egui::Align2::RIGHT_CENTER,
        "W",
        font_id,
        label_color,
    );

    // Draw center marker (radar site)
    painter.circle_filled(center, 4.0, Color32::from_rgb(180, 180, 200));
    painter.circle_stroke(center, 4.0, Stroke::new(1.0, Color32::from_rgb(100, 100, 120)));

    // Draw the sweep line if we have azimuth data
    if let Some(az) = azimuth {
        // Draw a fading "trail" behind the sweep line to show recent coverage
        let trail_length = 30.0; // degrees of trail
        let num_trail_segments = 15;
        for i in 0..num_trail_segments {
            let trail_az = az - (i as f32) * (trail_length / num_trail_segments as f32);
            let alpha = ((num_trail_segments - i) as f32 / num_trail_segments as f32 * 60.0) as u8;
            let trail_color = Color32::from_rgba_unmultiplied(80, 200, 80, alpha);

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
            Stroke::new(3.0, Color32::from_rgb(100, 255, 100)),
        );

        // Glow effect
        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(6.0, Color32::from_rgba_unmultiplied(100, 255, 100, 40)),
        );
    }
}
