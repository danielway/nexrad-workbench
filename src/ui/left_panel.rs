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
    /// Extracted VCP pattern from live streaming (used when scan is None)
    live_vcp_pattern: Option<&'a crate::data::keys::ExtractedVcp>,
    /// Unified position model with sweep timing (live or archived)
    position: Option<crate::state::VcpPositionModel>,
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
                live_vcp_pattern: None,
                position: Some(crate::state::VcpPositionModel::from_scan(scan)),
            }
        }
        None => {
            // In live mode, use the unified VcpPositionModel for azimuth,
            // elevation, and progress instead of reaching into LiveModeState.
            if let Some(ref position) = state.live_radar_model.position {
                let now = js_sys::Date::now() / 1000.0;
                let vcp = Some(position.vcp_number).filter(|&v| v > 0);
                let azimuth = position.estimated_azimuth_at(now);
                let sweep_index = position.elevation_index_at(now).or_else(|| {
                    state
                        .live_mode_state
                        .current_in_progress_elevation
                        .map(|e| e.saturating_sub(1) as usize)
                });
                let scan_progress = Some(position.progress_at(now));
                let elevation =
                    sweep_index.and_then(|idx| position.sweeps.get(idx).map(|s| s.elevation_angle));

                RadarStateAtTimestamp {
                    azimuth,
                    elevation,
                    vcp,
                    sweep_index,
                    scan_progress,
                    scan: None,
                    live_vcp_pattern: state.live_mode_state.current_vcp_pattern.as_ref(),
                    position: Some(position.clone()),
                }
            } else {
                RadarStateAtTimestamp {
                    azimuth: None,
                    elevation: None,
                    vcp: None,
                    sweep_index: None,
                    scan_progress: None,
                    scan: None,
                    live_vcp_pattern: None,
                    position: None,
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

            // Build row data from whichever source is available
            let extracted_pattern = radar_state
                .scan
                .and_then(|s| s.vcp_pattern.as_ref())
                .or(radar_state.live_vcp_pattern);
            let vcp_def = get_vcp_definition(vcp);

            let rows: Vec<ElevRow> = build_elevation_rows(
                radar_state.scan,
                extracted_pattern,
                vcp_def,
                radar_state.position.as_ref(),
                radar_state.sweep_index,
            );

            if rows.is_empty() {
                return;
            }

            // Render as aligned grid
            egui::ScrollArea::vertical()
                .max_height(f32::INFINITY)
                .show(ui, |ui| {
                    render_elevation_grid(ui, &rows);
                });
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

/// Pre-built row data for the elevation grid.
struct ElevRow<'a> {
    elevation_number: u8,
    elevation_angle: f32,
    is_current: bool,
    waveform: &'a str,
    waveform_raw: &'a str,
    prf_short: &'a str,
    /// Sweep start offset from volume start (seconds). Shown as M:SS.
    start_offset_secs: Option<f64>,
    /// Whether the timing is estimated (vs observed from actual data).
    timing_estimated: bool,
}

fn build_elevation_rows<'a>(
    scan: Option<&'a Scan>,
    extracted_pattern: Option<&'a crate::data::keys::ExtractedVcp>,
    vcp_def: Option<&'a crate::state::vcp::VcpDefinition>,
    position: Option<&crate::state::VcpPositionModel>,
    sweep_index: Option<usize>,
) -> Vec<ElevRow<'a>> {
    // Helper to get sweep start offset (from volume start) for a given index.
    let timing_for = |idx: usize| -> (Option<f64>, bool) {
        position
            .and_then(|p| {
                let sp = p.sweeps.get(idx)?;
                let offset = sp.start - p.volume_start;
                let estimated = sp.timing != crate::state::SweepTiming::Observed;
                Some((Some(offset), estimated))
            })
            .unwrap_or((None, true))
    };

    if let Some(pattern) = extracted_pattern {
        pattern
            .elevations
            .iter()
            .enumerate()
            .map(|(idx, elev)| {
                let (start_offset_secs, timing_estimated) = timing_for(idx);
                ElevRow {
                    elevation_number: (idx + 1) as u8,
                    elevation_angle: elev.angle,
                    is_current: sweep_index == Some(idx),
                    waveform: match elev.waveform.as_str() {
                        "CS" => "CS",
                        "CDW" | "CDWO" => "CD",
                        "B" => "B",
                        "SPP" => "SP",
                        _ => "--",
                    },
                    waveform_raw: &elev.waveform,
                    prf_short: prf_number_to_short(elev.prf_number),
                    start_offset_secs,
                    timing_estimated,
                }
            })
            .collect()
    } else if let Some(scan) = scan {
        scan.sweeps
            .iter()
            .enumerate()
            .map(|(idx, sweep)| {
                let (start_offset_secs, timing_estimated) = timing_for(idx);
                let meta = vcp_def.and_then(|def| {
                    def.elevations
                        .iter()
                        .find(|e| (e.angle - sweep.elevation).abs() < 0.1)
                });
                ElevRow {
                    elevation_number: (idx + 1) as u8,
                    elevation_angle: sweep.elevation,
                    is_current: sweep_index == Some(idx),
                    waveform: meta.map(|m| m.waveform).unwrap_or("--"),
                    waveform_raw: meta
                        .map(|m| match m.waveform {
                            "CS" => "CS",
                            "CD" => "CDW",
                            "B" => "B",
                            "SP" => "SPP",
                            other => other,
                        })
                        .unwrap_or("--"),
                    prf_short: meta
                        .map(|m| match m.prf {
                            "Low" => "L",
                            "Med" => "M",
                            "High" => "H",
                            _ => "-",
                        })
                        .unwrap_or("-"),
                    start_offset_secs,
                    timing_estimated,
                }
            })
            .collect()
    } else if let Some(def) = vcp_def {
        def.elevations
            .iter()
            .enumerate()
            .map(|(idx, elev)| {
                let (start_offset_secs, timing_estimated) = timing_for(idx);
                ElevRow {
                    elevation_number: (idx + 1) as u8,
                    elevation_angle: elev.angle,
                    is_current: sweep_index == Some(idx),
                    waveform: elev.waveform,
                    waveform_raw: match elev.waveform {
                        "CS" => "CS",
                        "CD" => "CDW",
                        "B" => "B",
                        "SP" => "SPP",
                        other => other,
                    },
                    prf_short: match elev.prf {
                        "Low" => "L",
                        "Med" => "M",
                        "High" => "H",
                        _ => "-",
                    },
                    start_offset_secs,
                    timing_estimated,
                }
            })
            .collect()
    } else {
        Vec::new()
    }
}

fn render_elevation_grid(ui: &mut egui::Ui, rows: &[ElevRow]) {
    let hdr_color = Color32::from_rgb(130, 130, 140);
    let font = egui::FontId::monospace(10.0);
    let hdr_font = egui::FontId::monospace(9.0);

    // Compact fixed-width table. egui::Grid auto-sizes columns from the
    // widest cell which over-allocates Wf/PRF and clips Products. Use
    // pre-formatted monospace lines instead for precise alignment.
    ui.label(
        RichText::new("Elev      Wf PRF Time  Products")
            .font(hdr_font)
            .color(hdr_color),
    );

    for row in rows {
        let color = if row.is_current {
            Color32::from_rgb(100, 255, 100)
        } else {
            Color32::from_rgb(160, 160, 170)
        };

        let time_text = match row.start_offset_secs {
            Some(offset) if offset >= 0.0 => {
                let secs = offset.round() as u32;
                let m = secs / 60;
                let s = secs % 60;
                if row.timing_estimated {
                    format!("~{}:{:02}", m, s)
                } else {
                    format!(" {}:{:02}", m, s)
                }
            }
            _ => "--:--".to_string(),
        };

        let products = waveform_to_products(row.waveform_raw);
        let product_str: String = if products.is_empty() {
            "--".to_string()
        } else {
            products
                .iter()
                .map(|&(l, _)| l)
                .collect::<Vec<_>>()
                .join("")
        };

        let line = format!(
            "{:<2}{:>5.1}\u{00B0}  {:<2} {:<1}  {:>5}  {}",
            row.elevation_number,
            row.elevation_angle,
            row.waveform,
            row.prf_short,
            time_text,
            product_str,
        );

        let resp = ui.label(RichText::new(&line).font(font.clone()).color(color));

        // Tooltip with full details
        resp.on_hover_ui(|ui| {
            ui.label(format!(
                "Elevation {} ({:.1}°)",
                row.elevation_number, row.elevation_angle
            ));
            let wf_full = match row.waveform {
                "CS" => "Contiguous Surveillance",
                "CD" => "Contiguous Doppler",
                "B" => "Batch",
                "SP" => "Staggered Pulse Pair",
                _ => row.waveform,
            };
            ui.label(format!("Waveform: {}", wf_full));
            let prf_full = match row.prf_short {
                "L" => "Low",
                "M" => "Medium",
                "H" => "High",
                _ => row.prf_short,
            };
            ui.label(format!("PRF: {}", prf_full));
            if row.timing_estimated {
                ui.label("Timing: estimated from VCP azimuth rates");
            } else {
                ui.label("Timing: observed from radial timestamps");
            }
        });
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
