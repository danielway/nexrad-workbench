//! Right panel UI: layers, visualization, and processing controls.

use crate::state::{AppState, ColorPalette, RadarProduct};
use eframe::egui::{self, RichText, ScrollArea};

pub fn render_right_panel(ctx: &egui::Context, state: &mut AppState) {
    egui::SidePanel::right("right_panel")
        .resizable(true)
        .default_width(220.0)
        .min_width(180.0)
        .max_width(350.0)
        .show(ctx, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Controls");
                ui.separator();

                render_product_section(ui, state);
                ui.add_space(5.0);

                render_palette_section(ui, state);
                ui.add_space(5.0);

                render_layers_section(ui, state);
                ui.add_space(5.0);

                render_processing_section(ui, state);
                ui.add_space(5.0);

                render_3d_section(ui, state);
            });
        });
}

fn render_product_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Product").strong())
        .default_open(true)
        .show(ui, |ui| {
            egui::ComboBox::from_id_salt("product_selector")
                .selected_text(state.viz_state.product.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for product in RadarProduct::all() {
                        ui.selectable_value(
                            &mut state.viz_state.product,
                            *product,
                            product.label(),
                        );
                    }
                });
        });
}

fn render_palette_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Palette").strong())
        .default_open(true)
        .show(ui, |ui| {
            egui::ComboBox::from_id_salt("palette_selector")
                .selected_text(state.viz_state.palette.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for palette in ColorPalette::all() {
                        ui.selectable_value(
                            &mut state.viz_state.palette,
                            *palette,
                            palette.label(),
                        );
                    }
                });
        });
}

fn render_layers_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Layers").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.checkbox(&mut state.layer_state.nws_alerts, "NWS Alerts");
            ui.checkbox(&mut state.layer_state.tornado_tracks, "Tornado Tracks");
            ui.checkbox(
                &mut state.layer_state.political_boundaries,
                "Political Boundaries",
            );
            ui.checkbox(&mut state.layer_state.terrain, "Terrain");

            ui.separator();
            ui.label(RichText::new("Map Overlays").small());

            ui.checkbox(&mut state.layer_state.geo.states, "State Lines");
            ui.checkbox(&mut state.layer_state.geo.counties, "County Lines");
        });
}

fn render_processing_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Processing").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.checkbox(&mut state.processing_state.smoothing_enabled, "Smoothing");

            if state.processing_state.smoothing_enabled {
                ui.indent("smoothing_indent", |ui| {
                    ui.add(
                        egui::Slider::new(
                            &mut state.processing_state.smoothing_strength,
                            0.0..=1.0,
                        )
                        .text("Strength"),
                    );
                });
            }

            ui.checkbox(
                &mut state.processing_state.dealiasing_enabled,
                "Velocity Dealiasing",
            );

            if state.processing_state.dealiasing_enabled {
                ui.indent("dealiasing_indent", |ui| {
                    ui.add(
                        egui::Slider::new(
                            &mut state.processing_state.dealiasing_strength,
                            0.0..=1.0,
                        )
                        .text("Strength"),
                    );
                });
            }
        });
}

fn render_3d_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("3D / Volumetric").strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut state.layer_state.globe_mode, "Globe Mode");
            ui.checkbox(
                &mut state.layer_state.multi_radar_mosaic,
                "Multi-radar Mosaic",
            );
        });
}
