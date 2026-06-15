//! Map scale bar overlay for the Flat2D view.
//!
//! Stacked km (top) and miles (bottom) bars in the bottom-left corner.
//! Each bar snaps to a "nice" round value (1/2/5 × 10^n) sized to fit
//! within a target pixel width, recomputed from the projection each
//! frame so it stays accurate across zoom and pan.

use crate::geo::MapProjection;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke};

const TARGET_BAR_WIDTH_PX: f32 = 120.0;
const KM_TO_MI: f64 = 0.621371;

/// Trait wrapper for the registry. Scale bar is 2D-only — `visible`
/// gates on view mode; the projection is rebuilt from state since the
/// canvas's local projection isn't reachable from chrome dispatch.
pub(super) struct ScaleBarOverlay;

impl super::Overlay for ScaleBarOverlay {
    fn z_order(&self) -> i32 {
        40
    }

    fn visible(&self, ctx: &super::OverlayContext) -> bool {
        ctx.state.viz_state.view_mode() == crate::state::ViewMode::Flat2D
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
    let Some(km_per_pixel) = compute_km_per_pixel(projection, rect) else {
        return;
    };

    let target_km = TARGET_BAR_WIDTH_PX as f64 * km_per_pixel;
    let target_mi = target_km * KM_TO_MI;

    let nice_km = nice_round(target_km);
    let nice_mi = nice_round(target_mi);

    let km_pixels = (nice_km / km_per_pixel) as f32;
    let mi_pixels = (nice_mi / KM_TO_MI / km_per_pixel) as f32;

    let painter = ui.painter();

    let margin = 10.0f32;
    let row_height = 16.0f32;
    let bar_thickness = 1.5f32;
    let cap_height = 5.0f32;
    let label_pad = 3.0f32;

    let bar_left = rect.left() + margin;
    let mi_y = rect.bottom() - margin - cap_height * 0.5;
    let km_y = mi_y - row_height;

    draw_row(
        painter,
        bar_left,
        km_y,
        km_pixels,
        bar_thickness,
        cap_height,
        label_pad,
        &format_km(nice_km),
    );
    draw_row(
        painter,
        bar_left,
        mi_y,
        mi_pixels,
        bar_thickness,
        cap_height,
        label_pad,
        &format_mi(nice_mi),
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
    let bar_color = Color32::from_rgba_unmultiplied(220, 220, 230, 230);
    let text_color = Color32::from_rgba_unmultiplied(220, 220, 230, 230);
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
        egui::FontId::monospace(10.0),
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
    let km = haversine_km(lat_a, lon_a, lat_b, lon_b);
    if km.is_finite() && km > 0.0 {
        Some(km / 100.0)
    } else {
        None
    }
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    r * c
}

/// Snap to a 1/2/5 × 10^n round number not exceeding `target`.
fn nice_round(target: f64) -> f64 {
    if !target.is_finite() || target <= 0.0 {
        return 0.0;
    }
    let mag = 10f64.powf(target.log10().floor());
    let frac = target / mag;
    let nice = if frac >= 5.0 {
        5.0
    } else if frac >= 2.0 {
        2.0
    } else {
        1.0
    };
    nice * mag
}

fn format_km(km: f64) -> String {
    if km >= 1.0 {
        format!("{:.0} km", km)
    } else if km >= 0.1 {
        format!("{:.1} km", km)
    } else {
        format!("{:.0} m", km * 1000.0)
    }
}

fn format_mi(mi: f64) -> String {
    if mi >= 1.0 {
        format!("{:.0} mi", mi)
    } else if mi >= 0.1 {
        format!("{:.1} mi", mi)
    } else {
        format!("{:.0} ft", mi * 5280.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_round_snaps_down() {
        assert_eq!(nice_round(120.0), 100.0);
        assert_eq!(nice_round(80.0), 50.0);
        assert_eq!(nice_round(30.0), 20.0);
        assert_eq!(nice_round(15.0), 10.0);
        assert_eq!(nice_round(7.0), 5.0);
        assert_eq!(nice_round(3.0), 2.0);
        assert_eq!(nice_round(1.5), 1.0);
        assert_eq!(nice_round(0.7), 0.5);
    }

    #[test]
    fn nice_round_handles_edges() {
        assert_eq!(nice_round(0.0), 0.0);
        assert_eq!(nice_round(-1.0), 0.0);
        assert_eq!(nice_round(f64::NAN), 0.0);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::geo::Projection;
    use eframe::egui::{Pos2, Rect, Vec2};
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---- haversine_km ----

    #[wasm_bindgen_test]
    fn haversine_zero_distance_is_zero() {
        let d = haversine_km(40.0, -100.0, 40.0, -100.0);
        assert!(d.abs() < 1e-9, "got {}", d);
    }

    #[wasm_bindgen_test]
    fn haversine_one_degree_lon_at_equator() {
        // One degree of longitude at the equator ~= 111.195 km
        // (R=6371, c = 1deg in rad).
        let d = haversine_km(0.0, 0.0, 0.0, 1.0);
        assert!((d - 111.195).abs() < 0.05, "got {}", d);
    }

    #[wasm_bindgen_test]
    fn haversine_one_degree_lat() {
        // One degree of latitude ~= 111.195 km anywhere.
        let d = haversine_km(10.0, -50.0, 11.0, -50.0);
        assert!((d - 111.195).abs() < 0.05, "got {}", d);
    }

    #[wasm_bindgen_test]
    fn haversine_is_symmetric() {
        let ab = haversine_km(35.0, -97.0, 36.0, -96.0);
        let ba = haversine_km(36.0, -96.0, 35.0, -97.0);
        assert!((ab - ba).abs() < 1e-9, "ab={} ba={}", ab, ba);
    }

    #[wasm_bindgen_test]
    fn haversine_lon_shrinks_with_latitude() {
        // A degree of longitude covers less ground far from the equator.
        let near_equator = haversine_km(0.0, 0.0, 0.0, 1.0);
        let high_lat = haversine_km(60.0, 0.0, 60.0, 1.0);
        assert!(
            high_lat < near_equator,
            "high={} eq={}",
            high_lat,
            near_equator
        );
        // cos(60deg) = 0.5, so roughly half.
        assert!(
            (high_lat - near_equator * 0.5).abs() < 0.1,
            "got {}",
            high_lat
        );
    }

    // ---- nice_round (cases distinct from the existing mod tests) ----

    #[wasm_bindgen_test]
    fn nice_round_large_magnitude() {
        assert!((nice_round(1234.0) - 1000.0).abs() < 1e-9);
        assert!((nice_round(5500.0) - 5000.0).abs() < 1e-9);
        assert!((nice_round(2999.0) - 2000.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn nice_round_exact_boundaries_snap_to_self() {
        assert!((nice_round(5.0) - 5.0).abs() < 1e-9);
        assert!((nice_round(2.0) - 2.0).abs() < 1e-9);
        assert!((nice_round(1.0) - 1.0).abs() < 1e-9);
        assert!((nice_round(100.0) - 100.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn nice_round_just_below_boundary_steps_down() {
        // 4.9 -> 2 (frac < 5 but >= 2), 1.9 -> 1 (frac < 2)
        assert!((nice_round(4.9) - 2.0).abs() < 1e-9);
        assert!((nice_round(1.9) - 1.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn nice_round_infinite_is_zero() {
        assert!(nice_round(f64::INFINITY) == 0.0);
    }

    #[wasm_bindgen_test]
    fn nice_round_never_exceeds_target() {
        for &t in &[3.7_f64, 19.0, 88.0, 0.34, 640.0] {
            let n = nice_round(t);
            assert!(n <= t + 1e-9, "nice {} exceeds target {}", n, t);
            assert!(n > 0.0, "nice {} not positive for {}", n, t);
        }
    }

    // ---- format_km ----

    #[wasm_bindgen_test]
    fn format_km_whole_km() {
        assert!(format_km(100.0) == "100 km");
        assert!(format_km(5.0) == "5 km");
        assert!(format_km(1.0) == "1 km");
    }

    #[wasm_bindgen_test]
    fn format_km_sub_km_decimal() {
        // [0.1, 1.0) -> one decimal of km
        assert!(format_km(0.5) == "0.5 km");
        assert!(format_km(0.2) == "0.2 km");
    }

    #[wasm_bindgen_test]
    fn format_km_meters() {
        // < 0.1 km -> meters
        assert!(format_km(0.05) == "50 m");
        assert!(format_km(0.02) == "20 m");
    }

    // ---- format_mi ----

    #[wasm_bindgen_test]
    fn format_mi_whole_miles() {
        assert!(format_mi(50.0) == "50 mi");
        assert!(format_mi(2.0) == "2 mi");
        assert!(format_mi(1.0) == "1 mi");
    }

    #[wasm_bindgen_test]
    fn format_mi_sub_mile_decimal() {
        // [0.1, 1.0) -> one decimal of mi
        assert!(format_mi(0.5) == "0.5 mi");
        assert!(format_mi(0.2) == "0.2 mi");
    }

    #[wasm_bindgen_test]
    fn format_mi_feet() {
        // < 0.1 mi -> feet (mi * 5280)
        assert!(format_mi(0.05) == "264 ft");
        assert!(format_mi(0.01) == "53 ft");
    }

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
