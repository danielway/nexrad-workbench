//! Batched helper for dashed rectangle borders.
//!
//! The timeline overlays draw a lot of dotted/dashed rectangles — one per
//! visible sweep, per frame. Each `painter.line_segment` call locks the
//! egui graphics buffer, so emitting ~50 individual segments per border
//! per sweep was showing up in idle-frame profiles. This helper pre-sizes
//! a `Vec<Shape>` and pushes everything in one `painter.extend` call,
//! amortizing the lock and reducing paint-list churn.

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

/// Fill a rectangle with batched diagonal hatch lines (the Queued frame-cell
/// texture, spec §6.2). Diagonals read distinctly from the dashed Available
/// outline even in grayscale, and replace the old dotted outline that was
/// confusable with it. `spacing` is the perpendicular gap between lines.
/// Batched into one `painter.extend` for the same perf reason as the dashed
/// helper — there can be one queued cell per visible scan, per frame.
pub(super) fn fill_hatched_rect(painter: &Painter, rect: Rect, stroke: Stroke, spacing: f32) {
    if spacing <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    // 45° hatching: lines of slope 1 (x + y = c). c ranges over
    // [min.x+min.y , max.x+max.y]; step c by `spacing * sqrt(2)` so the
    // perpendicular gap between lines is `spacing`.
    let c_min = rect.min.x + rect.min.y;
    let c_max = rect.max.x + rect.max.y;
    let step = spacing * std::f32::consts::SQRT_2;
    let count = (((c_max - c_min) / step).ceil() as usize).saturating_add(1);
    let mut shapes = Vec::with_capacity(count);

    let mut c = c_min;
    while c <= c_max {
        // Line x + y = c, clipped to the rect. Solve the two edge crossings.
        // Parametrize by x in [min.x, max.x]; y = c - x must land in
        // [min.y, max.y]. Intersect the x-interval with the y-constraint.
        let x_lo = rect.min.x.max(c - rect.max.y);
        let x_hi = rect.max.x.min(c - rect.min.y);
        if x_lo < x_hi {
            let p0 = Pos2::new(x_lo, c - x_lo);
            let p1 = Pos2::new(x_hi, c - x_hi);
            shapes.push(Shape::line_segment([p0, p1], stroke));
        }
        c += step;
    }

    if !shapes.is_empty() {
        painter.extend(shapes);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use eframe::egui::Color32;
    use wasm_bindgen_test::wasm_bindgen_test;

    // `DashedBorder::uniform` must copy the single `dash`/`period` arguments
    // into BOTH the horizontal and vertical fields, and default to all edges.
    #[wasm_bindgen_test]
    fn uniform_mirrors_dash_into_h_and_v() {
        let stroke = Stroke::new(2.0, Color32::from_rgb(10, 20, 30));
        let b = DashedBorder::uniform(stroke, 4.0, 8.0);

        assert!((b.h_dash - 4.0).abs() < 1e-6);
        assert!((b.v_dash - 4.0).abs() < 1e-6);
        assert!((b.h_period - 8.0).abs() < 1e-6);
        assert!((b.v_period - 8.0).abs() < 1e-6);
        // h and v parameters are identical for a uniform border.
        assert!((b.h_dash - b.v_dash).abs() < 1e-6);
        assert!((b.h_period - b.v_period).abs() < 1e-6);
    }

    // The stroke passed in must be preserved verbatim.
    #[wasm_bindgen_test]
    fn uniform_preserves_stroke() {
        let stroke = Stroke::new(1.5, Color32::from_rgb(200, 100, 50));
        let b = DashedBorder::uniform(stroke, 3.0, 6.0);

        assert!((b.stroke.width - 1.5).abs() < 1e-6);
        // Color32 channels compared individually (opaque, no premultiply issue).
        assert!(b.stroke.color.r() == 200);
        assert!(b.stroke.color.g() == 100);
        assert!(b.stroke.color.b() == 50);
        assert!(b.stroke.color.a() == 255);
    }

    // A uniform border always dashes every edge.
    #[wasm_bindgen_test]
    fn uniform_sets_all_edges() {
        let stroke = Stroke::new(1.0, Color32::WHITE);
        let b = DashedBorder::uniform(stroke, 1.0, 2.0);

        assert!(b.edges.top);
        assert!(b.edges.bottom);
        assert!(b.edges.left);
        assert!(b.edges.right);
    }

    // `DashedEdges::ALL` is the constant with every edge enabled.
    #[wasm_bindgen_test]
    fn dashed_edges_all_is_every_edge_true() {
        let e = DashedEdges::ALL;
        assert!(e.top);
        assert!(e.bottom);
        assert!(e.left);
        assert!(e.right);
        // The boolean-as-usize count used by the drawing code is 2 sides each.
        let horiz_sides = e.top as usize + e.bottom as usize;
        let vert_sides = e.left as usize + e.right as usize;
        assert!(horiz_sides == 2);
        assert!(vert_sides == 2);
    }
}
