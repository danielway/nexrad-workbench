//! Scan track rendering: scan blocks (warm palette) and shadow boundaries.

use super::DetailLevel;
use crate::data::ScanCompleteness;
use crate::state::radar_data::RadarTimeline;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, StrokeKind};

/// Render scan blocks on the scan track (warm palette, VCP-based colors).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_scan_track(
    painter: &Painter,
    rect: &Rect,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
    active_scan_key_ts: Option<f64>,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    match detail_level {
        DetailLevel::Solid => {
            // Draw solid regions for each contiguous time range
            for range in timeline.time_ranges() {
                let x_start = ts_to_x(range.start).max(rect.left());
                let x_end = ts_to_x(range.end).min(rect.right());

                // Enforce minimum visual width for sub-pixel data regions
                let x_end = if (x_end - x_start) > 0.0 && (x_end - x_start) < 8.0 {
                    (x_start + 8.0).min(rect.right())
                } else {
                    x_end
                };

                if x_end > x_start {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(x_start, rect.top() + 2.0),
                            Pos2::new(x_end, rect.bottom() - 2.0),
                        ),
                        2.0,
                        tl_colors::scan_fill(0, None),
                    );
                }
            }
        }
        DetailLevel::Scans | DetailLevel::Sweeps => {
            for scan in timeline.scans_in_range(view_start, view_end) {
                // Skip the scan that corresponds to the active real-time volume —
                // render_realtime_progress draws it with received/projected styling.
                if let Some(key_ts) = active_scan_key_ts {
                    if (scan.key_timestamp - key_ts).abs() < 0.5 {
                        continue;
                    }
                }
                let x_start = ts_to_x(scan.start_time).max(rect.left());
                let x_end = ts_to_x(scan.end_time).min(rect.right());
                let width = x_end - x_start;

                if width < 1.0 {
                    continue;
                }

                let scan_rect = Rect::from_min_max(
                    Pos2::new(x_start, rect.top() + 2.0),
                    Pos2::new(x_end, rect.bottom() - 2.0),
                );

                let fill = tl_colors::scan_fill(scan.vcp, scan.completeness);
                let border = tl_colors::scan_border(scan.vcp, scan.completeness);

                // Missing: outline only with dashed effect
                if scan.completeness == Some(ScanCompleteness::Missing) {
                    painter.rect_stroke(
                        scan_rect,
                        2.0,
                        Stroke::new(1.0, border),
                        StrokeKind::Inside,
                    );
                } else {
                    painter.rect_filled(scan_rect, 2.0, fill);
                    painter.rect_stroke(
                        scan_rect,
                        2.0,
                        Stroke::new(1.0, border),
                        StrokeKind::Inside,
                    );

                    // Hatch pattern for PartialWithVcp
                    if scan.completeness == Some(ScanCompleteness::PartialWithVcp) {
                        let hatch_color = tl_colors::scan_hatch(scan.vcp);
                        let spacing = 6.0;
                        let h = scan_rect.height();
                        // Use global x-coordinate phase so hatch lines are parallel across all blocks
                        let phase = scan_rect.left() % spacing;
                        let mut offset = -phase;
                        while offset < width + h {
                            // Unclipped 45° diagonal: top to bottom, shifting left by h
                            let x0 = scan_rect.left() + offset;
                            let x1 = x0 - h;
                            // Clip to rect: adjust y when x is clamped to preserve angle
                            let (cx0, cy0) = if x0 > scan_rect.right() {
                                (
                                    scan_rect.right(),
                                    scan_rect.top() + (x0 - scan_rect.right()),
                                )
                            } else {
                                (x0, scan_rect.top())
                            };
                            let (cx1, cy1) = if x1 < scan_rect.left() {
                                (
                                    scan_rect.left(),
                                    scan_rect.bottom() - (scan_rect.left() - x1),
                                )
                            } else {
                                (x1, scan_rect.bottom())
                            };
                            if cy0 < cy1 {
                                painter.line_segment(
                                    [Pos2::new(cx0, cy0), Pos2::new(cx1, cy1)],
                                    Stroke::new(0.5, hatch_color),
                                );
                            }
                            offset += spacing;
                        }
                    }

                    // PartialNoVcp: draw dashed border on top of filled rect
                    if scan.completeness == Some(ScanCompleteness::PartialNoVcp) {
                        // Already drew solid border above; the reduced alpha handles visual distinction
                    }
                }

                // Single combined label: "VCP 212 15/17" — centered in block
                // Only show when the block is wide enough to avoid overlap with
                // neighboring blocks and time tick labels.
                if width > 60.0 && scan.vcp > 0 {
                    let is_partial = matches!(
                        (scan.present_records, scan.expected_records),
                        (Some(p), Some(e)) if e > 0 && p < e
                    );
                    let label = if is_partial {
                        let (p, e) = (
                            scan.present_records.unwrap(),
                            scan.expected_records.unwrap(),
                        );
                        if width > 120.0 {
                            format!("VCP {} {}/{}", scan.vcp, p, e)
                        } else {
                            format!("{} {}/{}", scan.vcp, p, e)
                        }
                    } else if width > 100.0 {
                        let elev_count = scan
                            .vcp_pattern
                            .as_ref()
                            .map(|v| v.elevations.len())
                            .unwrap_or(scan.sweeps.len());
                        if elev_count > 0 {
                            format!("VCP {} ({})", scan.vcp, elev_count)
                        } else {
                            format!("VCP {}", scan.vcp)
                        }
                    } else {
                        format!("{}", scan.vcp)
                    };
                    painter.text(
                        scan_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::monospace(7.0),
                        Color32::from_rgba_unmultiplied(220, 220, 240, 180),
                    );
                }
            }
        }
    }
}

/// Render shadow scan boundaries from the archive index.
///
/// These are subtle markers showing where scans exist in the archive before
/// they are downloaded. Boundaries that overlap already-downloaded scans are
/// skipped so only un-downloaded positions are highlighted.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_shadow_boundaries(
    painter: &Painter,
    rect: &Rect,
    boundaries: &[crate::nexrad::ScanBoundary],
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    let view_start_i64 = view_start as i64;
    let view_end_i64 = view_end as i64;

    match detail_level {
        DetailLevel::Solid => {
            // At solid detail, merge all visible shadow boundaries into contiguous regions
            let visible: Vec<_> = boundaries
                .iter()
                .filter(|b| b.end > view_start_i64 && b.start < view_end_i64)
                .filter(|b| {
                    !timeline
                        .scans
                        .iter()
                        .any(|s| (s.key_timestamp as i64 - b.start).abs() < 60)
                })
                .collect();

            if visible.is_empty() {
                return;
            }

            // Merge into contiguous regions (gap < 15 min)
            let mut regions: Vec<(i64, i64)> = Vec::new();
            for b in &visible {
                if let Some(last) = regions.last_mut() {
                    if b.start - last.1 < 900 {
                        last.1 = b.end;
                        continue;
                    }
                }
                regions.push((b.start, b.end));
            }

            for (start, end) in regions {
                let x_start = ts_to_x(start as f64).max(rect.left());
                let x_end = ts_to_x(end as f64).min(rect.right());
                let x_end = if (x_end - x_start) > 0.0 && (x_end - x_start) < 8.0 {
                    (x_start + 8.0).min(rect.right())
                } else {
                    x_end
                };
                if x_end > x_start {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(x_start, rect.top() + 2.0),
                            Pos2::new(x_end, rect.bottom() - 2.0),
                        ),
                        2.0,
                        tl_colors::shadow_fill(),
                    );
                }
            }
        }
        DetailLevel::Scans | DetailLevel::Sweeps => {
            for b in boundaries {
                // Skip if outside visible range
                if b.end <= view_start_i64 || b.start >= view_end_i64 {
                    continue;
                }
                // Skip if this scan is already downloaded (within 60s tolerance)
                if timeline
                    .scans
                    .iter()
                    .any(|s| (s.key_timestamp as i64 - b.start).abs() < 60)
                {
                    continue;
                }

                let x_start = ts_to_x(b.start as f64).max(rect.left());
                let x_end = ts_to_x(b.end as f64).min(rect.right());
                let width = x_end - x_start;

                if width < 1.0 {
                    continue;
                }

                let shadow_rect = Rect::from_min_max(
                    Pos2::new(x_start, rect.top() + 2.0),
                    Pos2::new(x_end, rect.bottom() - 2.0),
                );

                painter.rect_filled(shadow_rect, 2.0, tl_colors::shadow_fill());
                painter.rect_stroke(
                    shadow_rect,
                    2.0,
                    Stroke::new(0.5, tl_colors::shadow_border()),
                    StrokeKind::Inside,
                );
            }
        }
    }
}
