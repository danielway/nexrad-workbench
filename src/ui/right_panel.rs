//! Right panel UI: product selection, layers, and rendering controls.

use crate::state::{
    format_bytes, AppState, ElevationSelection, InterpolationMode, RadarProduct, StorageSettings,
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

                render_volume_section(ui, state);
                ui.add_space(5.0);

                render_tools_section(ui, state);
                ui.add_space(5.0);

                render_events_section(ui, state);
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

            // Auto (latest sweep) checkbox
            let mut is_auto = state.viz_state.elevation_selection.is_auto();
            if ui
                .checkbox(&mut is_auto, "Auto (latest sweep)")
                .on_hover_text("Show the most recently completed sweep regardless of elevation")
                .changed()
            {
                if is_auto {
                    // Save current Fixed selection before switching to Latest
                    if let ElevationSelection::Fixed {
                        elevation_number,
                        angle,
                    } = &state.viz_state.elevation_selection
                    {
                        state.viz_state.last_fixed_selection = Some((*elevation_number, *angle));
                    }
                    state.viz_state.elevation_selection = ElevationSelection::Latest;
                } else {
                    // Restore previous Fixed selection
                    let (num, angle) = state.viz_state.last_fixed_selection.unwrap_or((1, 0.5));
                    state.viz_state.elevation_selection = ElevationSelection::Fixed {
                        elevation_number: num,
                        angle,
                    };
                }
            }

            ui.separator();

            // Elevation list
            let entries = state.viz_state.cached_vcp_elevations.clone();
            let list_enabled = !is_auto;
            let selected_product = state.viz_state.product.to_worker_string();

            ui.add_enabled_ui(list_enabled, |ui| {
                if entries.is_empty() {
                    // Pre-data: show default
                    let selected = matches!(
                        state.viz_state.elevation_selection,
                        ElevationSelection::Fixed {
                            elevation_number: 1,
                            ..
                        }
                    );
                    let resp = ui.selectable_label(selected, "1   0.5\u{00B0} (default)");
                    if resp.clicked() {
                        state.viz_state.elevation_selection = ElevationSelection::Fixed {
                            elevation_number: 1,
                            angle: 0.5,
                        };
                    }
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(250.0)
                        .id_salt("elevation_list")
                        .show(ui, |ui| {
                            for entry in &entries {
                                let is_selected = matches!(
                                    &state.viz_state.elevation_selection,
                                    ElevationSelection::Fixed { elevation_number, .. }
                                        if *elevation_number == entry.elevation_number
                                );

                                // Empty available_products means "unknown" — allow.
                                let product_available = entry.available_products.is_empty()
                                    || entry
                                        .available_products
                                        .iter()
                                        .any(|p| p == selected_product);

                                ui.horizontal(|ui| {
                                    // Build the label text
                                    let num_str = format!(
                                        "{:<3} {:.1}\u{00B0}",
                                        entry.elevation_number, entry.angle
                                    );

                                    let resp = ui
                                        .add_enabled(
                                            product_available,
                                            egui::Button::selectable(
                                                is_selected,
                                                RichText::new(&num_str),
                                            ),
                                        )
                                        .on_disabled_hover_text(format!(
                                            "{} not available at this elevation",
                                            state.viz_state.product.label()
                                        ));

                                    // Waveform badge
                                    if !entry.waveform.is_empty() {
                                        let wf_color = match entry.waveform.as_str() {
                                            "CS" => egui::Color32::from_rgb(100, 200, 100),
                                            _ => egui::Color32::from_rgb(100, 150, 255),
                                        };
                                        ui.label(
                                            RichText::new(&entry.waveform).small().color(wf_color),
                                        );
                                    }

                                    // SAILS/MRLE badge
                                    if entry.is_sails {
                                        ui.label(
                                            RichText::new("SAILS")
                                                .small()
                                                .color(egui::Color32::from_rgb(220, 180, 60)),
                                        )
                                        .on_hover_text(
                                            "Supplemental Adaptive Intra-Volume Low-Level Scan — \
                                             CD waveform, lower range resolution than CS base tilt",
                                        );
                                    }
                                    if entry.is_mrle {
                                        ui.label(
                                            RichText::new("MRLE")
                                                .small()
                                                .color(egui::Color32::from_rgb(220, 180, 60)),
                                        )
                                        .on_hover_text("Mid-Volume Rescan of Low-Level Elevations");
                                    }

                                    if resp.clicked() {
                                        state.viz_state.elevation_selection =
                                            ElevationSelection::Fixed {
                                                elevation_number: entry.elevation_number,
                                                angle: entry.angle,
                                            };
                                    }
                                });
                            }
                        });
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
            ui.checkbox(&mut state.layer_state.geo.cities, "Cities");
            ui.checkbox(&mut state.layer_state.geo.labels, "Labels");
        });
}

fn render_rendering_section(ui: &mut egui::Ui, state: &mut AppState) {
    let in_macro = state.playback_state.playback_mode() == crate::state::PlaybackMode::Macro;
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

            // Despeckle
            ui.checkbox(&mut proc.despeckle_enabled, "Despeckle");
            if proc.despeckle_enabled {
                ui.indent("despeckle_indent", |ui| {
                    let mut threshold = proc.despeckle_threshold as i32;
                    if ui
                        .add(egui::Slider::new(&mut threshold, 1..=16).text("Threshold"))
                        .changed()
                    {
                        proc.despeckle_threshold = threshold as u32;
                    }
                });
            }

            ui.add_space(4.0);

            // Sweep animation (disabled in macro mode — not meaningful when
            // playback jumps between complete frames)
            ui.add_enabled_ui(!in_macro, |ui| {
                ui.checkbox(&mut proc.sweep_animation, "Sweep Animation")
                    .on_hover_text(if in_macro {
                        "Sweep animation is disabled at this zoom level (macro playback mode)"
                    } else {
                        "Progressively reveal new data behind the sweep line during playback"
                    });
            });

            // Data age indicator (only meaningful when sweep animation is on)
            ui.add_enabled_ui(proc.sweep_animation && !in_macro, |ui| {
                ui.indent("data_age_indent", |ui| {
                    ui.checkbox(&mut proc.data_age_indicator, "Data Age Indicator")
                        .on_hover_text(
                            "Desaturate the oldest data behind the sweep line to indicate staleness",
                        );
                });
            });

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

fn render_volume_section(ui: &mut egui::Ui, state: &mut AppState) {
    use crate::state::ViewMode;

    // Only show volume controls in 3D mode
    if !matches!(state.viz_state.view_mode, ViewMode::Globe3D) {
        return;
    }

    egui::CollapsingHeader::new(RichText::new("3D Volume").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.checkbox(
                &mut state.viz_state.volume_3d_enabled,
                "Enable Volume Rendering",
            )
            .on_hover_text("Ray-march through all elevation sweeps as a volumetric cloud");

            if state.viz_state.volume_3d_enabled {
                ui.add_space(4.0);
                ui.add(
                    egui::Slider::new(&mut state.viz_state.volume_density_cutoff, 0.0..=30.0)
                        .text("Min Value")
                        .step_by(1.0),
                )
                .on_hover_text("Minimum physical value to render (e.g. dBZ for reflectivity)");
            }
        });
}

fn render_tools_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Tools").strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.checkbox(&mut state.viz_state.inspector_enabled, "Inspector")
                .on_hover_text("Hover over radar to see position and data value");

            let was_active = state.viz_state.distance_tool_active;
            ui.checkbox(
                &mut state.viz_state.distance_tool_active,
                "Distance Measure",
            )
            .on_hover_text("Click two points on the map to measure distance");
            // Clear measurement points when tool is toggled off
            if was_active && !state.viz_state.distance_tool_active {
                state.viz_state.distance_start = None;
                state.viz_state.distance_end = None;
            }

            ui.checkbox(&mut state.viz_state.storm_cells_visible, "Storm Cells")
                .on_hover_text("Detect and display storm cells on the radar");
            if state.viz_state.storm_cells_visible {
                ui.indent("storm_cell_indent", |ui| {
                    let mut threshold = state.viz_state.storm_cell_threshold_dbz;
                    if ui
                        .add(
                            egui::Slider::new(&mut threshold, 20.0..=60.0)
                                .text("Min dBZ")
                                .step_by(1.0),
                        )
                        .changed()
                    {
                        state.viz_state.storm_cell_threshold_dbz = threshold;
                        // Clear cached results so detection re-runs with new threshold
                        state.viz_state.detected_storm_cells.clear();
                    }
                });
            }
        });
}

fn render_events_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::CollapsingHeader::new(RichText::new("Events").strong())
        .default_open(true)
        .show(ui, |ui| {
            // Save current selection as event button
            let has_selection = state.playback_state.selection_range().is_some();
            let btn = ui
                .add_enabled(
                    has_selection,
                    egui::Button::new(format!(
                        "{} Save Selection as Event",
                        egui_phosphor::regular::BOOKMARK_SIMPLE
                    )),
                )
                .on_hover_text("Select a time range on the timeline first (Shift+drag)");
            if btn.clicked() {
                state.event_modal_open = true;
                state.event_modal_editing_id = None;
            }

            // Events for current site
            let current_site = state.viz_state.site_id.clone();
            let site_events: Vec<_> = state
                .saved_events
                .events
                .iter()
                .filter(|e| e.site_id == current_site)
                .cloned()
                .collect();
            let other_count = state.saved_events.events.len() - site_events.len();

            if !site_events.is_empty() {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                for event in &site_events {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&event.name)
                                .strong()
                                .color(event_color(event.id)),
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button(egui_phosphor::regular::PENCIL_SIMPLE)
                                .clicked()
                            {
                                state.event_modal_open = true;
                                state.event_modal_editing_id = Some(event.id);
                            }
                            if ui
                                .small_button(egui_phosphor::regular::NAVIGATION_ARROW)
                                .clicked()
                            {
                                navigate_to_event(state, event);
                            }
                        });
                    });

                    // Time range label
                    let start_label = format_event_time(event.start_time, state.use_local_time);
                    let end_label = format_event_time(event.end_time, state.use_local_time);
                    ui.label(
                        RichText::new(format!("{} - {}", start_label, end_label))
                            .small()
                            .weak(),
                    );
                    ui.add_space(2.0);
                }
            }

            // Other-site events
            if other_count > 0 {
                ui.add_space(4.0);
                egui::CollapsingHeader::new(
                    RichText::new(format!("{} events on other sites", other_count))
                        .small()
                        .weak(),
                )
                .id_salt("other_site_events")
                .default_open(false)
                .show(ui, |ui| {
                    let other_events: Vec<_> = state
                        .saved_events
                        .events
                        .iter()
                        .filter(|e| e.site_id != current_site)
                        .cloned()
                        .collect();
                    for event in &other_events {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} ({})", event.name, event.site_id)));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button(egui_phosphor::regular::PENCIL_SIMPLE)
                                        .clicked()
                                    {
                                        state.event_modal_open = true;
                                        state.event_modal_editing_id = Some(event.id);
                                    }
                                    if ui
                                        .small_button(egui_phosphor::regular::NAVIGATION_ARROW)
                                        .clicked()
                                    {
                                        navigate_to_event(state, event);
                                    }
                                },
                            );
                        });
                    }
                });
            }

            if site_events.is_empty() && other_count == 0 {
                ui.add_space(4.0);
                ui.label(RichText::new("No saved events").small().weak());
            }
        });
}

/// Navigate to a saved event: switch site if needed, set selection, center timeline.
fn navigate_to_event(state: &mut AppState, event: &crate::state::SavedEvent) {
    use crate::data::get_site;

    // Switch site if needed
    if event.site_id != state.viz_state.site_id {
        if let Some(site) = get_site(&event.site_id) {
            state.viz_state.site_id = site.id.to_string();
            state.viz_state.center_lat = site.lat;
            state.viz_state.center_lon = site.lon;
            state.viz_state.pan_offset = egui::Vec2::ZERO;
            state.viz_state.camera.center_on(site.lat, site.lon);
            state.push_command(crate::state::AppCommand::RefreshTimeline {
                auto_position: false,
            });
        }
    }

    // Set selection to event bounds
    state.playback_state.selection_start = Some(event.start_time);
    state.playback_state.selection_end = Some(event.end_time);

    // Center timeline on the event
    let mid = (event.start_time + event.end_time) / 2.0;
    state.playback_state.center_view_on(mid);
}

/// Format a timestamp for display in the events list.
fn format_event_time(ts: f64, use_local: bool) -> String {
    if use_local {
        let d = js_sys::Date::new_0();
        d.set_time(ts * 1000.0);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            d.get_full_year(),
            d.get_month() + 1,
            d.get_date(),
            d.get_hours(),
            d.get_minutes(),
        )
    } else {
        use chrono::{TimeZone, Utc};
        let dt = Utc.timestamp_opt(ts as i64, 0).unwrap();
        dt.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// Get a distinguishing color for an event based on its ID.
fn event_color(id: u64) -> egui::Color32 {
    const PALETTE: &[egui::Color32] = &[
        egui::Color32::from_rgb(255, 200, 80),
        egui::Color32::from_rgb(120, 220, 160),
        egui::Color32::from_rgb(160, 180, 255),
        egui::Color32::from_rgb(255, 150, 150),
        egui::Color32::from_rgb(200, 160, 255),
        egui::Color32::from_rgb(255, 180, 120),
    ];
    PALETTE[(id % PALETTE.len() as u64) as usize]
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
                state.push_command(crate::state::AppCommand::ClearCache);
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
