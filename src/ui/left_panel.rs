//! Left panel UI: radar operations visualization.

use crate::state::{get_vcp_definition, radar_data::Scan, AppState};
use eframe::egui::{self, Color32, Pos2, RichText, Stroke, Vec2};
use std::f32::consts::PI;

/// State queried from the radar timeline at the current timestamp
struct RadarStateAtTimestamp<'a> {
    /// Current azimuth angle in degrees (0-360), from actual radial data
    azimuth: Option<f32>,
    /// Current elevation angle in degrees, from actual radial data
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

pub fn render_left_panel(ctx: &egui::Context, state: &mut AppState) {
    if !state.left_sidebar_visible {
        return;
    }

    egui::SidePanel::left("left_panel")
        .resizable(true)
        .default_width(235.0)
        .min_width(235.0)
        .max_width(400.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                render_radar_operations_section(ui, state);
            });
        });
}

fn render_radar_operations_section(ui: &mut egui::Ui, state: &mut AppState) {
    // Header
    ui.label(RichText::new("Radar Operations").strong().size(14.0));

    ui.add_space(4.0);

    let radar_state = query_radar_state_at_timestamp(state);

    // Top-down and side views side-by-side
    let is_live = state.live_mode_state.is_active();
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new("Azimuth").small());
            render_top_down_view(ui, radar_state.azimuth, is_live);
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

fn query_radar_state_at_timestamp<'a>(state: &'a AppState) -> RadarStateAtTimestamp<'a> {
    let ts = state.playback_state.playback_position();

    // Find the scan at the current timestamp
    let scan = state.radar_timeline.find_scan_at_timestamp(ts);

    match scan {
        Some(scan) => {
            let sweep_data = scan.find_sweep_at_timestamp(ts);

            // At high playback speeds (>30 s/s), freeze all animated radar state
            // (azimuth, elevation, sweep indicator, progress) to prevent violent flashing.
            // Static VCP info (number, name, elevation list) still renders.
            let is_fast = state
                .playback_state
                .speed
                .timeline_seconds_per_real_second()
                > 30.0;

            let azimuth = if is_fast {
                None
            } else {
                sweep_data.and_then(|(_, sweep)| {
                    let dur = sweep.end_time - sweep.start_time;
                    if dur <= 0.0 {
                        return None;
                    }
                    let progress = (ts - sweep.start_time) / dur;
                    Some(((progress * 360.0) as f32) % 360.0)
                })
            };
            let elevation = if is_fast {
                None
            } else {
                sweep_data.map(|(_, s)| s.elevation)
            };
            let sweep_index = if is_fast {
                None
            } else {
                sweep_data.map(|(idx, _)| idx)
            };
            let scan_progress = if is_fast {
                None
            } else {
                scan.progress_at_timestamp(ts)
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
        None => {
            // In live mode, use estimated position from live state even when
            // no persisted scan exists at the current timestamp yet.
            let live = &state.live_mode_state;
            if live.is_active() && live.current_vcp_number.is_some() {
                let now = js_sys::Date::now() / 1000.0;
                let vcp = live.current_vcp_number;
                let azimuth = live.estimated_azimuth(now);
                let sweep_index = live.estimated_elevation_index(now).or_else(|| {
                    // Fall back to the actual in-progress elevation number (1-based → 0-based)
                    live.current_in_progress_elevation
                        .map(|e| e.saturating_sub(1) as usize)
                });
                let scan_progress = live.current_volume_start.and_then(|start| {
                    live.last_volume_duration_secs.map(|dur| {
                        if dur > 0.0 {
                            ((now - start) / dur).clamp(0.0, 1.0) as f32
                        } else {
                            0.0
                        }
                    })
                });
                // Derive elevation angle from VCP definition + estimated index
                let elevation = sweep_index.and_then(|idx| {
                    vcp.and_then(get_vcp_definition)
                        .and_then(|def| def.elevations.get(idx))
                        .map(|e| e.angle)
                });

                RadarStateAtTimestamp {
                    azimuth,
                    elevation,
                    vcp,
                    sweep_index,
                    scan_progress,
                    scan: None,
                }
            } else {
                RadarStateAtTimestamp {
                    azimuth: None,
                    elevation: None,
                    vcp: None,
                    sweep_index: None,
                    scan_progress: None,
                    scan: None,
                }
            }
        }
    }
}

fn render_top_down_view(ui: &mut egui::Ui, azimuth: Option<f32>, is_live: bool) {
    let size = Vec2::new(100.0, 100.0);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = response.rect;
    let center = rect.center();
    let dark = ui.visuals().dark_mode;
    // Leave more room for cardinal labels (12px margin instead of 8)
    let radius = (rect.width().min(rect.height()) / 2.0) - 12.0;

    // Background
    let bg = if dark {
        Color32::from_rgb(30, 30, 40)
    } else {
        Color32::from_rgb(225, 225, 230)
    };
    painter.rect_filled(rect, 4.0, bg);

    // In live mode, draw shaded "future" region (expected upcoming data)
    if is_live {
        if let Some(az) = azimuth {
            // Show a ~90 degree shaded sector ahead of current azimuth
            // This represents ~15 seconds of expected data at typical rotation speed
            let future_extent = 90.0_f32; // degrees

            // Draw shaded arc using multiple line segments
            let start_angle = az;
            let _end_angle = az + future_extent;
            let num_segments = 20;

            for i in 0..num_segments {
                let t = i as f32 / num_segments as f32;
                let angle1 = start_angle + t * future_extent;
                let angle2 = start_angle + (t + 1.0 / num_segments as f32) * future_extent;

                // Convert to radians (0 = North, clockwise, screen coords)
                let rad1 = (angle1 - 90.0) * PI / 180.0;
                let rad2 = (angle2 - 90.0) * PI / 180.0;

                // Create a wedge segment
                let _inner_radius = 0.0;
                let p0 = center;
                let p1 = Pos2::new(
                    center.x + radius * rad1.cos(),
                    center.y + radius * rad1.sin(),
                );
                let p2 = Pos2::new(
                    center.x + radius * rad2.cos(),
                    center.y + radius * rad2.sin(),
                );

                // Draw filled triangle for this segment
                painter.add(egui::Shape::convex_polygon(
                    vec![p0, p1, p2],
                    Color32::from_rgba_unmultiplied(80, 80, 120, 50),
                    Stroke::NONE,
                ));
            }
        }
    }

    // Concentric range rings
    let ring_color = if dark {
        Color32::from_rgb(60, 60, 80)
    } else {
        Color32::from_rgb(170, 170, 190)
    };
    for factor in [0.33, 0.66, 1.0] {
        painter.circle_stroke(center, radius * factor, Stroke::new(1.0, ring_color));
    }

    // Cardinal direction labels (inside the radar circle for cleaner look)
    let label_color = if dark {
        Color32::from_rgb(100, 100, 120)
    } else {
        Color32::from_rgb(80, 80, 100)
    };
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
    let dark = ui.visuals().dark_mode;

    // Background
    let bg = if dark {
        Color32::from_rgb(30, 30, 40)
    } else {
        Color32::from_rgb(225, 225, 230)
    };
    painter.rect_filled(rect, 4.0, bg);

    // Ground line at bottom
    let ground_y = rect.bottom() - 8.0;
    let ground_color = if dark {
        Color32::from_rgb(80, 60, 40)
    } else {
        Color32::from_rgb(140, 110, 80)
    };
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
    let tower_color = if dark {
        Color32::from_rgb(150, 150, 150)
    } else {
        Color32::from_rgb(100, 100, 100)
    };
    painter.line_segment(
        [
            Pos2::new(tower_x, tower_bottom),
            Pos2::new(tower_x, tower_top),
        ],
        Stroke::new(3.0, tower_color),
    );

    // Dish (small circle at top of tower)
    let dish_color = if dark {
        Color32::from_rgb(200, 200, 200)
    } else {
        Color32::from_rgb(80, 80, 80)
    };
    painter.circle_filled(Pos2::new(tower_x, tower_top), 4.0, dish_color);

    // Reference angle lines (0°, 10°, 20°)
    let beam_origin = Pos2::new(tower_x, tower_top);
    let beam_length = rect.width() - 30.0;
    let ref_line_color = if dark {
        Color32::from_rgb(60, 60, 80)
    } else {
        Color32::from_rgb(170, 170, 190)
    };
    let label_color = if dark {
        Color32::from_rgb(100, 100, 120)
    } else {
        Color32::from_rgb(80, 80, 100)
    };
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

            // Prefer extracted VCP pattern (from Message Type 5), then static definitions
            let extracted_pattern = radar_state.scan.and_then(|s| s.vcp_pattern.as_ref());
            let vcp_def = get_vcp_definition(vcp);

            if let Some(scan) = radar_state.scan {
                egui::ScrollArea::vertical()
                    .max_height(f32::INFINITY)
                    .show(ui, |ui| {
                        ui.set_min_width(available_width);
                        if let Some(pattern) = extracted_pattern {
                            // Use extracted VCP elevations (full fidelity from scan data)
                            for (idx, elev) in pattern.elevations.iter().enumerate() {
                                // Match by index: VCP pattern entries correspond 1:1 with sweeps
                                let is_current = radar_state.sweep_index == Some(idx);
                                let wf_short = match elev.waveform.as_str() {
                                    "CS" => "CS",
                                    "CDW" | "CDWO" => "CD",
                                    "B" => "B",
                                    "SPP" => "SP",
                                    _ => "--",
                                };
                                let meta = ElevRowMeta {
                                    waveform: wf_short,
                                    prf_short: prf_number_to_short(elev.prf_number),
                                    waveform_raw: &elev.waveform,
                                };
                                render_elevation_row(
                                    ui,
                                    elev.angle,
                                    Some(meta),
                                    is_current,
                                    available_width,
                                );
                            }
                        } else {
                            // Fall back: use sweep metadata with static VCP definitions
                            for (idx, sweep) in scan.sweeps.iter().enumerate() {
                                let is_current = radar_state.sweep_index == Some(idx);
                                let meta = vcp_def.and_then(|def| {
                                    def.elevations
                                        .iter()
                                        .find(|e| (e.angle - sweep.elevation).abs() < 0.1)
                                        .map(static_vcp_meta)
                                });
                                render_elevation_row(
                                    ui,
                                    sweep.elevation,
                                    meta,
                                    is_current,
                                    available_width,
                                );
                            }
                        }
                    });
            } else if let Some(pattern) = extracted_pattern {
                // No scan reference but have VCP pattern
                egui::ScrollArea::vertical()
                    .max_height(f32::INFINITY)
                    .show(ui, |ui| {
                        ui.set_min_width(available_width);
                        for elev in &pattern.elevations {
                            let wf_short = match elev.waveform.as_str() {
                                "CS" => "CS",
                                "CDW" | "CDWO" => "CD",
                                "B" => "B",
                                "SPP" => "SP",
                                _ => "--",
                            };
                            let meta = ElevRowMeta {
                                waveform: wf_short,
                                prf_short: prf_number_to_short(elev.prf_number),
                                waveform_raw: &elev.waveform,
                            };
                            render_elevation_row(
                                ui,
                                elev.angle,
                                Some(meta),
                                false,
                                available_width,
                            );
                        }
                    });
            } else if let Some(def) = vcp_def {
                // Fall back to static VCP definitions — use sweep_index from
                // live mode estimation to highlight the current elevation.
                egui::ScrollArea::vertical()
                    .max_height(f32::INFINITY)
                    .show(ui, |ui| {
                        ui.set_min_width(available_width);
                        for (idx, elev) in def.elevations.iter().enumerate() {
                            let is_current = radar_state.sweep_index == Some(idx);
                            render_elevation_row(
                                ui,
                                elev.angle,
                                Some(static_vcp_meta(elev)),
                                is_current,
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

/// Map a raw waveform code to available product letters and their colors.
fn waveform_to_products(waveform: &str) -> &'static [(&'static str, (u8, u8, u8))] {
    const REF: (&str, (u8, u8, u8)) = ("R", (80, 200, 80));
    const VEL: (&str, (u8, u8, u8)) = ("V", (200, 80, 80));
    const SW: (&str, (u8, u8, u8)) = ("S", (80, 180, 180));
    const ZDR: (&str, (u8, u8, u8)) = ("Z", (200, 200, 80));
    const PHI: (&str, (u8, u8, u8)) = ("P", (180, 80, 180));
    const RHO: (&str, (u8, u8, u8)) = ("C", (80, 120, 200));

    match waveform {
        "CS" => &[REF],
        "CDW" => &[REF, VEL, SW, ZDR, PHI, RHO],
        "CDWO" => &[REF, VEL, SW],
        "B" => &[REF, VEL, SW],
        "SPP" => &[REF, VEL],
        _ => &[],
    }
}

/// Display-ready elevation metadata for a row.
struct ElevRowMeta<'a> {
    waveform: &'a str,
    prf_short: &'a str,
    /// Original waveform code from VCP (for product mapping). Differs from
    /// `waveform` which may be a shortened display code.
    waveform_raw: &'a str,
}

fn render_elevation_row(
    ui: &mut egui::Ui,
    elevation: f32,
    meta: Option<ElevRowMeta>,
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
            ui.label(
                RichText::new(egui_phosphor::regular::CARET_RIGHT)
                    .color(text_color)
                    .small(),
            );
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
        let waveform = meta.as_ref().map(|m| m.waveform).unwrap_or("--");
        ui.label(
            RichText::new(format!(" {:2}", waveform))
                .color(dim_color)
                .monospace()
                .small(),
        );

        // PRF
        let prf_short = meta.as_ref().map(|m| m.prf_short).unwrap_or("-");
        ui.label(
            RichText::new(format!(" {}", prf_short))
                .color(dim_color)
                .monospace()
                .small(),
        );

        // Product indicators - right aligned
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let products = meta
                .as_ref()
                .map(|m| waveform_to_products(m.waveform_raw))
                .unwrap_or(&[]);
            if products.is_empty() {
                ui.label(RichText::new("--").color(dim_color).small());
            } else {
                // Right-to-left layout reverses order, so iterate backwards
                for &(letter, (r, g, b)) in products.iter().rev() {
                    ui.label(
                        RichText::new(letter)
                            .color(Color32::from_rgb(r, g, b))
                            .monospace()
                            .small(),
                    );
                }
            }
        });
    });
}

/// Convert a static VcpElevation to display metadata.
fn static_vcp_meta(e: &crate::state::vcp::VcpElevation) -> ElevRowMeta<'_> {
    ElevRowMeta {
        waveform: e.waveform,
        prf_short: match e.prf {
            "Low" => "L",
            "Med" => "M",
            "High" => "H",
            _ => "-",
        },
        // Map static waveform codes to raw codes for product lookup
        waveform_raw: match e.waveform {
            "CS" => "CS",
            "CD" => "CDW",
            "B" => "B",
            "SP" => "SPP",
            other => other,
        },
    }
}

/// Convert PRF number (1-8) to a short label.
fn prf_number_to_short(prf: u8) -> &'static str {
    // PRF numbers: 1-3 are low, 4-5 are medium, 6-8 are high
    match prf {
        1..=3 => "L",
        4..=5 => "M",
        6..=8 => "H",
        _ => "-",
    }
}
