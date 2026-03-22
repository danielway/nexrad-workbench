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

    // Draw the sweep line and donut chart if sweep animation is active
    if let Some((az, start_az)) = sweep_info {
        let (start_line_color, sweep_line_color, sweep_line_width) = if stale {
            (
                radar::sweep_start_line_stale(),
                radar::sweep_line_stale(),
                2.0,
            )
        } else {
            (radar::sweep_start_line(), radar::SWEEP_LINE, 3.0)
        };

        // Thin line at sweep start boundary
        let start_angle_rad = (start_az - 90.0) * PI / 180.0;
        let start_end = Pos2::new(
            center.x + radius * start_angle_rad.cos(),
            center.y + radius * start_angle_rad.sin(),
        );
        painter.line_segment([center, start_end], Stroke::new(1.5, start_line_color));

        // Main sweep line
        let angle_rad = (az - 90.0) * PI / 180.0;
        let end_x = center.x + radius * angle_rad.cos();
        let end_y = center.y + radius * angle_rad.sin();
        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(sweep_line_width, sweep_line_color),
        );

        // "NOW" label at the tip of the sweep line during live streaming
        if state.live_radar_model.active {
            let label_offset = radius + 8.0;
            let label_pos = Pos2::new(
                center.x + label_offset * angle_rad.cos(),
                center.y + label_offset * angle_rad.sin(),
            );
            let align = sweep_label_align(az);
            painter.text(
                label_pos,
                align,
                "NOW",
                egui::FontId::monospace(11.0),
                Color32::from_rgb(255, 80, 80),
            );
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
        if state.live_radar_model.active {
            draw_live_sweep_donut(painter, center, radius, az, start_az, state);
        } else if state.effective_sweep_animation() {
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

    // Current sweep arc: from sweep_start CW to sweep_az
    let current_color = Color32::from_rgba_unmultiplied(80, 200, 120, 160);
    // Previous sweep arc: from sweep_az CW to sweep_start (the rest)
    let prev_color = Color32::from_rgba_unmultiplied(120, 120, 180, 120);

    // Draw arcs as series of short line segments
    let segments = 180;
    let swept_arc_deg = (sweep_az - sweep_start).rem_euclid(360.0);

    for i in 0..segments {
        let frac_start = i as f32 / segments as f32;
        let frac_end = (i + 1) as f32 / segments as f32;
        let deg_start = frac_start * 360.0;
        let deg_end = frac_end * 360.0;

        // Is this segment in the current sweep region?
        let mid_deg = (deg_start + deg_end) / 2.0;
        let is_current = mid_deg < swept_arc_deg;

        let color = if is_current {
            current_color
        } else {
            prev_color
        };

        let a1 = ((sweep_start + deg_start) - 90.0) * PI / 180.0;
        let a2 = ((sweep_start + deg_end) - 90.0) * PI / 180.0;

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

    // Color constants for boundary label text
    let current_text_color = Color32::from_rgb(100, 220, 140); // green for current sweep
    let prev_text_color = Color32::from_rgb(160, 160, 220); // purple for previous sweep

    // Time labels at both discontinuity boundaries
    let label_radius = donut_outer + 14.0;
    let label_font = egui::FontId::monospace(10.0);
    let use_local = state.use_local_time;

    // Boundary 1: sweep line (sweep_az) — playback time | prev sweep time at this azimuth
    let playback_ts = state.playback_state.playback_position();
    let mut playback_time_str = format_time_short(playback_ts, use_local);
    if let Some(age) = format_age_compact(playback_ts) {
        playback_time_str.push(' ');
        playback_time_str.push_str(&age);
    }

    let prev_at_az_str = state
        .viz_state
        .prev_sweep_overlay
        .map(|(_, prev_start, prev_end)| {
            let frac = (swept_arc_deg / 360.0).clamp(0.0, 1.0) as f64;
            let prev_time_at_az = prev_start + frac * (prev_end - prev_start);
            let mut s = format_time_short(prev_time_at_az, use_local);
            if let Some(age) = format_age_compact(prev_time_at_az) {
                s.push(' ');
                s.push_str(&age);
            }
            s
        });

    draw_boundary_label(
        painter,
        center,
        label_radius,
        sweep_az,
        &playback_time_str,
        prev_at_az_str.as_deref(),
        current_text_color,
        prev_text_color,
        &label_font,
    );

    // Boundary 2: sweep start (sweep_start) — current sweep start | prev sweep end
    // Look up the current sweep's actual start time from the timeline
    let displayed_elev = state.viz_state.displayed_sweep_elevation_number;
    let current_sweep_start_secs = state
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
                .map(|s| s.start_time)
        });

    if swept_arc_deg >= 30.0 {
        if let Some((_, _, prev_end)) = state.viz_state.prev_sweep_overlay {
            // Both sweeps: current start (green) | prev end (purple)
            let start_time_str = current_sweep_start_secs.map(|s| {
                let mut t = format_time_short(s, use_local);
                if let Some(age) = format_age_compact(s) {
                    t.push(' ');
                    t.push_str(&age);
                }
                t
            });
            let mut prev_end_str = format_time_short(prev_end, use_local);
            if let Some(age) = format_age_compact(prev_end) {
                prev_end_str.push(' ');
                prev_end_str.push_str(&age);
            }

            let (left, right, left_c, right_c) = match start_time_str {
                Some(ref start) => (
                    start.as_str(),
                    Some(prev_end_str.as_str()),
                    current_text_color,
                    prev_text_color,
                ),
                None => (
                    prev_end_str.as_str(),
                    None,
                    prev_text_color,
                    prev_text_color,
                ),
            };
            draw_boundary_label(
                painter,
                center,
                label_radius,
                sweep_start,
                left,
                right,
                left_c,
                right_c,
                &label_font,
            );
        } else if let Some(start_secs) = current_sweep_start_secs {
            // Single sweep only: show start time in green (no separator)
            let mut start_str = format_time_short(start_secs, use_local);
            if let Some(age) = format_age_compact(start_secs) {
                start_str.push(' ');
                start_str.push_str(&age);
            }
            draw_boundary_label(
                painter,
                center,
                label_radius,
                sweep_start,
                &start_str,
                None,
                current_text_color,
                current_text_color,
                &label_font,
            );
        }
    }
}

fn draw_live_sweep_donut(
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

    let model = &state.live_radar_model;
    let sweep_model = model.active_sweep.as_ref();
    let chunks = sweep_model.map(|s| s.chunks.as_slice()).unwrap_or(&[]);

    // Compute the angular extent of actual received chunk data (from sweep_start).
    // This is independent of the extrapolated sweep line — chunks stay visible until
    // the elevation actually completes, even after the sweep line wraps past 360°.
    let data_arc_deg = if chunks.is_empty() {
        0.0
    } else {
        chunks
            .iter()
            .map(|c| (c.last_az - sweep_start).rem_euclid(360.0))
            .fold(0.0f32, f32::max)
    };

    // Distinct hues for chunk wedges (up to 8 before cycling)
    let chunk_colors = [
        Color32::from_rgba_unmultiplied(70, 200, 110, 160), // green
        Color32::from_rgba_unmultiplied(80, 180, 220, 160), // cyan
        Color32::from_rgba_unmultiplied(220, 180, 70, 160), // amber
        Color32::from_rgba_unmultiplied(180, 100, 220, 160), // purple
        Color32::from_rgba_unmultiplied(220, 110, 80, 160), // coral
        Color32::from_rgba_unmultiplied(100, 220, 180, 160), // teal
        Color32::from_rgba_unmultiplied(220, 140, 180, 160), // pink
        Color32::from_rgba_unmultiplied(160, 220, 80, 160), // lime
    ];
    let prev_color = Color32::from_rgba_unmultiplied(120, 120, 180, 120);
    let boundary_color = Color32::from_rgba_unmultiplied(200, 200, 220, 180);

    // Draw arcs segment by segment
    let seg_count = 360;
    for i in 0..seg_count {
        let deg = (i as f32 + 0.5) * 360.0 / seg_count as f32;
        let abs_deg = (sweep_start + deg).rem_euclid(360.0);

        // Color by actual chunk coverage, not sweep line position.
        // This way chunks persist visually until the elevation completes,
        // even when the sweep line extrapolation wraps past 360°.
        let color = {
            let chunk_match = chunks.iter().position(|c| {
                let arc = (c.last_az - c.first_az).rem_euclid(360.0);
                let from_first = (abs_deg - c.first_az).rem_euclid(360.0);
                from_first <= arc
            });
            if let Some(idx) = chunk_match {
                chunk_colors[idx % chunk_colors.len()]
            } else {
                prev_color
            }
        };

        let a1 = ((sweep_start + deg - 0.5) - 90.0) * PI / 180.0;
        let a2 = ((sweep_start + deg + 0.5) - 90.0) * PI / 180.0;
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

    // Draw thin boundary lines between chunks + time labels at boundaries
    let use_local = state.use_local_time;
    let boundary_label_radius = donut_outer + 14.0;
    let boundary_label_font = egui::FontId::monospace(9.0);
    let boundary_time_color = Color32::from_rgb(180, 180, 200);

    for (chunk_idx, c) in chunks
        .iter()
        .take(chunks.len().saturating_sub(1))
        .enumerate()
    {
        let a = (c.last_az - 90.0) * PI / 180.0;
        let p_inner = Pos2::new(
            center.x + donut_inner * a.cos(),
            center.y + donut_inner * a.sin(),
        );
        let p_outer = Pos2::new(
            center.x + donut_outer * a.cos(),
            center.y + donut_outer * a.sin(),
        );
        painter.line_segment([p_inner, p_outer], Stroke::new(1.5, boundary_color));

        // Time label at boundary: find the end time for this chunk from pre-filtered spans
        let boundary_time = sweep_model
            .and_then(|s| s.chunk_time_spans.get(chunk_idx))
            .map(|&(_, end, _)| end);
        if let Some(end_secs) = boundary_time {
            let time_str = format_time_short(end_secs, use_local);
            draw_boundary_label(
                painter,
                center,
                boundary_label_radius,
                c.last_az,
                &time_str,
                None,
                boundary_time_color,
                boundary_time_color,
                &boundary_label_font,
            );
        }
    }

    // Labels
    let label_radius = donut_outer + 14.0;
    let label_font = egui::FontId::monospace(10.0);
    let current_label_color = Color32::from_rgb(100, 220, 140);
    let prev_label_color = Color32::from_rgb(160, 160, 220);

    // Helper: look up elevation angle from VCP data by elevation number
    let volume = model.volume.as_ref();
    let elev_angle_str = |elev_num: u8| -> String {
        volume
            .and_then(|v| v.vcp_pattern.as_ref())
            .and_then(|vcp| {
                vcp.elevations
                    .get(elev_num.saturating_sub(1) as usize)
                    .map(|el| format!("{:.1}\u{00B0}", el.angle))
            })
            .unwrap_or_default()
    };

    // Per-chunk labels (only when chunks have enough angular separation)
    let expected_chunks = sweep_model.and_then(|s| s.chunks_expected);
    if chunks.len() > 1 {
        let elev_num = sweep_model
            .map(|s| format!("{}", s.elevation_number))
            .unwrap_or_else(|| "?".to_string());
        let expected_str = expected_chunks
            .map(|n| format!("/{}", n))
            .unwrap_or_default();
        for (i, c) in chunks.iter().enumerate() {
            let arc = (c.last_az - c.first_az).rem_euclid(360.0);
            if arc < 15.0 {
                continue; // too narrow for a label
            }
            let mid_az = c.first_az + arc / 2.0;
            let label = format!(
                "S{} {}{} \u{00B7} {:.0}\u{00B0}\u{2013}{:.0}\u{00B0}",
                elev_num,
                i + 1,
                expected_str,
                c.first_az,
                c.last_az
            );
            draw_boundary_label(
                painter,
                center,
                label_radius,
                mid_az,
                &label,
                None,
                current_label_color,
                current_label_color,
                &label_font,
            );
        }
    }

    // Overall sweep info label at the midpoint of all current data
    {
        let elev_num = sweep_model
            .map(|s| format!("{}", s.elevation_number))
            .unwrap_or_else(|| "?".to_string());
        let elev_angle = sweep_model
            .map(|s| elev_angle_str(s.elevation_number))
            .unwrap_or_default();
        let radials = sweep_model.map(|s| s.radials_received).unwrap_or(0);
        let completed = volume.map(|v| v.elevations_complete.len()).unwrap_or(0);
        let expected = volume
            .and_then(|v| v.elevations_expected)
            .map(|n| format!("/{}", n))
            .unwrap_or_default();

        // Place at sweep line position (the leading edge)
        let label = format!(
            "Sweep {} {} \u{00B7} {}r \u{00B7} {}{} elev",
            elev_num, elev_angle, radials, completed, expected
        );
        draw_boundary_label(
            painter,
            center,
            label_radius,
            sweep_az,
            &label,
            None,
            current_label_color,
            current_label_color,
            &label_font,
        );
    }

    // Previous sweep label at the midpoint of the purple (non-chunk) arc
    let prev_arc_deg = 360.0 - data_arc_deg;
    if prev_arc_deg > 30.0 {
        let prev_elev = volume.and_then(|v| v.elevations_complete.last().copied());
        if let Some(pe) = prev_elev {
            let label = format!("Prev sweep {} {}", pe, elev_angle_str(pe));
            let angle = sweep_start + data_arc_deg + prev_arc_deg / 2.0;
            draw_boundary_label(
                painter,
                center,
                label_radius,
                angle,
                &label,
                None,
                prev_label_color,
                prev_label_color,
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
