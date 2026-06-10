//! Scan track rendering: cached (on-device) scan blocks and available
//! (in-archive, not downloaded) blocks.
//!
//! Color on this track answers one question — is the data on the device?
//! Solid steel blue = cached; hollow dashed slate = available in the cloud
//! archive. VCP identity and sweep counts are carried by the block label
//! and the tooltip, not by hue.

use super::strokes::{stroke_dashed_rect, DashedBorder};
use super::DetailLevel;
use crate::data::ScanCompleteness;
use crate::state::TimelineView;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Painter, Pos2, Rect, Stroke, StrokeKind};

/// Draw an "available in the cloud archive" block: a faint wash with a
/// dashed border — an empty container the same shape as the solid cached
/// blocks — plus a cloud glyph when there's room for it.
pub(super) fn draw_available_block(painter: &Painter, block: Rect, dark: bool) {
    painter.rect_filled(block, 2.0, tl_colors::available_fill(dark));
    stroke_dashed_rect(
        painter,
        block,
        DashedBorder::uniform(
            Stroke::new(1.0, tl_colors::available_border(dark)),
            4.0,
            7.0,
        ),
    );
    if block.width() >= 24.0 {
        painter.text(
            block.center(),
            egui::Align2::CENTER_CENTER,
            egui_phosphor::regular::CLOUD_ARROW_DOWN,
            egui::FontId::proportional(11.0),
            tl_colors::available_glyph(dark),
        );
    }
}

/// Render scan blocks on the scan track.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_scan_track(
    painter: &Painter,
    rect: &Rect,
    view: &TimelineView<'_>,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
    dark: bool,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    match detail_level {
        DetailLevel::Solid => {
            // Draw solid regions for each contiguous time range
            for range in view.cache().time_ranges() {
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
                        tl_colors::cached_fill(dark, false),
                    );
                }
            }
        }
        DetailLevel::Scans | DetailLevel::Sweeps => {
            for (scan, clamped_end) in view.settled_scans_in_range(view_start, view_end) {
                let x_start = ts_to_x(scan.start_time).max(rect.left());
                let x_end = ts_to_x(clamped_end).min(rect.right());
                let width = x_end - x_start;

                if width < 1.0 {
                    continue;
                }

                let scan_rect = Rect::from_min_max(
                    Pos2::new(x_start, rect.top() + 2.0),
                    Pos2::new(x_end, rect.bottom() - 2.0),
                );

                // A scan with no cached sweeps at all is semantically
                // "available, not downloaded" — render it in that style.
                if scan.completeness == Some(ScanCompleteness::Missing) {
                    draw_available_block(painter, scan_rect, dark);
                    continue;
                }

                let partial = matches!(
                    scan.completeness,
                    Some(ScanCompleteness::PartialWithVcp | ScanCompleteness::PartialNoVcp)
                );
                painter.rect_filled(scan_rect, 2.0, tl_colors::cached_fill(dark, partial));
                painter.rect_stroke(
                    scan_rect,
                    2.0,
                    Stroke::new(1.0, tl_colors::cached_border(dark, partial)),
                    StrokeKind::Inside,
                );

                // Block label: VCP identity plus cached/planned sweep count.
                // Wide blocks spell out "VCP"; narrow ones show the number,
                // with the count kept only when it carries information
                // (i.e. the scan is partial).
                if width > 60.0 && scan.vcp > 0 {
                    let counts = match (scan.cached_sweep_count, scan.planned_sweep_count) {
                        (Some(p), Some(e)) if e > 0 => Some((p, e)),
                        _ => None,
                    };
                    let is_partial_count = counts.is_some_and(|(p, e)| p < e);
                    let label = if width > 110.0 {
                        match counts {
                            Some((p, e)) => format!("VCP {} \u{00B7} {}/{}", scan.vcp, p, e),
                            None => format!("VCP {}", scan.vcp),
                        }
                    } else if let (true, Some((p, e))) = (is_partial_count, counts) {
                        format!("{} \u{00B7} {}/{}", scan.vcp, p, e)
                    } else {
                        format!("{}", scan.vcp)
                    };
                    painter.text(
                        scan_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        super::style::block_font(),
                        tl_colors::block_label(),
                    );
                }
            }
        }
    }
}

/// Render shadow scan boundaries from the archive index as first-class
/// "available, not downloaded" blocks. The [`TimelineView`] supplies the
/// dedup against already-downloaded scans.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_shadow_boundaries(
    painter: &Painter,
    rect: &Rect,
    view: &TimelineView<'_>,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
    dark: bool,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    let view_start_i64 = view_start as i64;
    let view_end_i64 = view_end as i64;

    match detail_level {
        DetailLevel::Solid => {
            // At solid detail, merge all visible shadow boundaries into contiguous regions
            let visible: Vec<_> = view
                .shadow_boundaries()
                .iter()
                .filter(|b| !view.is_covered_by_cached(b.start))
                .filter(|b| b.end > view_start_i64 && b.start < view_end_i64)
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
                    draw_available_block(
                        painter,
                        Rect::from_min_max(
                            Pos2::new(x_start, rect.top() + 2.0),
                            Pos2::new(x_end, rect.bottom() - 2.0),
                        ),
                        dark,
                    );
                }
            }
        }
        DetailLevel::Scans | DetailLevel::Sweeps => {
            for b in view
                .shadow_boundaries()
                .iter()
                .filter(|b| !view.is_covered_by_cached(b.start))
            {
                // Skip if outside visible range
                if b.end <= view_start_i64 || b.start >= view_end_i64 {
                    continue;
                }

                let x_start = ts_to_x(b.start as f64).max(rect.left());
                let x_end = ts_to_x(b.end as f64).min(rect.right());
                let width = x_end - x_start;

                if width < 1.0 {
                    continue;
                }

                draw_available_block(
                    painter,
                    Rect::from_min_max(
                        Pos2::new(x_start, rect.top() + 2.0),
                        Pos2::new(x_end, rect.bottom() - 2.0),
                    ),
                    dark,
                );
            }
        }
    }
}
