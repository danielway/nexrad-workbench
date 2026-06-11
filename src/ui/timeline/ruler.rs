//! Ruler rendering: tick marks and playback cursor.

use super::{format_timestamp, select_tick_config, style, TimelineFrame};
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Painter, Pos2, Stroke};

/// Draw tick marks (major + minor) and labels in the tick lane. Tick
/// spacing comes from the zoom level; when displaying local time, ticks
/// align to local boundaries (e.g. day ticks land on local midnight, not
/// UTC midnight) by shifting into local seconds for alignment and back to
/// UTC for plotting.
pub(super) fn render_tick_marks(painter: &Painter, frame: &TimelineFrame<'_>) {
    let tick_rect = &frame.rects.tick;
    let (dark, use_local) = (frame.dark, frame.use_local);
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);

    let tick_config = select_tick_config(frame.zoom);
    let major_interval = tick_config.major_interval;
    let minor_interval = (major_interval / tick_config.minor_divisions as i64).max(1);

    // getTimezoneOffset() returns minutes positive-west; convert to
    // seconds-east so that local = utc + tz_offset_secs.
    let tz_offset_secs: i64 = if use_local {
        let d = js_sys::Date::new_0();
        d.set_time(frame.view_start * 1000.0);
        -(d.get_timezone_offset() as i64) * 60
    } else {
        0
    };

    let local_start = frame.view_start as i64 + tz_offset_secs;
    let local_end = frame.view_end as i64 + tz_offset_secs;
    let first_tick = (local_start / minor_interval) * minor_interval - tz_offset_secs;
    let last_tick = ((local_end / minor_interval) + 1) * minor_interval - tz_offset_secs;

    let mut tick = first_tick;
    while tick <= last_tick {
        let x = ts_to_x(tick as f64);

        if x >= tick_rect.left() && x <= tick_rect.right() {
            let local_tick = tick + tz_offset_secs;
            let is_major = local_tick % major_interval == 0;
            let tick_height = if is_major { 4.0 } else { 2.0 };
            let tick_color = if is_major {
                tl_colors::tick_major(dark)
            } else {
                tl_colors::tick_minor(dark)
            };

            // Tick mark hangs down from the bottom of the tick lane
            painter.line_segment(
                [
                    Pos2::new(x, tick_rect.bottom() - tick_height),
                    Pos2::new(x, tick_rect.bottom()),
                ],
                Stroke::new(1.0, tick_color),
            );

            // Label for major ticks — above tick marks
            if is_major {
                let label = format_timestamp(tick, tick_config, use_local);
                painter.text(
                    Pos2::new(x, tick_rect.bottom() - tick_height),
                    egui::Align2::CENTER_BOTTOM,
                    label,
                    style::tick_font(),
                    tl_colors::tick_label(dark),
                );
            }
        }

        tick += minor_interval;
    }
}

/// Draw the playback-position cursor ("the needle") — a neutral marker
/// deliberately distinct from the red now/live family, so "where I'm looking"
/// never reads as "now". The now-line itself is owned by [`super::now_edge`].
pub(super) fn render_playback_cursor(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    selected_ts: f64,
) {
    let overlay_rect = &frame.rects.overlay;
    let sel_x = frame.ts_to_x(selected_ts);
    if sel_x < overlay_rect.left() || sel_x > overlay_rect.right() {
        return;
    }

    let marker_color = tl_colors::selection(frame.dark);
    painter.line_segment(
        [
            Pos2::new(sel_x, overlay_rect.top()),
            Pos2::new(sel_x, overlay_rect.bottom()),
        ],
        Stroke::new(2.0, marker_color),
    );

    let triangle = vec![
        Pos2::new(sel_x - 5.0, overlay_rect.top()),
        Pos2::new(sel_x + 5.0, overlay_rect.top()),
        Pos2::new(sel_x, overlay_rect.top() + 8.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        triangle,
        marker_color,
        Stroke::NONE,
    ));
}
