//! mPING storm-report marker overlay on the 2D canvas.
//!
//! Renders one filled circle per report inside the visible map bounds,
//! colored by category. Reports outside the current ±30 min playback
//! window are dropped — the manager refetches as the cursor scrubs, but
//! state can briefly contain stale entries between invalidation and the
//! next response.
//!
//! The selected report (clicked marker) is highlighted with a brighter
//! ring and its details are drawn as a tooltip-style popover anchored
//! near the marker, similar to the canvas inspector.

use crate::geo::MapProjection;
use crate::mping::StormReport;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};
use geo_types::Coord;

use super::super::colors::mping as mping_colors;

pub(crate) fn render_mping_reports(
    painter: &Painter,
    projection: &MapProjection,
    reports: &[StormReport],
    window_min_ms: f64,
    window_max_ms: f64,
    selected_report_id: Option<i64>,
) {
    let (min_lon, min_lat, max_lon, max_lat) = projection.visible_bounds();
    let padding = 0.5;

    for report in reports {
        if window_max_ms > window_min_ms
            && (report.obtime_ms < window_min_ms || report.obtime_ms > window_max_ms)
        {
            continue;
        }

        if report.lat < min_lat - padding
            || report.lat > max_lat + padding
            || report.lon < min_lon - padding
            || report.lon > max_lon + padding
        {
            continue;
        }

        let pos = projection.geo_to_screen(Coord {
            x: report.lon,
            y: report.lat,
        });

        let fill = mping_colors::fill(report.category);
        let is_selected = selected_report_id == Some(report.id);
        let radius = if is_selected { 6.5 } else { 4.5 };
        painter.circle_filled(pos, radius, fill);
        painter.circle_stroke(pos, radius, Stroke::new(1.0, mping_colors::STROKE));
        if is_selected {
            // Bright halo so the selected marker is visually distinct from
            // the dozens of unselected ones around it.
            painter.circle_stroke(pos, radius + 2.5, Stroke::new(1.5, Color32::WHITE));
        }
    }
}

/// Render the detail popover for the currently-selected report, if any.
/// Anchored near the marker but kept inside the canvas rect so it doesn't
/// clip off-screen.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_mping_detail(
    painter: &Painter,
    rect: Rect,
    projection: &MapProjection,
    reports: &[StormReport],
    selected_report_id: Option<i64>,
    radar_lat: f64,
    radar_lon: f64,
    playback_secs: f64,
    use_local_time: bool,
) {
    let Some(id) = selected_report_id else {
        return;
    };
    let Some(report) = reports.iter().find(|r| r.id == id) else {
        return;
    };

    let marker_pos = projection.geo_to_screen(Coord {
        x: report.lon,
        y: report.lat,
    });

    // Distance + bearing from active radar.
    let dlat = report.lat - radar_lat;
    let dlon = (report.lon - radar_lon) * radar_lat.to_radians().cos();
    let range_km = (dlat * dlat + dlon * dlon).sqrt() * 111.0;
    let bearing_deg = (dlon.atan2(dlat).to_degrees() + 360.0) % 360.0;

    // Time relative to playback cursor.
    let report_secs = report.obtime_ms / 1000.0;
    let delta_secs = report_secs - playback_secs;
    let rel = format_relative(delta_secs);

    // Description falls back to the category label when the API didn't
    // give us a more specific description.
    let desc = if report.description.is_empty() {
        report.category.label().to_string()
    } else {
        report.description.clone()
    };

    let mut lines: Vec<(String, bool)> = Vec::new(); // (text, is_heading)
    lines.push((report.category.label().to_string(), true));
    lines.push((desc, false));
    lines.push((format_obtime(report.obtime_ms, use_local_time), false));
    lines.push((rel, false));
    lines.push((
        format!(
            "{:.1} km {} of radar",
            range_km,
            compass_dir(bearing_deg as f32)
        ),
        false,
    ));
    lines.push((
        format!(
            "{:.4}\u{00B0}{} {:.4}\u{00B0}{}",
            report.lat.abs(),
            if report.lat >= 0.0 { "N" } else { "S" },
            report.lon.abs(),
            if report.lon >= 0.0 { "E" } else { "W" },
        ),
        false,
    ));

    // Layout
    let heading_font = egui::FontId::proportional(12.0);
    let body_font = egui::FontId::monospace(11.0);
    let padding = Vec2::new(8.0, 6.0);
    let line_spacing = 2.0;

    let galleys: Vec<_> = lines
        .iter()
        .map(|(text, heading)| {
            let font = if *heading {
                heading_font.clone()
            } else {
                body_font.clone()
            };
            painter.layout_no_wrap(text.clone(), font, Color32::WHITE)
        })
        .collect();

    let inner_w = galleys.iter().map(|g| g.size().x).fold(0.0_f32, f32::max);
    let inner_h: f32 = galleys.iter().map(|g| g.size().y).sum::<f32>()
        + line_spacing * (galleys.len().saturating_sub(1) as f32);
    let total_size = Vec2::new(inner_w, inner_h) + padding * 2.0;

    // Anchor: right of the marker by default; flip to left if it would
    // clip off the canvas. Same for vertical.
    let mut anchor = marker_pos + Vec2::new(12.0, -total_size.y / 2.0);
    if anchor.x + total_size.x > rect.right() - 4.0 {
        anchor.x = marker_pos.x - total_size.x - 12.0;
    }
    anchor.x = anchor.x.max(rect.left() + 4.0);
    anchor.y = anchor
        .y
        .clamp(rect.top() + 4.0, rect.bottom() - total_size.y - 4.0);

    let bg_rect = Rect::from_min_size(anchor, total_size);

    // Background + colored left edge in the marker's category color so the
    // popover and marker are visually linked.
    painter.rect_filled(
        bg_rect,
        4.0,
        Color32::from_rgba_unmultiplied(20, 20, 30, 230),
    );
    painter.rect_stroke(
        bg_rect,
        4.0,
        Stroke::new(1.0, Color32::from_rgb(80, 80, 100)),
        StrokeKind::Outside,
    );
    let edge = Rect::from_min_size(bg_rect.min, Vec2::new(3.0, bg_rect.height()));
    painter.rect_filled(edge, 0.0, mping_colors::fill(report.category));

    // Draw text lines.
    let mut cursor = anchor + padding;
    for (i, galley) in galleys.into_iter().enumerate() {
        let h = galley.size().y;
        let color = if i == 0 {
            mping_colors::fill(report.category)
        } else {
            Color32::from_rgb(220, 220, 230)
        };
        painter.galley(cursor, galley, color);
        cursor.y += h + line_spacing;
    }

    // Connecting line from marker to popover.
    let line_target = if marker_pos.x < bg_rect.left() {
        Pos2::new(bg_rect.left(), bg_rect.center().y)
    } else {
        Pos2::new(bg_rect.right(), bg_rect.center().y)
    };
    painter.line_segment(
        [marker_pos, line_target],
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 100)),
    );
}

fn format_obtime(ts_ms: f64, use_local_time: bool) -> String {
    if use_local_time {
        let d = js_sys::Date::new_0();
        d.set_time(ts_ms);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02} local",
            d.get_full_year(),
            d.get_month() + 1,
            d.get_date(),
            d.get_hours(),
            d.get_minutes(),
        )
    } else {
        match chrono::DateTime::from_timestamp_millis(ts_ms as i64) {
            Some(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
            None => "unknown".to_string(),
        }
    }
}

fn format_relative(delta_secs: f64) -> String {
    let abs = delta_secs.abs();
    let unit = if abs < 60.0 {
        format!("{:.0}s", abs)
    } else if abs < 3600.0 {
        format!("{:.0}m", abs / 60.0)
    } else {
        format!("{:.1}h", abs / 3600.0)
    };
    if delta_secs < -0.5 {
        format!("{} before playback", unit)
    } else if delta_secs > 0.5 {
        format!("{} after playback", unit)
    } else {
        "at playback time".to_string()
    }
}

fn compass_dir(bearing_deg: f32) -> &'static str {
    let dirs = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let idx = (((bearing_deg % 360.0) / 22.5).round() as usize) % 16;
    dirs[idx]
}
