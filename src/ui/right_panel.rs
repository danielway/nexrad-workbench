//! Right panel UI: product selection, layers, and rendering controls.

use crate::state::{
    format_bytes, get_vcp_definition, AppState, InterpolationMode, RadarProduct, RenderMode,
    StorageSettings,
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

                render_layers_section(ui, state);
                ui.add_space(5.0);

                render_rendering_section(ui, state);
                ui.add_space(5.0);

                render_tools_section(ui, state);
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

                // Collect elevation angles: prefer extracted VCP, then static, then slider
                let elevation_angles: Option<Vec<f32>> = current_scan.and_then(|scan| {
                    // First try extracted VCP pattern
                    if let Some(ref pattern) = scan.vcp_pattern {
                        if !pattern.elevations.is_empty() {
                            return Some(pattern.elevations.iter().map(|e| e.angle).collect());
                        }
                    }
                    // Fall back to static VCP definition
                    get_vcp_definition(scan.vcp)
                        .map(|def| def.elevations.iter().map(|e| e.angle).collect())
                });

                // Slider for elevation selection; snaps to known VCP
                // elevation angles when available.
                let max_elev = elevation_angles
                    .as_ref()
                    .and_then(|a| a.last().copied())
                    .unwrap_or(19.5)
                    .max(19.5);

                let slider =
                    egui::Slider::new(&mut state.viz_state.target_elevation, 0.5..=max_elev)
                        .suffix("\u{00B0}")
                        .step_by(0.1);

                let resp = ui.add(slider);

                // Snap to nearest VCP elevation on release
                if resp.drag_stopped() || resp.lost_focus() {
                    if let Some(ref angles) = elevation_angles {
                        let current = state.viz_state.target_elevation;
                        if let Some(closest) = angles.iter().min_by(|a, b| {
                            ((**a - current).abs())
                                .partial_cmp(&((**b - current).abs()))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        }) {
                            state.viz_state.target_elevation = *closest;
                        }
                    }
                }
            }
        });
}

fn render_layers_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Layers").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.checkbox(&mut state.layer_state.geo.nexrad_sites, "NEXRAD Sites");
            ui.checkbox(&mut state.layer_state.geo.states, "State Lines");
            ui.checkbox(&mut state.layer_state.geo.counties, "County Lines");
            ui.checkbox(&mut state.layer_state.geo.cities, "Cities");
            ui.checkbox(&mut state.layer_state.geo.labels, "Labels");
        });
}

fn render_rendering_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Rendering").strong())
        .default_open(true)
        .show(ui, |ui| {
            let proc = &mut state.render_processing;

            // Interpolation mode
            ui.label("Interpolation:");
            egui::ComboBox::from_id_salt("interpolation_selector")
                .selected_text(proc.interpolation.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for mode in InterpolationMode::all() {
                        ui.selectable_value(&mut proc.interpolation, *mode, mode.label());
                    }
                });

            ui.add_space(8.0);

            // Smoothing
            ui.checkbox(&mut proc.smoothing_enabled, "Smoothing");
            if proc.smoothing_enabled {
                ui.indent("smoothing_indent", |ui| {
                    ui.add(
                        egui::Slider::new(&mut proc.smoothing_radius, 1.0..=10.0)
                            .text("Radius")
                            .step_by(0.5),
                    );
                });
            }

            ui.add_space(4.0);

            // Despeckle
            ui.checkbox(&mut proc.despeckle_enabled, "Despeckle");
            if proc.despeckle_enabled {
                ui.indent("despeckle_indent", |ui| {
                    let mut threshold = proc.despeckle_threshold as i32;
                    if ui
                        .add(egui::Slider::new(&mut threshold, 1..=8).text("Threshold"))
                        .changed()
                    {
                        proc.despeckle_threshold = threshold as u32;
                    }
                });
            }

            ui.add_space(4.0);

            // Edge softening
            ui.checkbox(&mut proc.edge_softening, "Edge Softening");

            ui.add_space(4.0);

            // Opacity
            let mut opacity_pct = proc.opacity * 100.0;
            if ui
                .add(
                    egui::Slider::new(&mut opacity_pct, 0.0..=100.0)
                        .text("Opacity")
                        .suffix("%")
                        .step_by(1.0),
                )
                .changed()
            {
                proc.opacity = opacity_pct / 100.0;
            }
        });
}

fn render_tools_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Tools").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.checkbox(&mut state.inspector_enabled, "Inspector")
                .on_hover_text("Hover over radar to see position and data value");

            let was_active = state.distance_tool_active;
            ui.checkbox(&mut state.distance_tool_active, "Distance Measure")
                .on_hover_text("Click two points on the map to measure distance");
            // Clear measurement points when tool is toggled off
            if was_active && !state.distance_tool_active {
                state.distance_start = None;
                state.distance_end = None;
            }

            ui.checkbox(&mut state.storm_cells_visible, "Storm Cells")
                .on_hover_text("Detect and display storm cells on the radar");
            if state.storm_cells_visible {
                ui.indent("storm_cell_indent", |ui| {
                    let mut threshold = state.storm_cell_threshold_dbz;
                    if ui
                        .add(
                            egui::Slider::new(&mut threshold, 20.0..=60.0)
                                .text("Min dBZ")
                                .step_by(1.0),
                        )
                        .changed()
                    {
                        state.storm_cell_threshold_dbz = threshold;
                        // Clear cached results so detection re-runs with new threshold
                        state.detected_storm_cells.clear();
                    }
                });
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

            ui.add_space(4.0);

            if ui
                .button("Reset App")
                .on_hover_text("Wipe all data and settings, then reload")
                .clicked()
            {
                state.wipe_modal_open = true;
            }
        });
}
