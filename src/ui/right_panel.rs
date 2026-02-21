//! Right panel UI: layers, visualization, and processing controls.

use crate::state::{
    format_bytes, get_vcp_definition, AppState, ColorPalette, RadarProduct, RenderMode,
    StorageSettings,
};
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
                ui.add_space(5.0);

                render_storage_section(ui, state);
            });
        });
}

fn render_product_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Product").strong())
        .default_open(true)
        .show(ui, |ui| {
            // Product selector
            ui.label("Data Product:");
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

            ui.add_space(8.0);

            // Render mode selector
            ui.label("Render Mode:");
            egui::ComboBox::from_id_salt("render_mode_selector")
                .selected_text(state.viz_state.render_mode.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for mode in RenderMode::all() {
                        ui.selectable_value(&mut state.viz_state.render_mode, *mode, mode.label());
                    }
                });

            ui.label(
                RichText::new(state.viz_state.render_mode.description())
                    .small()
                    .weak(),
            );

            // Elevation selector (only for fixed tilt mode)
            if matches!(state.viz_state.render_mode, RenderMode::FixedTilt) {
                ui.add_space(8.0);
                ui.label("Target Elevation:");

                // Get current scan's VCP to show available elevations
                let playback_ts = state.playback_state.playback_position();
                let current_scan = state.radar_timeline.find_scan_at_timestamp(playback_ts);

                if let Some(scan) = current_scan {
                    if let Some(vcp_def) = get_vcp_definition(scan.vcp) {
                        // Show elevations from VCP definition
                        egui::ComboBox::from_id_salt("elevation_selector")
                            .selected_text(format!("{:.1}°", state.viz_state.target_elevation))
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for elev in vcp_def.elevations {
                                    let is_selected =
                                        (state.viz_state.target_elevation - elev.angle).abs() < 0.1;
                                    if ui
                                        .selectable_label(
                                            is_selected,
                                            format!("{:.1}°", elev.angle),
                                        )
                                        .clicked()
                                    {
                                        state.viz_state.target_elevation = elev.angle;
                                    }
                                }
                            });
                    } else {
                        // Fallback: direct input slider
                        ui.add(
                            egui::Slider::new(&mut state.viz_state.target_elevation, 0.5..=19.5)
                                .suffix("°")
                                .step_by(0.5),
                        );
                    }
                } else {
                    // No scan at timestamp, show slider
                    ui.add(
                        egui::Slider::new(&mut state.viz_state.target_elevation, 0.5..=19.5)
                            .suffix("°")
                            .step_by(0.5),
                    );
                }
            }
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
            ui.checkbox(&mut state.layer_state.geo.nexrad_sites, "NEXRAD Sites");
            ui.checkbox(&mut state.layer_state.geo.states, "State Lines");
            ui.checkbox(&mut state.layer_state.geo.counties, "County Lines");
            ui.checkbox(&mut state.layer_state.geo.labels, "Labels");
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

fn render_storage_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Storage").strong())
        .default_open(false)
        .show(ui, |ui| {
            // Current cache size
            let cache_size = state.session_stats.cache_size_bytes;
            let quota = state.storage_settings.quota_bytes;
            let usage_pct = if quota > 0 {
                (cache_size as f64 / quota as f64 * 100.0).min(100.0)
            } else {
                0.0
            };

            ui.horizontal(|ui| {
                ui.label("Cache:");
                ui.label(
                    RichText::new(format!(
                        "{} / {} ({:.0}%)",
                        format_bytes(cache_size),
                        format_bytes(quota),
                        usage_pct
                    ))
                    .monospace(),
                );
            });

            // Usage bar
            let bar_width = ui.available_width();
            let (response, painter) =
                ui.allocate_painter(egui::Vec2::new(bar_width, 8.0), egui::Sense::hover());
            let rect = response.rect;

            // Background
            painter.rect_filled(rect, 2.0, egui::Color32::from_gray(40));

            // Fill based on usage
            let fill_width = (rect.width() * usage_pct as f32 / 100.0).max(0.0);
            let fill_color = if usage_pct > 90.0 {
                egui::Color32::from_rgb(220, 80, 80) // Red when nearly full
            } else if usage_pct > 70.0 {
                egui::Color32::from_rgb(220, 180, 80) // Amber when getting full
            } else {
                egui::Color32::from_rgb(80, 160, 80) // Green otherwise
            };
            let fill_rect =
                egui::Rect::from_min_size(rect.min, egui::Vec2::new(fill_width, rect.height()));
            painter.rect_filled(fill_rect, 2.0, fill_color);

            ui.add_space(8.0);

            // Quota slider
            ui.label("Storage Quota:");
            let min_quota_mb = (StorageSettings::min_quota() / (1024 * 1024)) as f32;
            let max_quota_mb = (StorageSettings::max_quota() / (1024 * 1024)) as f32;
            let mut quota_mb = (state.storage_settings.quota_bytes / (1024 * 1024)) as f32;

            let slider = egui::Slider::new(&mut quota_mb, min_quota_mb..=max_quota_mb)
                .suffix(" MB")
                .logarithmic(true)
                .clamp_to_range(true);

            if ui.add(slider).changed() {
                state
                    .storage_settings
                    .set_quota((quota_mb as u64) * 1024 * 1024);
                state.storage_settings.save();
            }

            ui.add_space(8.0);

            // Clear cache button
            if ui
                .button("Clear Cache")
                .on_hover_text("Delete all cached radar data")
                .clicked()
            {
                state.clear_cache_requested = true;
            }
        });
}
