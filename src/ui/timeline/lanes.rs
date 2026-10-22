//! The other sites' lanes under the main track: one 11 px row per site but
//! the focus, in list order, each a 1:1 projection of a
//! [`crate::core::timeline_view::LaneVm`]. Held volumes are solid, listed
//! ones hollow, the live volume solid up to its newest chunk with a dashed
//! outline to its projected end (red when the stream stalled), a failed
//! download hollow with a red edge. The site id sits at the left edge over a
//! dark backing.

use super::strokes::{stroke_dashed_rect, DashedBorder};
use super::style::{LANE_GAP, LANE_H};
use super::TimelineFrame;
use crate::core::timeline_view::{LaneBlock, LaneBlockState, LaneVm};
use crate::ui::theme;
use crate::ui::theme::timeline as tl_colors;
use eframe::egui::{FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

/// The lane rects under `track`, one per lane, top to bottom.
pub(super) fn lane_rects(track: Rect, lanes: usize) -> Vec<Rect> {
    (0..lanes)
        .map(|i| {
            let top = track.bottom() + LANE_GAP + i as f32 * (LANE_H + LANE_GAP);
            Rect::from_min_max(
                Pos2::new(track.left(), top),
                Pos2::new(track.right(), top + LANE_H),
            )
        })
        .collect()
}

/// Paint every lane into its rect.
pub(super) fn paint_lanes(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    lanes: &[LaneVm],
    rects: &[Rect],
) {
    for (lane, rect) in lanes.iter().zip(rects) {
        let painter = painter.with_clip_rect(*rect);
        for block in &lane.blocks {
            paint_block(&painter, frame, block, *rect);
        }
        paint_label(&painter, &lane.site, *rect);
    }
}

/// The x span of `[a, b]` on the lane, or `None` when it is off-screen.
fn span(frame: &TimelineFrame<'_>, rect: Rect, a: f64, b: f64) -> Option<(f32, f32)> {
    let x0 = frame.ts_to_x(a).max(rect.left() - 2.0);
    let x1 = frame.ts_to_x(b).min(rect.right() + 2.0);
    (x1 > x0 && x1 >= rect.left() && x0 <= rect.right()).then_some((x0, x1))
}

fn paint_block(painter: &Painter, frame: &TimelineFrame<'_>, block: &LaneBlock, rect: Rect) {
    let Some((x0, x1)) = span(frame, rect, block.start_secs, block.end_secs) else {
        return;
    };
    let body = |x0: f32, x1: f32| {
        Rect::from_min_max(Pos2::new(x0, rect.top()), Pos2::new(x1, rect.bottom()))
    };
    // One pixel between neighbouring volumes so they read as separate.
    let whole = body(x0, (x1 - 1.0).max(x0 + 1.0));
    match block.state {
        LaneBlockState::Held => {
            painter.rect_filled(whole, 2.0, tl_colors::cached_fill(false));
        }
        LaneBlockState::Listed => {
            painter.rect_filled(whole, 2.0, tl_colors::available_fill());
            painter.rect_stroke(
                whole,
                2.0,
                Stroke::new(1.0_f32, tl_colors::available_border()),
                StrokeKind::Inside,
            );
        }
        LaneBlockState::Failed => {
            painter.rect_filled(whole, 2.0, tl_colors::available_fill());
            painter.rect_stroke(
                whole,
                2.0,
                Stroke::new(1.0_f32, theme::acquisition::FAILED),
                StrokeKind::Inside,
            );
        }
        LaneBlockState::LiveFilling { newest_secs } | LaneBlockState::Stalled { newest_secs } => {
            let split = frame.ts_to_x(newest_secs).clamp(x0, x1);
            if split > x0 {
                painter.rect_filled(body(x0, split), 2.0, tl_colors::cached_fill(false));
            }
            if x1 > split {
                let color = if matches!(block.state, LaneBlockState::Stalled { .. }) {
                    theme::acquisition::FAILED
                } else {
                    tl_colors::cell_projected_border()
                };
                stroke_dashed_rect(
                    painter,
                    body(split, x1).shrink(0.5),
                    DashedBorder::uniform(Stroke::new(1.0_f32, color), 3.0, 5.0),
                );
            }
        }
    }
}

/// The site id at the lane's left edge, over a dark backing so the blocks
/// under it stay legible as the view pans.
fn paint_label(painter: &Painter, site: &str, rect: Rect) {
    let galley = painter.layout_no_wrap(site.to_string(), FontId::monospace(8.5), theme::TEXT_DIM);
    let size = galley.size() + Vec2::new(8.0, 0.0);
    let backing = Rect::from_min_size(
        Pos2::new(rect.left() + 2.0, rect.center().y - size.y / 2.0),
        size,
    );
    painter.rect_filled(backing, 0.0, theme::alpha(theme::PANEL, 217));
    painter.galley(backing.min + Vec2::new(4.0, 0.0), galley, theme::TEXT_DIM);
}

/// The lane and block under `pos`, for the hover.
pub(super) fn hovered<'l>(
    lanes: &'l [LaneVm],
    rects: &[Rect],
    pos: Pos2,
    ts: f64,
) -> Option<(&'l LaneVm, &'l LaneBlock)> {
    let i = rects.iter().position(|r| r.contains(pos))?;
    let lane = lanes.get(i)?;
    lane.block_at(ts).map(|b| (lane, b))
}
