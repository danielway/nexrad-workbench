//! Sweep track rendering: sweep blocks (cool palette) and connector lines.

use super::TimelineFrame;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Painter, Pos2, Rect, Stroke, StrokeKind};

/// Render sweep blocks on the sweep track (cool indigo-to-cyan palette).
/// Only called at [`super::DetailLevel::Tilts`] (when the sweep rect exists).
pub(super) fn render_sweep_track(painter: &Painter, frame: &TimelineFrame<'_>) {
    let Some(rect) = frame.rects.sweep.as_ref() else {
        return;
    };
    let view = &frame.view;
    let (active_sweep, prev_active_sweep) = (frame.active_sweep, frame.prev_active_sweep);
    let selected_elevation_number = view.elevation_filter();
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);

    for (scan, _) in view.settled_scans_in_range(frame.view_start, frame.view_end) {
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
                scan.key_timestamp == scan_ts && sweep.elevation_number == elev_num
            });
            let is_prev_active = !is_active
                && prev_active_sweep.is_some_and(|(scan_ts, elev_num)| {
                    scan.key_timestamp == scan_ts && sweep.elevation_number == elev_num
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

            // Elevation + product labels — show the VCP target angle so the
            // label reads as the cut's identity (e.g. 0.5°), not the
            // encoder average that drifts a few hundredths per spin.
            if width > 25.0 {
                let display_angle = scan.display_angle(sweep);
                let mut label = if width > 60.0 {
                    format!("E{} {:.1}\u{00B0}", sweep.elevation_number, display_angle)
                } else {
                    format!("{:.1}", display_angle)
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
                    super::style::block_font(),
                    tl_colors::block_label(),
                );
            }
        }
    }
}

/// Draw thin connector lines from scan boundaries into the sweep track.
pub(super) fn render_connector_lines(painter: &Painter, frame: &TimelineFrame<'_>) {
    let scan_rect = &frame.rects.scan;
    let Some(sweep_rect) = frame.rects.sweep.as_ref() else {
        return;
    };
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);

    for (scan, clamped_end) in frame
        .view
        .visual_scans_in_range(frame.view_start, frame.view_end)
    {
        if scan.sweeps.is_empty() {
            continue;
        }
        for ts in [scan.start_time, clamped_end] {
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
