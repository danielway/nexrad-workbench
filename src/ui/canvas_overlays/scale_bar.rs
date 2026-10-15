//! Map scale bar overlay for the Flat2D view.
//!
//! Stacked km (top) and miles (bottom) bars in the bottom-left corner, on a
//! scrim so they never print over map labels (one-instrument §7). The bars'
//! lengths and labels are the core's [`crate::core::canvas::scale_bar_vm`];
//! this file only measures km-per-pixel from the projection and paints.

use crate::geo::MapProjection;
use crate::ui::theme;
use eframe::egui::{self, Pos2, Rect, Stroke};

/// Trait wrapper for the registry. Scale bar is 2D-only — `visible`
/// gates on view mode; the projection is rebuilt from state since the
/// canvas's local projection isn't reachable from chrome dispatch.
pub(super) struct ScaleBarOverlay;

impl super::Overlay for ScaleBarOverlay {
    fn z_order(&self) -> i32 {
        40
    }

    fn visible(&self, ctx: &super::OverlayContext) -> bool {
        ctx.state.viz_state.view_mode() == crate::geo::ViewMode::Flat2D
    }

    fn draw(&self, ui: &mut egui::Ui, ctx: &super::OverlayContext) {
        let mut projection = MapProjection::new(
            ctx.state.viz_state.center_lat,
            ctx.state.viz_state.center_lon,
        );
        projection.update(
            ctx.state.viz_state.zoom(),
            ctx.state.viz_state.pan_offset(),
            ctx.rect,
        );
        draw_scale_bar(ui, &ctx.rect, &projection);
    }
}

fn draw_scale_bar(ui: &mut egui::Ui, rect: &Rect, projection: &MapProjection) {
    let Some(vm) =
        compute_km_per_pixel(projection, rect).and_then(crate::core::canvas::scale_bar_vm)
    else {
        return;
    };

    let painter = ui.painter();

    let margin = 10.0f32;
    let pad = 6.0f32;
    let row_height = 16.0f32;
    let bar_thickness = 1.5f32;
    let cap_height = 5.0f32;
    let label_pad = 3.0f32;

    // The scrim: wide enough for the longer bar, two rows tall.
    let width = vm.km_px.max(vm.mi_px) + pad * 2.0;
    let height = row_height * 2.0 + pad * 2.0;
    let scrim = Rect::from_min_size(
        Pos2::new(rect.left() + margin, rect.bottom() - margin - height),
        egui::vec2(width, height),
    );
    painter.rect(
        scrim,
        4.0,
        theme::map::scrim(),
        Stroke::new(1.0_f32, theme::map::scrim_edge()),
        egui::StrokeKind::Inside,
    );

    let bar_left = scrim.left() + pad;
    let mi_y = scrim.bottom() - pad - cap_height * 0.5;
    let km_y = mi_y - row_height;

    draw_row(
        painter,
        bar_left,
        km_y,
        vm.km_px,
        bar_thickness,
        cap_height,
        label_pad,
        &vm.km_label,
    );
    draw_row(
        painter,
        bar_left,
        mi_y,
        vm.mi_px,
        bar_thickness,
        cap_height,
        label_pad,
        &vm.mi_label,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    painter: &egui::Painter,
    left: f32,
    y: f32,
    width: f32,
    bar_thickness: f32,
    cap_height: f32,
    label_pad: f32,
    label: &str,
) {
    let bar_color = theme::alpha(theme::TEXT, 230);
    let text_color = theme::TEXT_DIM;
    let stroke = Stroke::new(bar_thickness, bar_color);

    let right = left + width;
    painter.line_segment([Pos2::new(left, y), Pos2::new(right, y)], stroke);
    painter.line_segment(
        [
            Pos2::new(left, y - cap_height * 0.5),
            Pos2::new(left, y + cap_height * 0.5),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(right, y - cap_height * 0.5),
            Pos2::new(right, y + cap_height * 0.5),
        ],
        stroke,
    );

    painter.text(
        Pos2::new((left + right) * 0.5, y - label_pad),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::monospace(theme::CAPTION - 1.0),
        text_color,
    );
}

/// km per screen pixel at the canvas center, derived from the projection.
///
/// Works against any [`Projection`] — 2D always resolves, 3D returns
/// `None` when the canvas center happens to land off-globe (the scale
/// bar is suppressed in that case).
fn compute_km_per_pixel(projection: &dyn crate::geo::Projection, rect: &Rect) -> Option<f64> {
    if rect.width() < 4.0 || rect.height() < 4.0 {
        return None;
    }
    let center = rect.center();
    let (lat_a, lon_a) = projection.screen_to_geo(center)?;
    let (lat_b, lon_b) = projection.screen_to_geo(Pos2::new(center.x + 100.0, center.y))?;
    let km = crate::core::canvas::haversine_km(lat_a, lon_a, lat_b, lon_b);
    if km.is_finite() && km > 0.0 {
        Some(km / 100.0)
    } else {
        None
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::geo::Projection;
    use eframe::egui::{Pos2, Rect, Vec2};
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---- compute_km_per_pixel (mock Projection, no egui runtime) ----

    /// Linear mock: screen x maps to longitude via `deg_per_px`, lat fixed
    /// at the equator. `resolves` toggles whether screen_to_geo returns None
    /// (simulating a 3D off-globe center).
    struct LinearProj {
        deg_per_px: f64,
        resolves: bool,
    }

    impl Projection for LinearProj {
        fn geo_to_screen(&self, _lat: f64, _lon: f64) -> Option<Pos2> {
            None
        }
        fn screen_to_geo(&self, pos: Pos2) -> Option<(f64, f64)> {
            if self.resolves {
                Some((0.0, pos.x as f64 * self.deg_per_px))
            } else {
                None
            }
        }
        fn visible_bounds(&self) -> Option<(f64, f64, f64, f64)> {
            None
        }
    }

    fn big_rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))
    }

    #[wasm_bindgen_test]
    fn km_per_pixel_linear_projection() {
        // 0.01 deg/px => over the 100px probe, dlon = 1.0 deg at the
        // equator => ~111.195 km / 100 px => ~1.11195 km/px.
        let proj = LinearProj {
            deg_per_px: 0.01,
            resolves: true,
        };
        let v = compute_km_per_pixel(&proj, &big_rect()).expect("should resolve");
        assert!((v - 1.11195).abs() < 0.01, "got {}", v);
    }

    #[wasm_bindgen_test]
    fn km_per_pixel_scales_with_deg_per_px() {
        // Doubling deg/px doubles km/px.
        let a = compute_km_per_pixel(
            &LinearProj {
                deg_per_px: 0.01,
                resolves: true,
            },
            &big_rect(),
        )
        .expect("a");
        let b = compute_km_per_pixel(
            &LinearProj {
                deg_per_px: 0.02,
                resolves: true,
            },
            &big_rect(),
        )
        .expect("b");
        assert!((b - 2.0 * a).abs() < 0.01, "a={} b={}", a, b);
    }

    #[wasm_bindgen_test]
    fn km_per_pixel_none_on_tiny_rect() {
        let proj = LinearProj {
            deg_per_px: 0.01,
            resolves: true,
        };
        let tiny = Rect::from_min_size(Pos2::ZERO, Vec2::new(3.0, 3.0));
        assert!(compute_km_per_pixel(&proj, &tiny).is_none());
    }

    #[wasm_bindgen_test]
    fn km_per_pixel_none_when_center_off_globe() {
        // screen_to_geo returns None => suppressed (3D off-globe case).
        let proj = LinearProj {
            deg_per_px: 0.01,
            resolves: false,
        };
        assert!(compute_km_per_pixel(&proj, &big_rect()).is_none());
    }
}
