//! Left panel UI: file upload controls and radar operations visualization.

use crate::data::{all_sites_sorted, get_site};
use crate::file_ops::FilePickerChannel;
use crate::nexrad::{DownloadChannel, NexradCache};
use crate::state::{get_vcp_definition, radar_data::Scan, AppState};
use chrono::Datelike;
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
    download_channel: &DownloadChannel,
    nexrad_cache: &NexradCache,
) {
    egui::SidePanel::left("left_panel")
        .resizable(true)
        .default_width(235.0)
        .min_width(235.0)
        .max_width(400.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                render_radar_operations_section(ui, state);
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);
                render_aws_archive_section(ui, ctx, state, download_channel, nexrad_cache);
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);
                render_load_data_section(ui, state, file_picker);
            });
        });
}

fn render_aws_archive_section(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    state: &mut AppState,
    download_channel: &DownloadChannel,
    nexrad_cache: &NexradCache,
) {
    ui.heading("AWS Archive");
    ui.separator();

    // Initialize date to today if not set
    let today = chrono::Utc::now().date_naive();
    let selected_date = state.archive_date.unwrap_or(today);

    // Date input fields
    ui.horizontal(|ui| {
        ui.label("Date:");

        // Year input
        let mut year = selected_date.year();
        ui.add(
            egui::DragValue::new(&mut year)
                .range(1991..=today.year())
                .prefix(""),
        );
        ui.label("-");

        // Month input
        let mut month = selected_date.month();
        ui.add(egui::DragValue::new(&mut month).range(1..=12).prefix(""));
        ui.label("-");

        // Day input
        let mut day = selected_date.day();
        ui.add(egui::DragValue::new(&mut day).range(1..=31).prefix(""));

        // Update date if changed
        if let Some(new_date) = chrono::NaiveDate::from_ymd_opt(year, month, day) {
            if new_date != selected_date {
                state.archive_date = Some(new_date);
            }
        }
    });

    ui.add_space(8.0);

    // Download button
    let is_downloading = state.download_in_progress;

    ui.add_enabled_ui(!is_downloading, |ui| {
        if ui.button("Download from AWS").clicked() {
            let date = state.archive_date.unwrap_or(today);
            state.download_in_progress = true;
            state.status_message = format!(
                "Downloading {} data for {}...",
                state.viz_state.site_id, date
            );

            download_channel.download(
                ctx.clone(),
                state.viz_state.site_id.clone(),
                date,
                nexrad_cache.clone(),
            );
        }
    });

    if is_downloading {
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Downloading...");
        });
    }

    ui.add_space(5.0);
    ui.label(
        RichText::new("Downloads archival NEXRAD data from AWS S3")
            .small()
            .color(Color32::GRAY),
    );
}

fn render_load_data_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    file_picker: &FilePickerChannel,
) {
    ui.heading("Load File");
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
                ui.label(RichText::new("\u{2713}").color(egui::Color32::from_rgb(100, 200, 100)));
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

fn render_radar_operations_section(ui: &mut egui::Ui, state: &mut AppState) {
    // Header
    ui.label(RichText::new("Radar Operations").strong().size(14.0));

    ui.add_space(4.0);

    // Site selector dropdown
    ui.horizontal(|ui| {
        ui.label("Site:");

        // Get display text for current selection
        let current_display = get_site(&state.viz_state.site_id)
            .map(|s| s.display_label())
            .unwrap_or_else(|| state.viz_state.site_id.clone());

        egui::ComboBox::from_id_salt("site_selector")
            .selected_text(current_display)
            .width(180.0)
            .height(300.0)
            .show_ui(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                for site in all_sites_sorted() {
                    let is_selected = site.id == state.viz_state.site_id;
                    if ui
                        .selectable_label(is_selected, site.display_label())
                        .clicked()
                    {
                        state.viz_state.site_id = site.id.to_string();
                        state.viz_state.center_lat = site.lat;
                        state.viz_state.center_lon = site.lon;
                        // Reset pan when changing sites
                        state.viz_state.pan_offset = Vec2::ZERO;
                    }
                }
            });
    });

    ui.add_space(5.0);

    let radar_state = query_radar_state_at_timestamp(state);

    // Top-down and side views side-by-side
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new("Azimuth").small());
            render_top_down_view(ui, radar_state.azimuth);
        });
        ui.add_space(5.0);
        ui.vertical(|ui| {
            ui.label(RichText::new("Elevation").small());
            render_side_view(ui, radar_state.elevation);
        });
    });

    ui.add_space(10.0);

    // VCP breakdown
    render_vcp_breakdown(ui, &radar_state);
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
    let size = Vec2::new(100.0, 100.0);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = response.rect;
    let center = rect.center();
    // Leave more room for cardinal labels (12px margin instead of 8)
    let radius = (rect.width().min(rect.height()) / 2.0) - 12.0;

    // Dark background
    painter.rect_filled(rect, 4.0, Color32::from_rgb(30, 30, 40));

    // Concentric range rings
    let ring_color = Color32::from_rgb(60, 60, 80);
    for factor in [0.33, 0.66, 1.0] {
        painter.circle_stroke(center, radius * factor, Stroke::new(1.0, ring_color));
    }

    // Cardinal direction labels (inside the radar circle for cleaner look)
    let label_color = Color32::from_rgb(100, 100, 120);
    let label_offset = radius - 6.0;
    let font_id = egui::FontId::proportional(8.0);

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
    painter.circle_filled(center, 2.5, Color32::from_rgb(200, 200, 200));

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

        ui.label(RichText::new(format!("{:.1}\u{00B0}", az)).small());
    } else {
        ui.label(RichText::new("--").small().color(Color32::GRAY));
    }
}

fn render_side_view(ui: &mut egui::Ui, elevation: Option<f32>) {
    let size = Vec2::new(120.0, 100.0);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = response.rect;

    // Dark background
    painter.rect_filled(rect, 4.0, Color32::from_rgb(30, 30, 40));

    // Ground line at bottom
    let ground_y = rect.bottom() - 8.0;
    let ground_color = Color32::from_rgb(80, 60, 40);
    painter.line_segment(
        [
            Pos2::new(rect.left() + 5.0, ground_y),
            Pos2::new(rect.right() - 5.0, ground_y),
        ],
        Stroke::new(2.0, ground_color),
    );

    // Tower/dish on left side
    let tower_x = rect.left() + 15.0;
    let tower_bottom = ground_y;
    let tower_top = tower_bottom - 20.0;

    // Tower base
    painter.line_segment(
        [
            Pos2::new(tower_x, tower_bottom),
            Pos2::new(tower_x, tower_top),
        ],
        Stroke::new(3.0, Color32::from_rgb(150, 150, 150)),
    );

    // Dish (small circle at top of tower)
    painter.circle_filled(
        Pos2::new(tower_x, tower_top),
        4.0,
        Color32::from_rgb(200, 200, 200),
    );

    // Reference angle lines (0°, 10°, 20°)
    let beam_origin = Pos2::new(tower_x, tower_top);
    let beam_length = rect.width() - 30.0;
    let ref_line_color = Color32::from_rgb(60, 60, 80);
    let label_color = Color32::from_rgb(100, 100, 120);
    let font_id = egui::FontId::proportional(8.0);

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

        ui.label(RichText::new(format!("{:.1}\u{00B0}", elev)).small());
    } else {
        ui.label(RichText::new("--").small().color(Color32::GRAY));
    }
}

fn render_vcp_breakdown(ui: &mut egui::Ui, radar_state: &RadarStateAtTimestamp) {
    match radar_state.vcp {
        Some(vcp) => {
            // VCP header
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("VCP {}", vcp)).strong());
                if let Some(def) = get_vcp_definition(vcp) {
                    ui.label(RichText::new(def.name).small().color(Color32::GRAY));
                }
            });

            // Progress bar
            if let Some(progress) = radar_state.scan_progress {
                ui.add_space(3.0);
                let progress_bar = egui::ProgressBar::new(progress)
                    .show_percentage()
                    .animate(false);
                ui.add(progress_bar);
            }

            ui.add_space(8.0);

            // Elevation list - use full available width
            let available_width = ui.available_width();

            // Elevation list header
            ui.horizontal(|ui| {
                ui.set_min_width(available_width);
                ui.label(RichText::new(" ").monospace().small()); // Spacer for indicator
                ui.label(RichText::new("Elev").small().color(Color32::GRAY));
                ui.add_space(6.0);
                ui.label(RichText::new("Wf").small().color(Color32::GRAY));
                ui.label(RichText::new("PRF").small().color(Color32::GRAY));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Info").small().color(Color32::GRAY));
                });
            });

            ui.add_space(2.0);

            // Use the scan's actual sweeps with VCP metadata if available
            let vcp_def = get_vcp_definition(vcp);
            if let Some(scan) = radar_state.scan {
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        ui.set_min_width(available_width);
                        for (idx, sweep) in scan.sweeps.iter().enumerate() {
                            let is_current = radar_state.sweep_index == Some(idx);
                            // Try to match elevation with VCP definition for metadata
                            let elev_meta = vcp_def.and_then(|def| {
                                def.elevations
                                    .iter()
                                    .find(|e| (e.angle - sweep.elevation).abs() < 0.1)
                            });
                            render_elevation_row(
                                ui,
                                sweep.elevation,
                                elev_meta,
                                is_current,
                                available_width,
                            );
                        }
                    });
            } else if let Some(def) = vcp_def {
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        ui.set_min_width(available_width);
                        for elev in def.elevations.iter() {
                            render_elevation_row(
                                ui,
                                elev.angle,
                                Some(elev),
                                false,
                                available_width,
                            );
                        }
                    });
            }
        }
        None => {
            ui.label(
                RichText::new("No scan data at current time")
                    .small()
                    .color(Color32::GRAY),
            );
        }
    }
}

/// Get a brief description of what a sweep at this elevation/waveform does
fn get_sweep_info(elevation: f32, waveform: Option<&str>) -> &'static str {
    match waveform {
        Some("CS") => {
            // Contiguous Surveillance - optimized for reflectivity at long range
            if elevation < 1.0 {
                "Refl, long rng"
            } else {
                "Refl, surv"
            }
        }
        Some("CD") => {
            // Contiguous Doppler - optimized for velocity data
            if elevation < 3.0 {
                "Vel+Refl"
            } else if elevation < 8.0 {
                "Vel, mid alt"
            } else {
                "Vel, high alt"
            }
        }
        _ => {
            // Fallback based on elevation
            if elevation < 2.0 {
                "Low tilt"
            } else if elevation < 6.0 {
                "Mid tilt"
            } else {
                "High tilt"
            }
        }
    }
}

fn render_elevation_row(
    ui: &mut egui::Ui,
    elevation: f32,
    elev_meta: Option<&crate::state::vcp::VcpElevation>,
    is_current: bool,
    row_width: f32,
) {
    let text_color = if is_current {
        Color32::from_rgb(100, 255, 100)
    } else {
        Color32::from_rgb(180, 180, 180)
    };
    let dim_color = if is_current {
        Color32::from_rgb(80, 200, 80)
    } else {
        Color32::from_rgb(120, 120, 130)
    };

    ui.horizontal(|ui| {
        ui.set_min_width(row_width);

        // Current indicator
        if is_current {
            ui.label(RichText::new("\u{25B6}").color(text_color).small());
        } else {
            ui.label(RichText::new(" ").monospace().small());
        }

        // Elevation angle
        ui.label(
            RichText::new(format!("{:4.1}\u{00B0}", elevation))
                .color(text_color)
                .monospace()
                .small(),
        );

        // Waveform type
        let waveform = elev_meta.map(|e| e.waveform).unwrap_or("--");
        ui.label(
            RichText::new(format!(" {:2}", waveform))
                .color(dim_color)
                .monospace()
                .small(),
        );

        // PRF
        let prf = elev_meta.map(|e| e.prf).unwrap_or("--");
        let prf_short = match prf {
            "Low" => "L",
            "Med" => "M",
            "High" => "H",
            _ => "-",
        };
        ui.label(
            RichText::new(format!(" {}", prf_short))
                .color(dim_color)
                .monospace()
                .small(),
        );

        // Info/description - right aligned
        let info = get_sweep_info(elevation, elev_meta.map(|e| e.waveform));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(info).color(dim_color).small());
        });
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
