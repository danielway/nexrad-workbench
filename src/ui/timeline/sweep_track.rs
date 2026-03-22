//! Sweep track rendering: sweep blocks (cool palette) and connector lines.

use crate::state::radar_data::RadarTimeline;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, StrokeKind};

/// Render sweep blocks on the sweep track (cool indigo-to-cyan palette).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_sweep_track(
    painter: &Painter,
    rect: &Rect,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    active_sweep: Option<(i64, u8)>,
    selected_elevation_number: Option<u8>,
    active_scan_key_ts: Option<f64>,
    prev_active_sweep: Option<(i64, u8)>,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    for scan in timeline.scans_in_range(view_start, view_end) {
        // Skip the scan that corresponds to the active real-time volume —
        // render_realtime_progress owns all sweeps for the in-progress volume.
        if let Some(key_ts) = active_scan_key_ts {
            if (scan.key_timestamp - key_ts).abs() < 0.5 {
                continue;
            }
        }
        if scan.sweeps.is_empty() {
            continue;
        }

        let vcp_elevations = scan.vcp_pattern.as_ref().map(|v| &v.elevations);

        for sweep in scan.sweeps.iter() {
            let x_start = ts_to_x(sweep.start_time).max(rect.left());
            let x_end = ts_to_x(sweep.end_time).min(rect.right());
            let width = x_end - x_start;

            if width < 0.5 {
                continue;
            }

            let matches_elevation =
                selected_elevation_number.is_none_or(|num| sweep.elevation_number == num);
            let is_active = active_sweep.is_some_and(|(scan_ts, elev_num)| {
                scan.key_timestamp as i64 == scan_ts && sweep.elevation_number == elev_num
            });
            let is_prev_active = !is_active
                && prev_active_sweep.is_some_and(|(scan_ts, elev_num)| {
                    scan.key_timestamp as i64 == scan_ts && sweep.elevation_number == elev_num
                });

            let fill = tl_colors::sweep_fill(sweep.elevation, matches_elevation);
            let border = if is_prev_active {
                tl_colors::PREV_ACTIVE_SWEEP
            } else {
                tl_colors::sweep_border(sweep.elevation, is_active)
            };

            let sweep_rect = Rect::from_min_max(
                Pos2::new(x_start, rect.top() + 2.0),
                Pos2::new(x_end, rect.bottom() - 2.0),
            );

            painter.rect_filled(sweep_rect, 1.0, fill);

            if width > 3.0 {
                let stroke_width = if is_active {
                    2.0
                } else if is_prev_active {
                    1.5
                } else {
                    0.5
                };
                let stroke_kind = if is_active || is_prev_active {
                    StrokeKind::Outside
                } else {
                    StrokeKind::Inside
                };
                painter.rect_stroke(
                    sweep_rect,
                    1.0,
                    Stroke::new(stroke_width, border),
                    stroke_kind,
                );
            }

            // Elevation + product labels
            if width > 25.0 {
                let mut label = if width > 60.0 {
                    format!("E{} {:.1}\u{00B0}", sweep.elevation_number, sweep.elevation)
                } else {
                    format!("{:.1}", sweep.elevation)
                };

                if width > 80.0 {
                    if let Some(elevs) = vcp_elevations {
                        if let Some(vcp_elev) =
                            elevs.get(sweep.elevation_number.saturating_sub(1) as usize)
                        {
                            let products = match vcp_elev.waveform.as_str() {
                                "CS" | "ContiguousSurveillance" => "R",
                                "CDW"
                                | "CDWO"
                                | "ContiguousDopplerWithGating"
                                | "ContiguousDopplerWithoutGating" => "V",
                                "B" | "Batch" => "R/V",
                                "SPP" | "StaggeredPulsePair" => "R/V/D",
                                _ => "",
                            };
                            if !products.is_empty() {
                                label.push_str(&format!(" {}", products));
                            }
                        }
                    }
                }

                painter.text(
                    sweep_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::monospace(8.0),
                    Color32::from_rgba_unmultiplied(220, 230, 255, 180),
                );
            }
        }
    }
}

/// Draw thin connector lines from scan boundaries into the sweep track.
pub(super) fn render_connector_lines(
    painter: &Painter,
    scan_rect: &Rect,
    sweep_rect: &Rect,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
) {
    let ts_to_x = |ts: f64| -> f32 { scan_rect.left() + ((ts - view_start) * zoom) as f32 };

    for scan in timeline.scans_in_range(view_start, view_end) {
        if scan.sweeps.is_empty() {
            continue;
        }
        for ts in [scan.start_time, scan.end_time] {
            let x = ts_to_x(ts);
            if x >= scan_rect.left() && x <= scan_rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(x, scan_rect.bottom()),
                        Pos2::new(x, sweep_rect.top() + 2.0),
                    ],
                    Stroke::new(0.5, tl_colors::connector()),
                );
            }
        }
    }
}
