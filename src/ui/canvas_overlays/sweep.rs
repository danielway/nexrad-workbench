//! Radar sweep overlay: range rings, radial lines, sweep animation, and donut chart.
//!
//! Draws the radar coverage grid (range rings, 30-degree radials, cardinal labels)
//! centered on the active site. During sweep animation, renders a rotating sweep
//! line and a donut chart showing current vs. previous sweep angular coverage.
//! In live mode, the donut chart shows per-chunk coverage with distinct colors.

use super::super::canvas::{format_age_compact, format_time_short};
use super::super::colors::{canvas as canvas_colors, radar};
use crate::geo::MapProjection;
use crate::nexrad::RADAR_COVERAGE_RANGE_KM;
use crate::state::AppState;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, Vec2};
use geo_types::Coord;
use std::f32::consts::PI;

pub(crate) fn render_radar_sweep(
    painter: &Painter,
    projection: &MapProjection,
    state: &AppState,
    sweep_info: Option<(f32, f32)>,
    stale: bool,
) {
    let radar_lat = state.viz_state.center_lat;
    let radar_lon = state.viz_state.center_lon;
    let dark = state.is_dark;

    // Compute center from geographic projection (same as GPU renderer)
    let center = projection.geo_to_screen(Coord {
        x: radar_lon,
        y: radar_lat,
    });

    // Compute radius in screen pixels for the coverage range
    let range_km = RADAR_COVERAGE_RANGE_KM;
    let km_to_deg = 1.0 / 111.0;
    let lat_correction = radar_lat.to_radians().cos();
    let lon_range = range_km * km_to_deg / lat_correction;

    let edge = projection.geo_to_screen(Coord {
        x: radar_lon + lon_range,
        y: radar_lat,
    });
    let radius = (edge.x - center.x).abs();

    // Draw range rings
    let ring_color = canvas_colors::ring(dark);
    let ring_major_color = canvas_colors::ring_major(dark);
    let num_rings = 6;
    for i in 1..=num_rings {
        let ring_radius = radius * (i as f32 / num_rings as f32);
        let is_major = i % 2 == 0;
        let color = if is_major {
            ring_major_color
        } else {
            ring_color
        };
        let width = if is_major { 1.5 } else { 1.0 };
        painter.circle_stroke(center, ring_radius, Stroke::new(width, color));
    }

    // Draw radial lines (every 30 degrees)
    let radial_color = canvas_colors::radial(dark);
    for i in 0..12 {
        let angle = (i as f32) * 30.0 * PI / 180.0 - PI / 2.0;
        let end_x = center.x + radius * angle.cos();
        let end_y = center.y + radius * angle.sin();
        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(0.5, radial_color),
        );
    }

    // Draw cardinal direction labels
    let label_offset = radius + 15.0;
    let font_id = egui::FontId::proportional(12.0);
    let cardinal_color = canvas_colors::cardinal_label(dark);

    painter.text(
        center + Vec2::new(0.0, -label_offset),
        egui::Align2::CENTER_BOTTOM,
        "N",
        font_id.clone(),
        cardinal_color,
    );
    painter.text(
        center + Vec2::new(label_offset, 0.0),
        egui::Align2::LEFT_CENTER,
        "E",
        font_id.clone(),
        cardinal_color,
    );
    painter.text(
        center + Vec2::new(0.0, label_offset),
        egui::Align2::CENTER_TOP,
        "S",
        font_id.clone(),
        cardinal_color,
    );
    painter.text(
        center + Vec2::new(-label_offset, 0.0),
        egui::Align2::RIGHT_CENTER,
        "W",
        font_id,
        cardinal_color,
    );

    // Draw center marker (radar site)
    painter.circle_filled(center, 4.0, canvas_colors::center_marker(dark));
    painter.circle_stroke(
        center,
        4.0,
        Stroke::new(1.0, canvas_colors::center_marker_stroke(dark)),
    );

    // Draw the sweep line and donut chart if sweep animation is active.
    // In live mode, sweep_info = data boundaries (matching GPU compositing),
    // and the "now" line is drawn separately at the estimated antenna position.
    if let Some((az, start_az)) = sweep_info {
        let is_live = state.live_radar_model.active;

        let (start_line_color, data_edge_color, data_edge_width) = if stale {
            (
                radar::sweep_start_line_stale(),
                radar::sweep_line_stale(),
                2.0,
            )
        } else {
            (radar::sweep_start_line(), radar::SWEEP_LINE, 3.0)
        };

        // Line at data start boundary
        let start_angle_rad = (start_az - 90.0) * PI / 180.0;
        let start_end = Pos2::new(
            center.x + radius * start_angle_rad.cos(),
            center.y + radius * start_angle_rad.sin(),
        );
        painter.line_segment([center, start_end], Stroke::new(1.5, start_line_color));

        // Line at data trailing edge
        let data_angle_rad = (az - 90.0) * PI / 180.0;
        painter.line_segment(
            [
                center,
                Pos2::new(
                    center.x + radius * data_angle_rad.cos(),
                    center.y + radius * data_angle_rad.sin(),
                ),
            ],
            Stroke::new(if is_live { 2.0 } else { data_edge_width }, data_edge_color),
        );

        // In live mode, draw a separate "NOW" line at the estimated antenna position
        if is_live {
            if let Some(now_az) = state.live_radar_model.estimated_azimuth {
                let now_rad = (now_az - 90.0) * PI / 180.0;
                let now_color = Color32::from_rgb(255, 80, 80);
                painter.line_segment(
                    [
                        center,
                        Pos2::new(
                            center.x + radius * now_rad.cos(),
                            center.y + radius * now_rad.sin(),
                        ),
                    ],
                    Stroke::new(2.0, now_color),
                );

                // "NOW" label — same metadata style as slice labels
                let now_label_radius = radius + 4.0 + 6.0 + 14.0; // donut_outer + offset
                let collecting_label = state
                    .live_radar_model
                    .position
                    .as_ref()
                    .and_then(|p| {
                        let now_secs = js_sys::Date::now() / 1000.0;
                        p.elevation_index_at(now_secs).and_then(|idx| {
                            p.sweeps.get(idx).map(|s| {
                                let angle = state
                                    .live_radar_model
                                    .volume
                                    .as_ref()
                                    .and_then(|v| v.vcp_pattern.as_ref())
                                    .and_then(|vcp| {
                                        vcp.elevations
                                            .get(s.elevation_number.saturating_sub(1) as usize)
                                            .map(|el| format!("{:.1}\u{00B0}", el.angle))
                                    })
                                    .unwrap_or_default();
                                format!("NOW \u{00B7} S{} {}", s.elevation_number, angle)
                            })
                        })
                    })
                    .unwrap_or_else(|| "NOW".to_string());

                draw_boundary_label(
                    painter,
                    center,
                    now_label_radius,
                    now_az,
                    &collecting_label,
                    None,
                    now_color,
                    now_color,
                    &egui::FontId::monospace(10.0),
                );
            }
        }

        // Draw chunk boundary lines across the radar render during live streaming
        if let Some(sweep) = state.live_radar_model.active_sweep.as_ref() {
            let boundary_line_color = Color32::from_rgba_unmultiplied(200, 200, 220, 100);
            for c in sweep
                .chunks
                .iter()
                .take(sweep.chunks.len().saturating_sub(1))
            {
                let a = (c.last_az - 90.0) * PI / 180.0;
                let p_end = Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin());
                painter.line_segment([center, p_end], Stroke::new(1.0, boundary_line_color));
            }
        }

        // Donut chart showing current vs previous sweep regions
        if is_live || state.effective_sweep_animation() {
            if stale {
                draw_sweep_donut_stale(painter, center, radius);
            } else {
                draw_sweep_donut(painter, center, radius, az, start_az, state);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_boundary_label(
    painter: &Painter,
    center: Pos2,
    label_radius: f32,
    azimuth_deg: f32,
    left_text: &str,
    right_text: Option<&str>,
    left_color: Color32,
    right_color: Color32,
    font: &egui::FontId,
) {
    let label_angle = (azimuth_deg - 90.0) * PI / 180.0;
    let label_pos = Pos2::new(
        center.x + label_radius * label_angle.cos(),
        center.y + label_radius * label_angle.sin(),
    );
    let align = sweep_label_align(azimuth_deg);

    // Build a LayoutJob with colored segments
    let mut job = egui::text::LayoutJob::default();
    job.append(
        left_text,
        0.0,
        egui::TextFormat {
            font_id: font.clone(),
            color: left_color,
            ..Default::default()
        },
    );
    if let Some(right) = right_text {
        job.append(
            " | ",
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: Color32::from_rgb(120, 120, 140),
                ..Default::default()
            },
        );
        job.append(
            right,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: right_color,
                ..Default::default()
            },
        );
    }

    let galley = painter.layout_job(job);
    let text_size = galley.size();
    let bg_pos = align_pos(label_pos, text_size, align);
    let padding = Vec2::new(3.0, 2.0);
    painter.rect_filled(
        Rect::from_min_size(bg_pos - padding, text_size + padding * 2.0),
        3.0,
        Color32::from_rgba_unmultiplied(15, 15, 25, 200),
    );
    painter.galley(bg_pos, galley, Color32::WHITE);
}

fn draw_sweep_donut_stale(painter: &Painter, center: Pos2, radius: f32) {
    let donut_inner = radius + 4.0;
    let donut_outer = radius + 10.0;
    let donut_mid = (donut_inner + donut_outer) / 2.0;
    let donut_width = donut_outer - donut_inner;

    let color = Color32::from_rgba_unmultiplied(100, 100, 110, 80);
    let segments = 180;

    for i in 0..segments {
        let frac_start = i as f32 / segments as f32;
        let frac_end = (i + 1) as f32 / segments as f32;

        let a1 = (frac_start * 360.0 - 90.0) * PI / 180.0;
        let a2 = (frac_end * 360.0 - 90.0) * PI / 180.0;

        let p1 = Pos2::new(
            center.x + donut_mid * a1.cos(),
            center.y + donut_mid * a1.sin(),
        );
        let p2 = Pos2::new(
            center.x + donut_mid * a2.cos(),
            center.y + donut_mid * a2.sin(),
        );

        painter.line_segment([p1, p2], Stroke::new(donut_width, color));
    }
}

fn draw_sweep_donut(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    sweep_az: f32,
    sweep_start: f32,
    state: &AppState,
) {
    let donut_inner = radius + 4.0;
    let donut_outer = radius + 10.0;
    let donut_mid = (donut_inner + donut_outer) / 2.0;
    let donut_width = donut_outer - donut_inner;

    let current_color = Color32::from_rgba_unmultiplied(80, 200, 120, 160);
    let prev_color = Color32::from_rgba_unmultiplied(120, 120, 180, 120);
    let current_text_color = Color32::from_rgb(100, 220, 140);
    let prev_text_color = Color32::from_rgb(160, 160, 220);

    let swept_arc_deg = (sweep_az - sweep_start).rem_euclid(360.0);
    let prev_arc_deg = 360.0 - swept_arc_deg;

    // Draw the two-tone arc ring
    let segments = 180;
    for i in 0..segments {
        let frac_start = i as f32 / segments as f32;
        let frac_end = (i + 1) as f32 / segments as f32;
        let mid_deg = (frac_start + frac_end) / 2.0 * 360.0;
        let color = if mid_deg < swept_arc_deg {
            current_color
        } else {
            prev_color
        };
        let a1 = ((sweep_start + frac_start * 360.0) - 90.0) * PI / 180.0;
        let a2 = ((sweep_start + frac_end * 360.0) - 90.0) * PI / 180.0;
        painter.line_segment(
            [
                Pos2::new(
                    center.x + donut_mid * a1.cos(),
                    center.y + donut_mid * a1.sin(),
                ),
                Pos2::new(
                    center.x + donut_mid * a2.cos(),
                    center.y + donut_mid * a2.sin(),
                ),
            ],
            Stroke::new(donut_width, color),
        );
    }

    let label_radius = donut_outer + 14.0;
    let label_font = egui::FontId::monospace(10.0);
    let use_local = state.use_local_time;
    let is_live = state.live_radar_model.active;

    // ── Gather sweep metadata for both slices ─────────────────────────
    // Helper to format a timestamp with age
    let fmt_time = |ts: f64| -> String {
        let mut s = format_time_short(ts, use_local);
        if let Some(age) = format_age_compact(ts) {
            s.push(' ');
            s.push_str(&age);
        }
        s
    };

    // Helper to look up elevation angle from VCP pattern
    let elev_angle_str = |elev_num: u8, vcp: Option<&crate::data::keys::ExtractedVcp>| -> String {
        vcp.and_then(|v| {
            v.elevations
                .get(elev_num.saturating_sub(1) as usize)
                .map(|el| format!("{:.1}\u{00B0}", el.angle))
        })
        .unwrap_or_default()
    };

    // Current sweep: time at data edge, time at sweep start, metadata
    let (cur_edge_time, cur_start_time, cur_meta): (
        Option<String>,
        Option<String>,
        Option<String>,
    );
    // Previous sweep: time at data edge (=sweep start boundary), metadata
    let (prev_edge_time, prev_meta): (Option<String>, Option<String>);

    if is_live {
        let model = &state.live_radar_model;
        let sweep = model.active_sweep.as_ref();
        let vcp = model.volume.as_ref().and_then(|v| v.vcp_pattern.as_ref());

        // Current sweep edge time: latest chunk end time
        cur_edge_time =
            sweep.and_then(|s| s.chunk_time_spans.last().map(|&(_, end, _)| fmt_time(end)));
        // Current sweep start time: first chunk start time
        cur_start_time = sweep.and_then(|s| {
            s.chunk_time_spans
                .first()
                .map(|&(start, _, _)| fmt_time(start))
        });

        // Current sweep metadata
        cur_meta = sweep.map(|s| {
            let angle = elev_angle_str(s.elevation_number, vcp);
            let completed = model
                .volume
                .as_ref()
                .map(|v| v.elevations_complete.len())
                .unwrap_or(0);
            let expected = model
                .volume
                .as_ref()
                .and_then(|v| v.elevations_expected)
                .map(|n| format!("/{}", n))
                .unwrap_or_default();
            format!(
                "S{} {} \u{00B7} {}r \u{00B7} {}{} elev",
                s.elevation_number, angle, s.radials_received, completed, expected
            )
        });

        // Previous sweep: last completed elevation
        let prev_elev = model
            .volume
            .as_ref()
            .and_then(|v| v.elevations_complete.last().copied());
        prev_edge_time = None; // no precise time available for live prev sweep edge
        prev_meta = prev_elev.map(|pe| {
            let angle = elev_angle_str(pe, vcp);
            format!("S{} {}", pe, angle)
        });
    } else {
        // Cached playback
        let playback_ts = state.playback_state.playback_position();
        let displayed_elev = state.viz_state.displayed_sweep_elevation_number;

        // Look up current sweep from timeline
        let current_sweep_info = state
            .radar_timeline
            .find_recent_scan(playback_ts, 15.0 * 60.0)
            .and_then(|scan| {
                scan.sweeps
                    .iter()
                    .filter(|s| Some(s.elevation_number) == displayed_elev)
                    .rfind(|s| s.start_time <= playback_ts)
                    .or_else(|| {
                        scan.sweeps
                            .iter()
                            .find(|s| Some(s.elevation_number) == displayed_elev)
                    })
                    .map(|s| (s.elevation_number, s.elevation, s.start_time, s.end_time))
            });

        cur_edge_time = Some(fmt_time(playback_ts));
        cur_start_time = current_sweep_info.map(|(_, _, start, _)| fmt_time(start));
        cur_meta =
            current_sweep_info.map(|(en, angle, _, _)| format!("S{} {:.1}\u{00B0}", en, angle));

        // Previous sweep times
        let prev_overlay = state.viz_state.prev_sweep_overlay;
        prev_edge_time = prev_overlay.map(|(_, _, prev_end)| fmt_time(prev_end));
        prev_meta = state.viz_state.prev_sweep_elevation_number.map(|pe| {
            let angle = prev_overlay
                .map(|(elev_deg, _, _)| format!("{:.1}\u{00B0}", elev_deg))
                .unwrap_or_default();
            format!("S{} {}", pe, angle)
        });

        // Also compute prev time at data edge for the data-edge boundary label
        // (interpolated from prev sweep's time range at the current sweep azimuth)
    }

    // Prev sweep time interpolated at the data-edge azimuth (for cached playback)
    let prev_at_edge_time = if !is_live {
        state.viz_state.prev_sweep_overlay.map(|(_, ps, pe)| {
            let frac = (swept_arc_deg / 360.0).clamp(0.0, 1.0) as f64;
            fmt_time(ps + frac * (pe - ps))
        })
    } else {
        None
    };

    // ── Boundary label at data edge (sweep_az) ───────────────────────
    {
        let left = cur_edge_time.as_deref().unwrap_or("");
        let right = prev_at_edge_time.as_deref();
        if !left.is_empty() || right.is_some() {
            draw_boundary_label(
                painter,
                center,
                label_radius,
                sweep_az,
                left,
                right,
                current_text_color,
                prev_text_color,
                &label_font,
            );
        }
    }

    // ── Boundary label at sweep start (sweep_start) ──────────────────
    if swept_arc_deg >= 30.0 {
        let left = cur_start_time.as_deref();
        let right = prev_edge_time.as_deref();
        match (left, right) {
            (Some(l), Some(r)) => {
                draw_boundary_label(
                    painter,
                    center,
                    label_radius,
                    sweep_start,
                    l,
                    Some(r),
                    current_text_color,
                    prev_text_color,
                    &label_font,
                );
            }
            (Some(l), None) => {
                draw_boundary_label(
                    painter,
                    center,
                    label_radius,
                    sweep_start,
                    l,
                    None,
                    current_text_color,
                    current_text_color,
                    &label_font,
                );
            }
            (None, Some(r)) => {
                draw_boundary_label(
                    painter,
                    center,
                    label_radius,
                    sweep_start,
                    r,
                    None,
                    prev_text_color,
                    prev_text_color,
                    &label_font,
                );
            }
            (None, None) => {}
        }
    }

    // ── Metadata label centered over the current (green) slice ───────
    if swept_arc_deg >= 30.0 {
        if let Some(ref meta) = cur_meta {
            let mid_az = sweep_start + swept_arc_deg / 2.0;
            draw_boundary_label(
                painter,
                center,
                label_radius,
                mid_az,
                meta,
                None,
                current_text_color,
                current_text_color,
                &label_font,
            );
        }
    }

    // ── Metadata label centered over the previous (purple) slice ─────
    if prev_arc_deg >= 30.0 {
        if let Some(ref meta) = prev_meta {
            let mid_az = sweep_az + prev_arc_deg / 2.0;
            draw_boundary_label(
                painter,
                center,
                label_radius,
                mid_az,
                meta,
                None,
                prev_text_color,
                prev_text_color,
                &label_font,
            );
        }
    }
}

fn sweep_label_align(az_deg: f32) -> egui::Align2 {
    // Determine which quadrant the label falls in
    let az = az_deg.rem_euclid(360.0);
    if !(45.0..315.0).contains(&az) {
        egui::Align2::CENTER_BOTTOM // top
    } else if az < 135.0 {
        egui::Align2::LEFT_CENTER // right
    } else if az < 225.0 {
        egui::Align2::CENTER_TOP // bottom
    } else {
        egui::Align2::RIGHT_CENTER // left
    }
}

fn align_pos(pos: Pos2, size: Vec2, align: egui::Align2) -> Pos2 {
    let x = match align.x() {
        egui::Align::Min => pos.x,
        egui::Align::Center => pos.x - size.x / 2.0,
        egui::Align::Max => pos.x - size.x,
    };
    let y = match align.y() {
        egui::Align::Min => pos.y,
        egui::Align::Center => pos.y - size.y / 2.0,
        egui::Align::Max => pos.y - size.y,
    };
    Pos2::new(x, y)
}
