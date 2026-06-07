//! Ruler rendering: tick marks and playback cursor.

use super::{format_timestamp, TickConfig};
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Painter, Pos2, Rect, Stroke};

/// Draw tick marks (major + minor) and labels in the tick lane.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_tick_marks(
    painter: &Painter,
    tick_rect: &Rect,
    first_tick: i64,
    last_tick: i64,
    minor_interval: i64,
    major_interval: i64,
    tz_offset_secs: i64,
    tick_config: &TickConfig,
    dark: bool,
    use_local: bool,
    view_start: f64,
    zoom: f64,
) {
    let ts_to_x = |ts: f64| -> f32 { tick_rect.left() + ((ts - view_start) * zoom) as f32 };

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
                    egui::FontId::monospace(8.0),
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
    overlay_rect: &Rect,
    selected_ts: f64,
    view_start: f64,
    zoom: f64,
    dark: bool,
) {
    let sel_x = overlay_rect.left() + ((selected_ts - view_start) * zoom) as f32;
    if sel_x < overlay_rect.left() || sel_x > overlay_rect.right() {
        return;
    }

    let marker_color = tl_colors::selection(dark);
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
