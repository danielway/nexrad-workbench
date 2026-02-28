//! Right panel UI: product selection, layers, and rendering controls.

use crate::state::{
    format_bytes, get_vcp_definition, AppState, ColorPalette, InterpolationMode, RadarProduct,
    RenderMode, SmoothingMode, StorageSettings, ThemeMode,
};
use eframe::egui::{self, RichText, ScrollArea};

pub fn render_right_panel(ctx: &egui::Context, state: &mut AppState) {
    if !state.right_sidebar_visible {
        return;
    }

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

                render_rendering_section(ui, state);
                ui.add_space(5.0);

                render_processing_section(ui, state);
                ui.add_space(5.0);

                render_palette_section(ui, state);
                ui.add_space(5.0);

                render_layers_section(ui, state);
                ui.add_space(5.0);

                render_appearance_section(ui, state);
                ui.add_space(5.0);

                render_storage_section(ui, state);
            });
        });
}

fn render_product_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Product").strong())
        .default_open(true)
        .show(ui, |ui| {
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

            if matches!(state.viz_state.render_mode, RenderMode::FixedTilt) {
                ui.add_space(8.0);
                ui.label("Target Elevation:");

                let playback_ts = state.playback_state.playback_position();
                let current_scan = state.radar_timeline.find_scan_at_timestamp(playback_ts);

                if let Some(scan) = current_scan {
                    if let Some(vcp_def) = get_vcp_definition(scan.vcp) {
                        egui::ComboBox::from_id_salt("elevation_selector")
                            .selected_text(format!(
                                "{:.1}\u{00B0}",
                                state.viz_state.target_elevation
                            ))
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for elev in vcp_def.elevations {
                                    let is_selected =
                                        (state.viz_state.target_elevation - elev.angle).abs() < 0.1;
                                    if ui
                                        .selectable_label(
                                            is_selected,
                                            format!("{:.1}\u{00B0}", elev.angle),
                                        )
                                        .clicked()
                                    {
                                        state.viz_state.target_elevation = elev.angle;
                                    }
                                }
                            });
                    } else {
                        ui.add(
                            egui::Slider::new(&mut state.viz_state.target_elevation, 0.5..=19.5)
                                .suffix("\u{00B0}")
                                .step_by(0.5),
                        );
                    }
                } else {
                    ui.add(
                        egui::Slider::new(&mut state.viz_state.target_elevation, 0.5..=19.5)
                            .suffix("\u{00B0}")
                            .step_by(0.5),
                    );
                }
            }
        });
}

fn render_rendering_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Rendering").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.label("Interpolation:");
            egui::ComboBox::from_id_salt("interpolation_selector")
                .selected_text(state.viz_state.interpolation.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for mode in InterpolationMode::all() {
                        ui.selectable_value(
                            &mut state.viz_state.interpolation,
                            *mode,
                            mode.label(),
                        );
                    }
                });
            ui.label(
                RichText::new(match state.viz_state.interpolation {
                    InterpolationMode::Nearest => "Fast, produces blocky output",
                    InterpolationMode::Bilinear => "Smooth, anti-aliased output",
                })
                .small()
                .weak(),
            );
        });
}

fn render_processing_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Processing").strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.checkbox(&mut state.viz_state.processing.enabled, "Enable Processing");

            if state.viz_state.processing.enabled {
                ui.add_space(4.0);

                // Threshold filter
                ui.label("Threshold Filter:");

                let mut use_min = state.viz_state.processing.threshold_min.is_some();
                let mut min_val = state.viz_state.processing.threshold_min.unwrap_or(5.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut use_min, "Min:");
                    ui.add_enabled(
                        use_min,
                        egui::DragValue::new(&mut min_val).speed(0.5).suffix(" dBZ"),
                    );
                });
                state.viz_state.processing.threshold_min =
                    if use_min { Some(min_val) } else { None };

                let mut use_max = state.viz_state.processing.threshold_max.is_some();
                let mut max_val = state.viz_state.processing.threshold_max.unwrap_or(75.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut use_max, "Max:");
                    ui.add_enabled(
                        use_max,
                        egui::DragValue::new(&mut max_val).speed(0.5).suffix(" dBZ"),
                    );
                });
                state.viz_state.processing.threshold_max =
                    if use_max { Some(max_val) } else { None };

                ui.add_space(4.0);

                // Smoothing
                ui.label("Smoothing:");
                egui::ComboBox::from_id_salt("smoothing_selector")
                    .selected_text(state.viz_state.processing.smoothing.label())
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for mode in SmoothingMode::all() {
                            ui.selectable_value(
                                &mut state.viz_state.processing.smoothing,
                                *mode,
                                mode.label(),
                            );
                        }
                    });

                if state.viz_state.processing.smoothing != SmoothingMode::None {
                    let label = match state.viz_state.processing.smoothing {
                        SmoothingMode::Median => "Kernel Size:",
                        SmoothingMode::Gaussian => "Strength:",
                        SmoothingMode::None => unreachable!(),
                    };
                    ui.label(label);
                    ui.add(
                        egui::Slider::new(
                            &mut state.viz_state.processing.smoothing_strength,
                            1..=9,
                        )
                        .step_by(1.0),
                    );
                }
            } else {
                ui.label(
                    RichText::new("Filters and smoothing for data cleanup")
                        .small()
                        .weak(),
                );
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
            ui.checkbox(&mut state.layer_state.geo.nexrad_sites, "NEXRAD Sites");
            ui.checkbox(&mut state.layer_state.geo.states, "State Lines");
            ui.checkbox(&mut state.layer_state.geo.counties, "County Lines");
            ui.checkbox(&mut state.layer_state.geo.labels, "Labels");
        });
}

fn render_appearance_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Appearance").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.label("Theme:");
            let prev_mode = state.theme_mode;
            egui::ComboBox::from_id_salt("theme_selector")
                .selected_text(state.theme_mode.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for mode in ThemeMode::all() {
                        ui.selectable_value(&mut state.theme_mode, *mode, mode.label());
                    }
                });
            if state.theme_mode != prev_mode {
                crate::state::theme::save_theme_mode(state.theme_mode);
            }
        });
}

fn render_storage_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Storage").strong())
        .default_open(true)
        .show(ui, |ui| {
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

            let bar_width = ui.available_width();
            let (response, painter) =
                ui.allocate_painter(egui::Vec2::new(bar_width, 8.0), egui::Sense::hover());
            let rect = response.rect;

            painter.rect_filled(rect, 2.0, egui::Color32::from_gray(40));

            let fill_width = (rect.width() * usage_pct as f32 / 100.0).max(0.0);
            let fill_color = if usage_pct > 90.0 {
                egui::Color32::from_rgb(220, 80, 80)
            } else if usage_pct > 70.0 {
                egui::Color32::from_rgb(220, 180, 80)
            } else {
                egui::Color32::from_rgb(80, 160, 80)
            };
            let fill_rect =
                egui::Rect::from_min_size(rect.min, egui::Vec2::new(fill_width, rect.height()));
            painter.rect_filled(fill_rect, 2.0, fill_color);

            ui.add_space(8.0);

            ui.label("Storage Quota:");
            let min_quota_mb = (StorageSettings::min_quota() / (1024 * 1024)) as f32;
            let max_quota_mb = (StorageSettings::max_quota() / (1024 * 1024)) as f32;
            let mut quota_mb = (state.storage_settings.quota_bytes / (1024 * 1024)) as f32;

            let slider = egui::Slider::new(&mut quota_mb, min_quota_mb..=max_quota_mb)
                .suffix(" MB")
                .logarithmic(true)
                .clamping(egui::SliderClamping::Always);

            if ui.add(slider).changed() {
                state
                    .storage_settings
                    .set_quota((quota_mb as u64) * 1024 * 1024);
                state.storage_settings.save();
            }

            ui.add_space(8.0);

            if ui
                .button("Clear Cache")
                .on_hover_text("Delete all cached radar data")
                .clicked()
            {
                state.clear_cache_requested = true;
            }
        });
}
