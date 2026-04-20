//! Batched helpers for dashed borders and diagonal hatches.
//!
//! The timeline overlays draw a lot of dotted/dashed/hatched rectangles
//! — one per visible sweep, per frame. Each `painter.line_segment` call
//! locks the egui graphics buffer, so emitting ~50 individual segments
//! per border per sweep was showing up in idle-frame profiles. These
//! helpers pre-size a `Vec<Shape>` and push everything in one
//! `painter.extend` call, amortizing the lock and reducing paint-list
//! churn.

use eframe::egui::{Painter, Pos2, Rect, Shape, Stroke};

/// Which edges of a rectangle should receive dashes.
#[derive(Clone, Copy, Debug)]
pub(super) struct DashedEdges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl DashedEdges {
    pub(super) const ALL: Self = Self {
        top: true,
        bottom: true,
        left: true,
        right: true,
    };
    pub(super) const HORIZONTAL: Self = Self {
        top: true,
        bottom: true,
        left: false,
        right: false,
    };
}

/// Parameters for dashing a rectangle's border.
///
/// Horizontal edges (top/bottom) and vertical edges (left/right) each
/// have their own dash length and period so the overlay module can keep
/// its original visual: 4-on-4-off horizontally, 3-on-3-off vertically,
/// regardless of the block's aspect ratio.
#[derive(Clone, Copy, Debug)]
pub(super) struct DashedBorder {
    pub stroke: Stroke,
    pub h_dash: f32,
    pub h_period: f32,
    pub v_dash: f32,
    pub v_period: f32,
    pub edges: DashedEdges,
}

impl DashedBorder {
    pub(super) fn uniform(stroke: Stroke, dash: f32, period: f32) -> Self {
        Self {
            stroke,
            h_dash: dash,
            h_period: period,
            v_dash: dash,
            v_period: period,
            edges: DashedEdges::ALL,
        }
    }

    pub(super) fn rect(
        stroke: Stroke,
        h_dash: f32,
        h_period: f32,
        v_dash: f32,
        v_period: f32,
    ) -> Self {
        Self {
            stroke,
            h_dash,
            h_period,
            v_dash,
            v_period,
            edges: DashedEdges::ALL,
        }
    }

    pub(super) fn with_edges(mut self, edges: DashedEdges) -> Self {
        self.edges = edges;
        self
    }
}

/// Draw the four dashed edges of a rectangle in a single batched add.
pub(super) fn stroke_dashed_rect(painter: &Painter, rect: Rect, border: DashedBorder) {
    let DashedBorder {
        stroke,
        h_dash,
        h_period,
        v_dash,
        v_period,
        edges,
    } = border;

    let horiz_steps = if (edges.top || edges.bottom) && h_period > 0.0 && rect.width() > 0.0 {
        (rect.width() / h_period).ceil() as usize + 1
    } else {
        0
    };
    let vert_steps = if (edges.left || edges.right) && v_period > 0.0 && rect.height() > 0.0 {
        (rect.height() / v_period).ceil() as usize + 1
    } else {
        0
    };
    let horiz_sides = edges.top as usize + edges.bottom as usize;
    let vert_sides = edges.left as usize + edges.right as usize;
    let mut shapes = Vec::with_capacity(horiz_steps * horiz_sides + vert_steps * vert_sides);

    if horiz_steps > 0 {
        let mut x = rect.min.x;
        while x < rect.max.x {
            let end = (x + h_dash).min(rect.max.x);
            if edges.top {
                shapes.push(Shape::line_segment(
                    [Pos2::new(x, rect.min.y), Pos2::new(end, rect.min.y)],
                    stroke,
                ));
            }
            if edges.bottom {
                shapes.push(Shape::line_segment(
                    [Pos2::new(x, rect.max.y), Pos2::new(end, rect.max.y)],
                    stroke,
                ));
            }
            x += h_period;
        }
    }

    if vert_steps > 0 {
        let mut y = rect.min.y;
        while y < rect.max.y {
            let end = (y + v_dash).min(rect.max.y);
            if edges.left {
                shapes.push(Shape::line_segment(
                    [Pos2::new(rect.min.x, y), Pos2::new(rect.min.x, end)],
                    stroke,
                ));
            }
            if edges.right {
                shapes.push(Shape::line_segment(
                    [Pos2::new(rect.max.x, y), Pos2::new(rect.max.x, end)],
                    stroke,
                ));
            }
            y += v_period;
        }
    }

    if !shapes.is_empty() {
        painter.extend(shapes);
    }
}

/// Fill a rectangle with 45° diagonal stripes in a single batched add.
///
/// Used for the scan-track hatch pattern and download-ghost stripes.
/// `phase` lets multiple adjacent rects align their stripes by passing
/// a shared x-origin (e.g. `rect.left() % spacing`), matching the
/// existing appearance.
pub(super) fn fill_diagonal_hatch(
    painter: &Painter,
    rect: Rect,
    spacing: f32,
    phase: f32,
    stroke: Stroke,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || spacing <= 0.0 {
        return;
    }

    let h = rect.height();
    let w = rect.width();
    let step_count = ((w + h) / spacing).ceil() as usize + 1;
    let mut shapes = Vec::with_capacity(step_count);

    let mut offset = -phase;
    while offset < w + h {
        let x0 = rect.left() + offset;
        let x1 = x0 - h;
        let (cx0, cy0) = if x0 > rect.right() {
            (rect.right(), rect.top() + (x0 - rect.right()))
        } else {
            (x0, rect.top())
        };
        let (cx1, cy1) = if x1 < rect.left() {
            (rect.left(), rect.bottom() - (rect.left() - x1))
        } else {
            (x1, rect.bottom())
        };
        if cy0 < cy1 {
            shapes.push(Shape::line_segment(
                [Pos2::new(cx0, cy0), Pos2::new(cx1, cy1)],
                stroke,
            ));
        }
        offset += spacing;
    }

    if !shapes.is_empty() {
        painter.extend(shapes);
    }
}
