//! Left panel UI: file upload controls and radar operations visualization.

use crate::file_ops::FilePickerChannel;
use crate::state::{get_vcp_definition, radar_data::Scan, AppState};
use eframe::egui::{self, Color32, Pos2, RichText, Stroke, Vec2};
use std::f32::consts::PI;

/// State queried from the radar timeline at the current timestamp
struct RadarStateAtTimestamp<'a> {
    /// Current azimuth angle in degrees (0-360)
    azimuth: Option<f32>,
    /// Current elevation angle in degrees
    elevation: Option<f32>,
    /// Current VCP number
    vcp: Option<u16>,
    /// Index of the current sweep within the scan
    sweep_index: Option<usize>,
    /// Scan progress as a percentage (0.0-1.0)
    scan_progress: Option<f32>,
    /// Reference to the current scan (for elevation list)
    scan: Option<&'a Scan>,
}

pub fn render_left_panel(
    ctx: &egui::Context,
    state: &mut AppState,
    file_picker: &FilePickerChannel,
) {
    egui::SidePanel::left("left_panel")
        .resizable(true)
        .default_width(250.0)
        .min_width(200.0)
        .max_width(400.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                render_load_data_section(ui, state, file_picker);
                ui.add_space(10.0);
                render_radar_operations_section(ui, state);
            });
        });
}

fn render_load_data_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    file_picker: &FilePickerChannel,
) {
    ui.heading("Load Data");
    ui.separator();

    let is_loading = state.upload_state.loading;

    ui.add_enabled_ui(!is_loading, |ui| {
        if ui.button("Choose file...").clicked() {
            state.upload_state.loading = true;
            state.status_message = "Opening file dialog...".to_string();
            file_picker.pick_file(ui.ctx().clone());
        }
    });

    if is_loading {
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Selecting file...");
        });
    }

    ui.add_space(10.0);

    if let Some(ref name) = state.upload_state.file_name {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("\u{2713}").color(egui::Color32::from_rgb(100, 200, 100)),
                );
                ui.label(RichText::new("File loaded").small());
            });
            ui.label(RichText::new(name).strong().monospace());

            if let Some(size) = state.upload_state.file_size {
                ui.label(format_file_size(size));
            }

            if state.upload_state.file_data.is_some() {
                ui.label(
                    RichText::new("Ready for processing")
                        .small()
                        .color(egui::Color32::from_rgb(100, 200, 100)),
                );
            }
        });
    }
}

fn render_radar_operations_section(ui: &mut egui::Ui, state: &AppState) {
    ui.collapsing("Radar Operations", |ui| {
        let radar_state = query_radar_state_at_timestamp(state);

        ui.add_space(5.0);

        // Top-down view (azimuth)
        ui.label(RichText::new("Top View (Azimuth)").strong());
        render_top_down_view(ui, radar_state.azimuth);

        ui.add_space(10.0);

        // Side view (elevation)
        ui.label(RichText::new("Side View (Elevation)").strong());
        render_side_view(ui, radar_state.elevation);

        ui.add_space(10.0);
        ui.separator();

        // VCP breakdown
        render_vcp_breakdown(ui, &radar_state);
    });
}

fn query_radar_state_at_timestamp(state: &AppState) -> RadarStateAtTimestamp<'_> {
    let ts = match state.playback_state.selected_timestamp {
        Some(ts) => ts,
        None => {
            return RadarStateAtTimestamp {
                azimuth: None,
                elevation: None,
                vcp: None,
                sweep_index: None,
                scan_progress: None,
                scan: None,
            }
        }
    };

    // Find the scan at the current timestamp
    let scan = state.radar_timeline.find_scan_at_timestamp(ts);

    match scan {
        Some(scan) => {
            let scan_progress = scan.progress_at_timestamp(ts);
            let sweep_data = scan.find_sweep_at_timestamp(ts);

            let (sweep_index, azimuth, elevation) = match sweep_data {
                Some((idx, sweep)) => {
                    let az = sweep.interpolate_azimuth(ts);
                    (Some(idx), az, Some(sweep.elevation))
                }
                None => (None, None, None),
            };

            RadarStateAtTimestamp {
                azimuth,
                elevation,
                vcp: Some(scan.vcp),
                sweep_index,
                scan_progress,
                scan: Some(scan),
            }
        }
        None => RadarStateAtTimestamp {
            azimuth: None,
            elevation: None,
            vcp: None,
            sweep_index: None,
            scan_progress: None,
            scan: None,
        },
    }
}

fn render_top_down_view(ui: &mut egui::Ui, azimuth: Option<f32>) {
    let size = Vec2::new(120.0, 120.0);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = response.rect;
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) / 2.0) - 10.0;

    // Dark background
    painter.rect_filled(rect, 4.0, Color32::from_rgb(30, 30, 40));

    // Concentric range rings
    let ring_color = Color32::from_rgb(60, 60, 80);
    for factor in [0.25, 0.5, 0.75, 1.0] {
        painter.circle_stroke(
            center,
            radius * factor,
            Stroke::new(1.0, ring_color),
        );
    }

    // Cardinal direction labels
    let label_color = Color32::from_rgb(150, 150, 170);
    let label_offset = radius + 6.0;
    let font_id = egui::FontId::proportional(10.0);

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

    // Center dot (radar dish)
    painter.circle_filled(center, 3.0, Color32::from_rgb(200, 200, 200));

    // Azimuth line (if we have data)
    if let Some(az) = azimuth {
        // Convert azimuth to radians (0 = North, clockwise)
        // In screen coordinates: 0 degrees should point up (negative Y)
        let angle_rad = (az - 90.0) * PI / 180.0;
        let end_x = center.x + radius * angle_rad.cos();
        let end_y = center.y + radius * angle_rad.sin();

        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(2.0, Color32::from_rgb(100, 255, 100)),
        );

        // Azimuth label below
        ui.label(format!("Az: {:.1}\u{00B0}", az));
    } else {
        ui.label(RichText::new("No scan data").small().color(Color32::GRAY));
    }
}

fn render_side_view(ui: &mut egui::Ui, elevation: Option<f32>) {
    let size = Vec2::new(140.0, 70.0);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = response.rect;

    // Dark background
    painter.rect_filled(rect, 4.0, Color32::from_rgb(30, 30, 40));

    // Ground line at bottom
    let ground_y = rect.bottom() - 10.0;
    let ground_color = Color32::from_rgb(80, 60, 40);
    painter.line_segment(
        [Pos2::new(rect.left() + 5.0, ground_y), Pos2::new(rect.right() - 5.0, ground_y)],
        Stroke::new(2.0, ground_color),
    );

    // Tower/dish on left side
    let tower_x = rect.left() + 20.0;
    let tower_bottom = ground_y;
    let tower_top = tower_bottom - 15.0;

    // Tower base
    painter.line_segment(
        [Pos2::new(tower_x, tower_bottom), Pos2::new(tower_x, tower_top)],
        Stroke::new(3.0, Color32::from_rgb(150, 150, 150)),
    );

    // Dish (small circle at top of tower)
    painter.circle_filled(Pos2::new(tower_x, tower_top), 4.0, Color32::from_rgb(200, 200, 200));

    // Reference angle lines (0°, 10°, 20°)
    let beam_origin = Pos2::new(tower_x, tower_top);
    let beam_length = rect.width() - 40.0;
    let ref_line_color = Color32::from_rgb(60, 60, 80);
    let label_color = Color32::from_rgb(100, 100, 120);
    let font_id = egui::FontId::proportional(9.0);

    for angle in [0.0_f32, 10.0, 20.0] {
        let angle_rad = angle * PI / 180.0;
        let end_x = beam_origin.x + beam_length * angle_rad.cos();
        let end_y = beam_origin.y - beam_length * angle_rad.sin();

        painter.line_segment(
            [beam_origin, Pos2::new(end_x, end_y)],
            Stroke::new(1.0, ref_line_color),
        );

        // Angle label at end of line
        painter.text(
            Pos2::new(end_x + 2.0, end_y),
            egui::Align2::LEFT_CENTER,
            format!("{:.0}\u{00B0}", angle),
            font_id.clone(),
            label_color,
        );
    }

    // Current elevation beam (if we have data)
    if let Some(elev) = elevation {
        // Clamp elevation for display (max ~25 degrees fits in view)
        let display_elev = elev.min(25.0);
        let angle_rad = display_elev * PI / 180.0;
        let end_x = beam_origin.x + beam_length * angle_rad.cos();
        let end_y = beam_origin.y - beam_length * angle_rad.sin();

        painter.line_segment(
            [beam_origin, Pos2::new(end_x, end_y)],
            Stroke::new(2.5, Color32::from_rgb(100, 255, 100)),
        );

        // Elevation label below
        ui.label(format!("Elev: {:.1}\u{00B0}", elev));
    } else {
        ui.label(RichText::new("No scan data").small().color(Color32::GRAY));
    }
}

fn render_vcp_breakdown(ui: &mut egui::Ui, radar_state: &RadarStateAtTimestamp) {
    match radar_state.vcp {
        Some(vcp) => {
            // VCP header
            if let Some(def) = get_vcp_definition(vcp) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("VCP {}", vcp)).strong());
                    ui.label(RichText::new(def.name).small());
                });
                ui.label(RichText::new(def.description).small().color(Color32::GRAY));
            } else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("VCP {}", vcp)).strong());
                    ui.label(RichText::new("(details not available)").small().color(Color32::GRAY));
                });
            }

            ui.add_space(5.0);

            // Progress bar
            if let Some(progress) = radar_state.scan_progress {
                let progress_bar = egui::ProgressBar::new(progress)
                    .show_percentage()
                    .animate(false);
                ui.add(progress_bar);
            }

            ui.add_space(5.0);

            // Elevation list
            ui.label(RichText::new("Elevations").strong());

            // Use the scan's actual sweeps if available, otherwise fall back to VCP definition
            if let Some(scan) = radar_state.scan {
                egui::ScrollArea::vertical()
                    .max_height(150.0)
                    .show(ui, |ui| {
                        for (idx, sweep) in scan.sweeps.iter().enumerate() {
                            let is_current = radar_state.sweep_index == Some(idx);
                            render_elevation_row(
                                ui,
                                idx,
                                sweep.elevation,
                                is_current,
                            );
                        }
                    });
            } else if let Some(def) = get_vcp_definition(vcp) {
                egui::ScrollArea::vertical()
                    .max_height(150.0)
                    .show(ui, |ui| {
                        for (idx, elev) in def.elevations.iter().enumerate() {
                            render_elevation_row_with_details(
                                ui,
                                idx,
                                elev.angle,
                                elev.waveform,
                                elev.prf,
                                false,
                            );
                        }
                    });
            }
        }
        None => {
            ui.label(RichText::new("No scan data at current time").color(Color32::GRAY));
        }
    }
}

fn render_elevation_row(
    ui: &mut egui::Ui,
    _idx: usize,
    elevation: f32,
    is_current: bool,
) {
    let text_color = if is_current {
        Color32::from_rgb(100, 255, 100)
    } else {
        Color32::from_rgb(180, 180, 180)
    };

    ui.horizontal(|ui| {
        if is_current {
            ui.label(RichText::new("\u{25B6}").color(text_color)); // Right-pointing triangle
        } else {
            ui.label(RichText::new("  ").monospace());
        }
        ui.label(RichText::new(format!("{:5.1}\u{00B0}", elevation)).color(text_color).monospace());
    });
}

fn render_elevation_row_with_details(
    ui: &mut egui::Ui,
    _idx: usize,
    elevation: f32,
    waveform: &str,
    prf: &str,
    is_current: bool,
) {
    let text_color = if is_current {
        Color32::from_rgb(100, 255, 100)
    } else {
        Color32::from_rgb(180, 180, 180)
    };

    ui.horizontal(|ui| {
        if is_current {
            ui.label(RichText::new("\u{25B6}").color(text_color));
        } else {
            ui.label(RichText::new("  ").monospace());
        }
        ui.label(RichText::new(format!("{:5.1}\u{00B0}", elevation)).color(text_color).monospace());
        ui.label(RichText::new(format!("{}  {}", waveform, prf)).small().color(Color32::GRAY));
    });
}

fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}
