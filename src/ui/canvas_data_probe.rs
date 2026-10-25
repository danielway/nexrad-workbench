//! Canvas data-probe tool and measurement overlays.
//!
//! Renders the hover tooltip (lat/lon, azimuth, range, product value),
//! the crosshair cursor and distance measurement lines on top of the radar
//! canvas. The "Data probe" name
//! distinguishes this map-space hover tool from the scan inspector (the
//! per-scan volume breakdown), which is a separate surface.

use crate::geo::MapProjection;
use crate::nexrad::RadarGpuRenderer;
use crate::ui::theme;
use eframe::egui::{self, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};
use geo_types::Coord;
use std::sync::{Arc, Mutex};

use super::canvas::format_unix_timestamp;

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_data_probe(
    painter: &Painter,
    projection: &MapProjection,
    hover_pos: Pos2,
    radar_lat: f64,
    radar_lon: f64,
    gpu_renderer: Option<&Arc<Mutex<RadarGpuRenderer>>>,
    product: &crate::core::RadarProduct,
    use_local_time: bool,
    sweep_params: Option<(f32, f32)>,
    // The storm cell under the pointer (core::storm_cell_view::probe_line).
    cell_line: Option<String>,
    // With several sites on the map, one line per site under the cursor
    // (core::site_canvas::probe_site_line), focus first; they replace the
    // single site's polar, value and time lines.
    site_lines: Option<Vec<String>>,
) {
    let geo = projection.screen_to_geo(hover_pos);
    let lat = geo.y;
    let lon = geo.x;

    // Polar coordinates relative to the radar site (pure core).
    let (azimuth_deg, range_km) = crate::core::canvas::geo_to_polar(lat, lon, radar_lat, radar_lon);

    // Look up data value and collection time (sweep-aware when animating)
    let (value, collection_time) = gpu_renderer
        .map(|r| {
            let renderer = r.lock().expect("renderer mutex poisoned");
            let v = renderer.value_at_polar(azimuth_deg as f32, range_km, sweep_params);
            let t = renderer.collection_time_at_polar(azimuth_deg as f32, sweep_params);
            (v, t)
        })
        .unwrap_or((None, None));

    // Build tooltip text
    let mut lines = vec![format!("{:.4}\u{00B0}N {:.4}\u{00B0}W", lat, -lon)];
    if let Some(site_lines) = site_lines {
        lines.extend(site_lines);
        lines.extend(cell_line);
        paint_probe(painter, hover_pos, &lines.join("\n"));
        return;
    }
    lines.push(format!(
        "Az: {:.1}\u{00B0}  Rng: {:.1} km",
        azimuth_deg, range_km
    ));
    if let Some(v) = value {
        let unit = product.unit();
        if unit.is_empty() {
            lines.push(format!("{}: {:.3}", product.short_code(), v));
        } else {
            lines.push(format!("{}: {:.1} {}", product.short_code(), v, unit));
        }
    }
    if let Some(ts) = collection_time {
        lines.push(format_unix_timestamp(ts, use_local_time));
    }
    lines.extend(cell_line);
    paint_probe(painter, hover_pos, &lines.join("\n"));
}

/// The probe's tooltip and crosshair at `hover_pos`.
fn paint_probe(painter: &Painter, hover_pos: Pos2, text: &str) {
    // Draw tooltip background
    let font_id = egui::FontId::monospace(11.0);
    let galley = painter.layout_no_wrap(text.to_string(), font_id, theme::TEXT);
    let tooltip_size = galley.size();
    let padding = Vec2::new(6.0, 4.0);
    let tooltip_pos = hover_pos + Vec2::new(16.0, -tooltip_size.y - 8.0);
    let bg_rect = Rect::from_min_size(tooltip_pos - padding, tooltip_size + padding * 2.0);

    painter.rect_filled(bg_rect, 4.0, theme::alpha(theme::RAISED, 220));
    painter.rect_stroke(
        bg_rect,
        4.0,
        Stroke::new(1.0_f32, theme::TEXT_FAINT),
        StrokeKind::Outside,
    );
    painter.galley(tooltip_pos, galley, theme::TEXT);

    // Draw crosshair at hover position
    let cross_size = 8.0;
    let cross_color = theme::alpha(theme::TEXT, 160);
    painter.line_segment(
        [
            hover_pos - Vec2::new(cross_size, 0.0),
            hover_pos + Vec2::new(cross_size, 0.0),
        ],
        Stroke::new(1.0_f32, cross_color),
    );
    painter.line_segment(
        [
            hover_pos - Vec2::new(0.0, cross_size),
            hover_pos + Vec2::new(0.0, cross_size),
        ],
        Stroke::new(1.0_f32, cross_color),
    );
}

/// Paint the distance tool's view-model ([`crate::core::measure::view`]):
/// the snap hint ring, the line, the endpoint markers (a snapped end with
/// its track letter beside it, an ended track's end muted) and the readout
/// just past the end point.
pub(crate) fn render_distance_measurement(
    painter: &Painter,
    projection: &MapProjection,
    vm: &crate::core::measure::MeasureVm,
    // Whether a screen rectangle is on the canvas and clear of the chrome.
    clear: &dyn Fn(Rect) -> bool,
) {
    let screen = |lat: f64, lon: f64| projection.geo_to_screen(Coord { x: lon, y: lat });

    if let Some(h) = &vm.snap_hint {
        let p = screen(h.lat, h.lon);
        painter.circle_stroke(p, 11.0, Stroke::new(3.0_f32, theme::map::HALO));
        painter.circle_stroke(p, 11.0, Stroke::new(1.5_f32, theme::map::TOOL));
    }

    if let Some((a, b)) = vm.line {
        painter.line_segment(
            [screen(a.0, a.1), screen(b.0, b.1)],
            Stroke::new(2.0_f32, theme::map::TOOL),
        );
    }

    for e in &vm.ends {
        let p = screen(e.lat, e.lon);
        let (fill, text) = if e.muted {
            (theme::TEXT_DIM, theme::TEXT_DIM)
        } else {
            (theme::map::TOOL, theme::TEXT)
        };
        painter.circle_filled(p, 5.0, fill);
        painter.circle_stroke(p, 5.0, Stroke::new(1.5_f32, theme::map::OUTLINE));
        if let Some(label) = &e.label {
            let galley = painter.layout_no_wrap(label.clone(), egui::FontId::monospace(11.0), text);
            let pos = p + Vec2::new(8.0, -8.0 - galley.size().y / 2.0);
            let bg = Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(3.0, 1.0));
            painter.rect_filled(bg, 3.0, theme::alpha(theme::RAISED, 220));
            painter.rect_stroke(bg, 3.0, Stroke::new(1.0_f32, fill), StrokeKind::Outside);
            painter.galley(pos, galley, text);
        }
    }

    if let (Some((a, b)), false) = (vm.line, vm.readout.is_empty()) {
        let (sa, sb) = (screen(a.0, a.1), screen(b.0, b.1));
        let galley = painter.layout_no_wrap(
            vm.readout.join("\n"),
            egui::FontId::monospace(11.0),
            theme::TEXT,
        );
        let size = galley.size();
        let padding = Vec2::new(5.0, 3.0);
        // Just past the end point, along the line, so it covers neither
        // the line nor the start (a snapped storm and its letter); failing
        // that past the start, beside the middle, or above, below or beside
        // either end: the first spot on the canvas, clear of the chrome and
        // of the end markers and their letters.
        let along = (sb - sa).normalized();
        let along = if along.is_finite() && along != Vec2::ZERO {
            along
        } else {
            Vec2::new(0.0, -1.0)
        };
        let half = size / 2.0 + padding;
        let reach = |d: Vec2| (half.x / d.x.abs()).min(half.y / d.y.abs()) + 16.0;
        let mid = sa + (sb - sa) / 2.0;
        let perp = Vec2::new(-along.y, along.x);
        let up = if perp.y <= 0.0 { perp } else { -perp };
        let clip = painter.clip_rect().shrink2(padding + Vec2::splat(4.0));
        // The end markers and their letters stay uncovered.
        let marks: Vec<Rect> = vm
            .ends
            .iter()
            .map(|e| {
                let p = screen(e.lat, e.lon);
                match e.label {
                    Some(_) => {
                        Rect::from_min_max(p + Vec2::new(-8.0, -26.0), p + Vec2::new(40.0, 8.0))
                    }
                    None => Rect::from_center_size(p, Vec2::splat(16.0)),
                }
            })
            .collect();
        let (dx, dy) = (Vec2::new(half.x + 16.0, 0.0), Vec2::new(0.0, half.y + 16.0));
        let candidates = [
            sb + along * reach(along),
            sa - along * reach(along),
            mid + up * reach(up),
            mid - up * reach(up),
            sb - dy,
            sb + dy,
            sa - dy,
            sa + dy,
            sb - dx,
            sb + dx,
            sa - dx,
            sa + dx,
        ];
        let fits = |c: &Pos2| {
            let bg = Rect::from_center_size(*c, size + padding * 2.0);
            clear(bg) && !marks.iter().any(|m| m.intersects(bg))
        };
        let center = candidates
            .iter()
            .copied()
            .find(fits)
            .unwrap_or(candidates[0]);
        let mut pos = center - size / 2.0;
        pos.x = pos.x.min(clip.right() - size.x).max(clip.left());
        pos.y = pos.y.min(clip.bottom() - size.y).max(clip.top());
        let bg = Rect::from_min_size(pos - padding, size + padding * 2.0);
        painter.rect_filled(bg, 3.0, theme::alpha(theme::RAISED, 220));
        painter.rect_stroke(
            bg,
            3.0,
            Stroke::new(1.0_f32, theme::map::TOOL),
            StrokeKind::Outside,
        );
        painter.galley(pos, galley, theme::TEXT);
    }
}
