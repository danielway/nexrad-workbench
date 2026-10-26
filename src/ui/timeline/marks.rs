//! Strip marks: the states a cell can't show for lack of room, in both
//! tiers (one-instrument §5 "Zoom and tiers"). A thin projection of
//! [`crate::core::timeline_view::strip_marks`], which decides what is a mark
//! and where it sits:
//!   - Failed    → the ⚠ glyph in text color (never red); click retries
//!   - Queued    → a small hatched box
//!   - Projected → a small dashed box under a tilde
//!   - Filling   → a small slate box filled to the received fraction
//!
//! Marks sit in the top band of the main track, over cells and blocks alike.

use super::strokes::{fill_hatched_rect, stroke_dashed_rect, DashedBorder};
use super::TimelineFrame;
use crate::core::timeline_view::{StripMark, StripMarkKind};
use crate::ui::theme::timeline as tl_colors;
use eframe::egui::{self, Painter, Pos2, Rect, Stroke, StrokeKind};

/// A failure mark's clickable rect and the scan-starts it retries.
pub(super) struct FailedTick {
    pub rect: Rect,
    pub keys: Vec<f64>,
}

/// The narrowest a box mark draws (px).
const MIN_MARK_W: f32 = 6.0;
/// Height of a box mark.
const MARK_H: f32 = 7.0;
/// Size of the ⚠ glyph.
const GLYPH_PX: f32 = 11.0;

/// Paint the marks; return the failure marks' hit rects for the retry.
pub(super) fn paint_strip_marks(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    marks: &[StripMark],
    hover: Option<Pos2>,
) -> Vec<FailedTick> {
    let track = frame.rects.scan;
    let top = track.top() + 3.0;
    let mut ticks = Vec::new();
    for m in marks {
        let x0 = frame.ts_to_x(m.start_secs);
        let x1 = frame.ts_to_x(m.end_secs);
        let cx = frame.ts_to_x(m.center_secs());
        if x1 < track.left() - GLYPH_PX || x0 > track.right() + GLYPH_PX {
            continue;
        }
        let half = ((x1 - x0) / 2.0).max(MIN_MARK_W / 2.0);
        let boxed = Rect::from_min_max(
            Pos2::new((cx - half).max(track.left()), top),
            Pos2::new((cx + half).min(track.right()), top + MARK_H),
        );
        match m.kind {
            StripMarkKind::Failed => {
                let hit = Rect::from_center_size(
                    Pos2::new(cx.clamp(track.left() + 6.0, track.right() - 6.0), top + 5.0),
                    egui::vec2(GLYPH_PX + 4.0, GLYPH_PX + 2.0),
                );
                let color = if hover.is_some_and(|h| hit.contains(h)) {
                    tl_colors::FAILURE_GLYPH_HOVER
                } else {
                    tl_colors::FAILURE_GLYPH
                };
                // A dark backdrop keeps the glyph legible over a cell.
                painter.rect_filled(hit.shrink(1.0), 2.0, tl_colors::block_label_backdrop());
                painter.text(
                    hit.center(),
                    egui::Align2::CENTER_CENTER,
                    crate::ui::icons::WARNING,
                    egui::FontId::proportional(GLYPH_PX),
                    color,
                );
                ticks.push(FailedTick {
                    rect: hit,
                    keys: m.keys.clone(),
                });
            }
            StripMarkKind::Queued => {
                fill_hatched_rect(
                    painter,
                    boxed,
                    Stroke::new(0.75_f32, tl_colors::cell_queued_hatch()),
                    3.0,
                );
                painter.rect_stroke(
                    boxed,
                    1.0,
                    Stroke::new(0.75_f32, tl_colors::cell_queued_hatch()),
                    StrokeKind::Inside,
                );
            }
            StripMarkKind::Projected => {
                painter.rect_filled(boxed, 1.0, tl_colors::cell_projected_fill());
                stroke_dashed_rect(
                    painter,
                    boxed,
                    DashedBorder::uniform(
                        Stroke::new(0.75_f32, tl_colors::cell_projected_border()),
                        2.0,
                        4.0,
                    ),
                );
                painter.text(
                    Pos2::new(boxed.center().x, boxed.bottom() + 1.0),
                    egui::Align2::CENTER_TOP,
                    "~",
                    super::style::block_font(),
                    tl_colors::cell_projected_label(),
                );
            }
            StripMarkKind::Filling { progress } => {
                painter.rect_stroke(
                    boxed,
                    1.0,
                    Stroke::new(1.0_f32, tl_colors::cell_inflight_border()),
                    StrokeKind::Inside,
                );
                let frac = progress.unwrap_or(0.5).clamp(0.0, 1.0);
                if frac > 0.0 {
                    let fill = Rect::from_min_max(
                        boxed.min,
                        Pos2::new(boxed.left() + boxed.width() * frac, boxed.bottom()),
                    );
                    painter.rect_filled(fill, 1.0, tl_colors::cell_inflight());
                }
            }
        }
    }
    ticks
}
