//! Left panel UI: radar operations visualization.

use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::state::{get_vcp_definition, radar_data::Scan, AppState};
use eframe::egui::{self, Color32, Pos2, RichText, Stroke, Vec2};
use std::f32::consts::PI;

pub(super) struct LeftPanelLayer;

impl Layer for LeftPanelLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        30
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        // Power-user diagnostics: hidden unless the user has the side
        // panel open AND has Advanced mode enabled.
        ctx.chrome.left_sidebar_visible && ctx.state.show_advanced()
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_left_panel(ctx.ctx, ctx.state, ctx.timeline, ctx.live, ctx.playback);
    }
}

/// State queried from the radar timeline at the current timestamp
struct RadarStateAtTimestamp<'a> {
    /// Current azimuth angle in degrees (0-360), from actual radial data
    azimuth: Option<f32>,
    /// Current elevation angle in degrees, from actual radial data
    elevation: Option<f32>,
    /// Current VCP number
    vcp: Option<u16>,
    /// Elevation number (1-based VCP cut ordinal) of the sweep currently on
    /// the canvas. The VCP-row highlight is keyed off this so it stays in
    /// sync with the displayed cut even when the scan is missing elevations.
    current_elevation_number: Option<u8>,
    /// Scan progress as a percentage (0.0-1.0)
    scan_progress: Option<f32>,
    /// Reference to the current scan (for elevation list)
    scan: Option<&'a Scan>,
    /// Extracted VCP pattern from live streaming (used when scan is None)
    live_vcp_pattern: Option<&'a crate::data::keys::ExtractedVcp>,
    /// Unified position model with sweep timing (live or archived)
    position: Option<crate::nexrad::projection::ScanProjection>,
}

fn draw_left_panel(
    ctx: &egui::Context,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
) {
    egui::SidePanel::left("left_panel")
        .resizable(true)
        .default_width(235.0)
        .min_width(235.0)
        .max_width(400.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                render_radar_operations_section(ui, state, timeline, live, playback);
            });
        });
}

fn render_radar_operations_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
) {
    // Header
    ui.label(RichText::new("Radar Operations").strong().size(14.0));

    ui.add_space(4.0);

    let radar_state = query_radar_state_at_timestamp(state, timeline, live, playback);

    // Top-down and side views side-by-side. The "future data" sector only
    // makes sense while the playhead tracks the live edge — a detached
    // background stream renders the archive state under the cursor.
    let is_live = live.app_mode == crate::state::AppMode::Live;
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

fn query_radar_state_at_timestamp<'a>(
    state: &'a AppState,
    timeline: &'a crate::subsystem::Timeline,
    live: &'a crate::subsystem::Live,
    playback: &'a crate::subsystem::Playback,
) -> RadarStateAtTimestamp<'a> {
    let ts = playback.state.playback_position();

    // Resolve position detail through the same single adapter the timeline
    // uses, so the panel can't drift from it. The in-progress volume is
    // excluded from `settled_scan_at` and surfaced via `live_volume()` (with
    // its already-cached cuts merged in) — this replaces the bespoke
    // archive-vs-live reconciliation this function used to do itself.
    let now = state.frame_now.secs();
    let view = crate::state::TimelineView::build(
        &timeline.scans,
        &timeline.shadow_scan_boundaries,
        Some(&live.mode_state),
        live.radar_model.position.as_ref(),
        state.viz_state.elevation_selection.elevation_number(),
        now,
    );

    match view.settled_scan_at(ts) {
        Some(scan) => {
            // Time-window match: drives the rotating-azimuth animation,
            // which is only meaningful while the cursor is inside a
            // sweep's [start, end] interval.
            let sweep_at_ts = scan.find_sweep_at_timestamp(ts);
            // Highlight match: when the cursor sits in a gap between sweeps,
            // show the most-recently-completed sweep. Sweeps are stored in
            // elevation order, not time order (SAILS-style VCPs revisit the
            // lowest cut), so pick by max end_time rather than Vec position.
            let sweep_for_highlight = sweep_at_ts.or_else(|| {
                scan.sweeps
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.end_time <= ts)
                    .max_by(|(_, a), (_, b)| {
                        a.end_time
                            .partial_cmp(&b.end_time)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });

            // At high playback speeds (>30 s/s), freeze all animated radar state
            // (azimuth, elevation, sweep indicator, progress) to prevent violent flashing.
            // Static VCP info (number, name, elevation list) still renders.
            let is_fast = playback.state.playing
                && playback.state.speed.timeline_seconds_per_real_second() > 30.0;

            let azimuth = if is_fast {
                None
            } else {
                sweep_at_ts.and_then(|(_, sweep)| {
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
                sweep_for_highlight.map(|(_, s)| scan.display_angle(s))
            };
            let current_elevation_number = if is_fast {
                None
            } else {
                sweep_for_highlight.map(|(_, s)| s.elevation_number)
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
                current_elevation_number,
                scan_progress,
                scan: Some(scan),
                live_vcp_pattern: None,
                position: Some(crate::nexrad::projection::scan_to_projection(scan)),
            }
        }
        None => {
            // In live mode, read the frame-snapshotted derivations from
            // LiveRadarModel rather than re-evaluating with a fresh
            // js_sys::Date::now() — that would drift by ~frame-render
            // duration against every other surface that consumed the same
            // model.
            if let Some(position) = view.live_volume() {
                let frame = &live.radar_model.frame_now;
                let vcp = Some(position.vcp_number).filter(|&v| v > 0);
                let azimuth = live.radar_model.estimated_azimuth;
                let sweep_index = frame.sweep_index.or_else(|| {
                    position
                        .in_progress_elevation
                        .map(|e| e.saturating_sub(1) as usize)
                });
                let scan_progress = frame.progress;
                let elevation = frame.elevation_angle.or_else(|| {
                    sweep_index.and_then(|idx| position.sweeps.get(idx).map(|s| s.elevation_angle))
                });
                let current_elevation_number = sweep_index
                    .and_then(|idx| position.sweeps.get(idx).map(|s| s.elevation_number))
                    .or(position.in_progress_elevation);

                RadarStateAtTimestamp {
                    azimuth,
                    elevation,
                    vcp,
                    current_elevation_number,
                    scan_progress,
                    scan: None,
                    live_vcp_pattern: live
                        .radar_model
                        .volume
                        .as_ref()
                        .and_then(|v| v.vcp_pattern.as_ref()),
                    position: Some(position.clone()),
                }
            } else {
                RadarStateAtTimestamp {
                    azimuth: None,
                    elevation: None,
                    vcp: None,
                    current_elevation_number: None,
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
                radar_state.current_elevation_number,
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
    position: Option<&crate::nexrad::projection::ScanProjection>,
    current_elevation_number: Option<u8>,
) -> Vec<ElevRow<'a>> {
    // Helper to get sweep start offset (from volume start) for a given index.
    let timing_for = |idx: usize| -> (Option<f64>, bool) {
        position
            .and_then(|p| {
                let sp = p.sweeps.get(idx)?;
                let offset = sp.collection_start_secs - p.volume_start;
                let estimated = !sp.is_observed();
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
                let elevation_number = (idx + 1) as u8;
                ElevRow {
                    elevation_number,
                    elevation_angle: elev.angle,
                    is_current: current_elevation_number == Some(elevation_number),
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
                let target_angle = scan.display_angle(sweep);
                let meta = vcp_def.and_then(|def| {
                    def.elevations
                        .iter()
                        .find(|e| (e.angle - target_angle).abs() < 0.1)
                });
                ElevRow {
                    elevation_number: sweep.elevation_number,
                    elevation_angle: target_angle,
                    is_current: current_elevation_number == Some(sweep.elevation_number),
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
                let elevation_number = (idx + 1) as u8;
                ElevRow {
                    elevation_number,
                    elevation_angle: elev.angle,
                    is_current: current_elevation_number == Some(elevation_number),
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

    egui::Grid::new("vcp_elev_grid")
        .spacing([4.0, 1.0])
        .show(ui, |ui| {
            // Header
            ui.label(
                RichText::new("Elev")
                    .font(hdr_font.clone())
                    .color(hdr_color),
            );
            ui.label(RichText::new("Wf").font(hdr_font.clone()).color(hdr_color));
            ui.label(RichText::new("PRF").font(hdr_font.clone()).color(hdr_color));
            ui.label(
                RichText::new("Time")
                    .font(hdr_font.clone())
                    .color(hdr_color),
            );
            ui.label(RichText::new("Products").font(hdr_font).color(hdr_color));
            ui.end_row();

            for row in rows {
                let text_color = if row.is_current {
                    Color32::from_rgb(100, 255, 100)
                } else {
                    Color32::from_rgb(180, 180, 180)
                };
                let dim_color = if row.is_current {
                    Color32::from_rgb(80, 200, 80)
                } else {
                    Color32::from_rgb(120, 120, 130)
                };

                // Elevation number + angle
                ui.label(
                    RichText::new(format!(
                        "{:<2}{:>5.1}\u{00B0}",
                        row.elevation_number, row.elevation_angle
                    ))
                    .color(text_color)
                    .font(font.clone()),
                );

                // Waveform
                let wf_resp = ui.label(
                    RichText::new(row.waveform)
                        .color(dim_color)
                        .font(font.clone()),
                );
                match row.waveform {
                    "CS" => wf_resp.on_hover_text("Contiguous Surveillance"),
                    "CD" => wf_resp.on_hover_text("Contiguous Doppler"),
                    "B" => wf_resp.on_hover_text("Batch"),
                    "SP" => wf_resp.on_hover_text("Staggered Pulse Pair"),
                    _ => wf_resp,
                };

                // PRF
                let prf_resp = ui.label(
                    RichText::new(row.prf_short)
                        .color(dim_color)
                        .font(font.clone()),
                );
                match row.prf_short {
                    "L" => prf_resp.on_hover_text("Low PRF"),
                    "M" => prf_resp.on_hover_text("Medium PRF"),
                    "H" => prf_resp.on_hover_text("High PRF"),
                    _ => prf_resp,
                };

                // Time (offset from volume start as M:SS)
                let time_text = match row.start_offset_secs {
                    Some(offset) if offset >= 0.0 => {
                        let secs = offset.round() as u32;
                        let m = secs / 60;
                        let s = secs % 60;
                        if row.timing_estimated {
                            format!("~{}:{:02}", m, s)
                        } else {
                            format!("{}:{:02}", m, s)
                        }
                    }
                    _ => "--:--".to_string(),
                };
                let time_resp =
                    ui.label(RichText::new(time_text).color(dim_color).font(font.clone()));
                if row.timing_estimated {
                    time_resp.on_hover_text("Estimated from VCP azimuth rates");
                } else {
                    time_resp.on_hover_text("Observed from radial timestamps");
                };

                // Products
                let products = waveform_to_products(row.waveform_raw);
                if products.is_empty() {
                    ui.label(RichText::new("--").color(dim_color).font(font.clone()));
                } else {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for &(letter, (r, g, b)) in products {
                            let resp = ui.label(
                                RichText::new(letter)
                                    .color(Color32::from_rgb(r, g, b))
                                    .font(font.clone()),
                            );
                            match letter {
                                "R" => resp.on_hover_text("Reflectivity"),
                                "V" => resp.on_hover_text("Velocity"),
                                "S" => resp.on_hover_text("Spectrum Width"),
                                "Z" => resp.on_hover_text("Differential Reflectivity"),
                                "P" => resp.on_hover_text("Differential Phase"),
                                "C" => resp.on_hover_text("Correlation Coefficient"),
                                _ => resp,
                            };
                        }
                    });
                }

                ui.end_row();
            }
        });
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
