//! Ruler rendering: tick marks and playback cursor.

use super::{current_timestamp_secs, format_timestamp, TickConfig};
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke};

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

/// Draw the playback position cursor (selection marker) and "now" wall-clock marker.
///
/// `is_live` is true while streaming (renders the now-line as a bold LIVE status
/// marker); `at_edge` is true when the cursor sits within the live-edge band but
/// is not yet streaming (renders a "click here to go live" call-to-action pill).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_playback_cursor(
    painter: &Painter,
    overlay_rect: &Rect,
    selected_ts: f64,
    view_start: f64,
    zoom: f64,
    is_live: bool,
    at_edge: bool,
) {
    let ts_to_x = |ts: f64| -> f32 { overlay_rect.left() + ((ts - view_start) * zoom) as f32 };

    // Selection marker (playback position indicator)
    {
        let sel_x = ts_to_x(selected_ts);

        if sel_x >= overlay_rect.left() && sel_x <= overlay_rect.right() {
            let marker_color = tl_colors::SELECTION;

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
    }

    // "Now" marker (current wall-clock time)
    {
        let now_ts = current_timestamp_secs();
        let now_x = ts_to_x(now_ts);

        if now_x >= overlay_rect.left() && now_x <= overlay_rect.right() {
            if is_live {
                // Streaming: bold solid LIVE status line + broadcast LIVE badge.
                let live_color = tl_colors::LIVE_ACTIVE;
                painter.line_segment(
                    [
                        Pos2::new(now_x, overlay_rect.top()),
                        Pos2::new(now_x, overlay_rect.bottom()),
                    ],
                    Stroke::new(2.0, live_color),
                );
                let live_label = format!("{} LIVE", egui_phosphor::regular::BROADCAST);
                draw_now_pill(painter, overlay_rect, now_x, &live_label, live_color);
            } else {
                // Archive/Idle: subtle crosshair "now" marker.
                let now_color = tl_colors::NOW_MARKER;
                painter.line_segment(
                    [
                        Pos2::new(now_x, overlay_rect.top()),
                        Pos2::new(now_x, overlay_rect.top() + 4.0),
                    ],
                    Stroke::new(1.5, now_color),
                );
                painter.line_segment(
                    [
                        Pos2::new(now_x, overlay_rect.bottom() - 4.0),
                        Pos2::new(now_x, overlay_rect.bottom()),
                    ],
                    Stroke::new(1.5, now_color),
                );
                painter.line_segment(
                    [
                        Pos2::new(now_x, overlay_rect.top() + 4.0),
                        Pos2::new(now_x, overlay_rect.bottom() - 4.0),
                    ],
                    Stroke::new(
                        0.5,
                        Color32::from_rgba_unmultiplied(
                            now_color.r(),
                            now_color.g(),
                            now_color.b(),
                            100,
                        ),
                    ),
                );
                let d = 3.0;
                let diamond = vec![
                    Pos2::new(now_x, overlay_rect.bottom() - d),
                    Pos2::new(now_x + d, overlay_rect.bottom()),
                    Pos2::new(now_x, overlay_rect.bottom() + d),
                    Pos2::new(now_x - d, overlay_rect.bottom()),
                ];
                painter.add(egui::Shape::convex_polygon(
                    diamond,
                    now_color,
                    Stroke::NONE,
                ));

                // When parked at the live edge, prompt the user that clicking
                // here goes live (the click is handled by the live-edge band in
                // handle_timeline_interaction).
                if at_edge {
                    let cta_label = format!("{} LIVE", egui_phosphor::regular::PLAY);
                    draw_now_pill(
                        painter,
                        overlay_rect,
                        now_x,
                        &cta_label,
                        tl_colors::LIVE_PILL_CTA,
                    );
                }
            }
        }
    }
}

/// Draw a small labeled pill anchored at the now-line, near the top of the
/// timeline. Used for both the "click to go live" CTA and the live status badge.
fn draw_now_pill(painter: &Painter, overlay_rect: &Rect, now_x: f32, text: &str, color: Color32) {
    let font = egui::FontId::proportional(9.0);
    let galley = painter.layout_no_wrap(text.to_string(), font, Color32::WHITE);
    let pad = egui::vec2(5.0, 1.5);
    let size = galley.size() + pad * 2.0;
    // Anchor just right of the line so it doesn't cover the marker itself,
    // clamped to stay within the timeline.
    let mut left = now_x + 3.0;
    if left + size.x > overlay_rect.right() {
        left = now_x - 3.0 - size.x;
    }
    let rect = Rect::from_min_size(Pos2::new(left, overlay_rect.top() + 1.0), size);
    painter.rect_filled(rect, 3.0, color);
    painter.galley(rect.min + pad, galley, Color32::WHITE);
}
