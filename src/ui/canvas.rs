//! Central canvas UI: radar visualization area.

use crate::geo::{GeoLayerSet, MapProjection};
use crate::state::AppState;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Vec2};

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
        let mut projection = MapProjection::new(state.viz_state.center_lat, state.viz_state.center_lon);
        projection.update(state.viz_state.zoom, state.viz_state.pan_offset, rect);

        // Draw geographic layers BEFORE radar (so radar appears on top)
        if let Some(layers) = geo_layers {
            // Create a filtered layer set based on visibility settings
            let filtered = filter_geo_layers(layers, &state.layer_state.geo);
            crate::geo::render_geo_layers(&painter, &filtered, &projection, state.viz_state.zoom);
        }

        // Draw the radar texture if available
        if let Some(ref texture) = state.viz_state.texture {
            let tex_size = texture.size_vec2();
            let scale = (rect.width() / tex_size.x).min(rect.height() / tex_size.y);
            let scaled_size = tex_size * scale * state.viz_state.zoom;

            let center = rect.center() + state.viz_state.pan_offset;
            let tex_rect = Rect::from_center_size(center, scaled_size);

            painter.image(
                texture.id(),
                tex_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            // Draw placeholder text when no texture
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No radar data loaded",
                egui::FontId::proportional(20.0),
                Color32::from_rgb(100, 100, 120),
            );
        }

        // Draw overlay info in top-left corner
        draw_overlay_info(ui, &rect, state);

        // Handle zoom/pan interactions (store state but don't fully implement)
        handle_canvas_interaction(&response, state);
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

fn handle_canvas_interaction(response: &egui::Response, state: &mut AppState) {
    // Handle dragging for panning
    if response.dragged() {
        state.viz_state.pan_offset += response.drag_delta();
    }

    // Handle scroll for zooming (placeholder - stores state)
    if response.hovered() {
        let scroll_delta = response.ctx.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = 1.0 + scroll_delta.y * 0.001;
            state.viz_state.zoom = (state.viz_state.zoom * zoom_factor).clamp(0.1, 10.0);
        }
    }

    // Reset view on double-click
    if response.double_clicked() {
        state.viz_state.zoom = 1.0;
        state.viz_state.pan_offset = Vec2::ZERO;
    }
}
