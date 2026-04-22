//! Map scale bar overlay for the Flat2D view.
//!
//! Stacked km (top) and miles (bottom) bars in the bottom-left corner.
//! Each bar snaps to a "nice" round value (1/2/5 × 10^n) sized to fit
//! within a target pixel width, recomputed from the projection each
//! frame so it stays accurate across zoom and pan.

use crate::geo::MapProjection;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

const TARGET_BAR_WIDTH_PX: f32 = 120.0;
const KM_TO_MI: f64 = 0.621371;

pub(crate) fn draw_scale_bar(ui: &mut egui::Ui, rect: &Rect, projection: &MapProjection) {
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

    let margin = 16.0f32;
    let row_height = 18.0f32;
    let bar_thickness = 2.0f32;
    let cap_height = 6.0f32;
    let label_pad = 4.0f32;

    let max_pixels = km_pixels.max(mi_pixels);
    let panel_w = max_pixels + 24.0;
    let panel_h = row_height * 2.0 + 8.0;
    let panel_left = rect.left() + margin;
    let panel_bottom = rect.bottom() - margin;
    let panel_rect = Rect::from_min_size(
        Pos2::new(panel_left, panel_bottom - panel_h),
        Vec2::new(panel_w, panel_h),
    );

    painter.rect_filled(
        panel_rect,
        3.0,
        Color32::from_rgba_unmultiplied(15, 15, 25, 160),
    );
    painter.rect_stroke(
        panel_rect,
        3.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(80, 80, 100, 140)),
        egui::StrokeKind::Outside,
    );

    let bar_left = panel_left + 12.0;
    let km_y = panel_rect.top() + row_height * 0.5 + 2.0;
    let mi_y = km_y + row_height;

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
fn compute_km_per_pixel(projection: &MapProjection, rect: &Rect) -> Option<f64> {
    if rect.width() < 4.0 || rect.height() < 4.0 {
        return None;
    }
    let center = rect.center();
    let a = projection.screen_to_geo(center);
    let b = projection.screen_to_geo(Pos2::new(center.x + 100.0, center.y));
    let km = haversine_km(a.y, a.x, b.y, b.x);
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
