//! mPING storm-report marker overlay on the 2D canvas.
//!
//! Renders one filled circle per report inside the visible map bounds,
//! colored by category. Reports outside the current ±30 min playback
//! window are dropped — the manager refetches as the cursor scrubs, but
//! state can briefly contain stale entries between invalidation and the
//! next response.

use crate::geo::MapProjection;
use crate::mping::StormReport;
use eframe::egui::{Painter, Stroke};
use geo_types::Coord;

use super::super::colors::mping as mping_colors;

pub(crate) fn render_mping_reports(
    painter: &Painter,
    projection: &MapProjection,
    reports: &[StormReport],
    window_min_ms: f64,
    window_max_ms: f64,
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
        painter.circle_filled(pos, 4.5, fill);
        painter.circle_stroke(pos, 4.5, Stroke::new(1.0, mping_colors::STROKE));
    }
}
