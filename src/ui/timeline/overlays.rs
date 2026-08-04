//! Overlay rendering: saved events.
//!
//! Download/realtime/ghost rendering moved to the frames-first cell painter
//! ([`super::frame_cells`]) and the Macro track ([`super::scan_track`]) so live
//! and settled cells share one code path. This module now carries only the
//! saved-event overlay, drawn in neutral with a distinct bookmark shape (the
//! accent budget reserves color for the playhead / live edge / active ring).

use super::TimelineFrame;
use crate::state::SavedEvents;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Painter, Pos2, Rect, Stroke};

/// Render saved event overlays on the timeline: a faint neutral band, neutral
/// boundary lines, and a small bookmark tick at the start so events read by
/// SHAPE (not an amber accent fill). The name label disambiguates.
pub(super) fn render_saved_events(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    saved_events: &SavedEvents,
    current_site: &str,
) {
    let overlay_rect = &frame.rects.overlay;
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);

    for event in saved_events.events.iter() {
        if event.site_id != current_site {
            continue;
        }

        let start_x = ts_to_x(event.start_time);
        let end_x = ts_to_x(event.end_time);

        // Skip if entirely outside the visible area
        if end_x < overlay_rect.left() || start_x > overlay_rect.right() {
            continue;
        }

        let visible_start = start_x.max(overlay_rect.left());
        let visible_end = end_x.min(overlay_rect.right());

        // Faint neutral fill band
        let event_rect = Rect::from_min_max(
            Pos2::new(visible_start, overlay_rect.top()),
            Pos2::new(visible_end, overlay_rect.bottom()),
        );
        painter.rect_filled(event_rect, 0.0, tl_colors::event_fill());

        // Boundary lines (neutral)
        let border_color = tl_colors::event_border();
        if start_x >= overlay_rect.left() && start_x <= overlay_rect.right() {
            painter.line_segment(
                [
                    Pos2::new(start_x, overlay_rect.top()),
                    Pos2::new(start_x, overlay_rect.bottom()),
                ],
                Stroke::new(1.0_f32, border_color),
            );
            // Bookmark tick: a small downward pennant at the start so the event
            // reads by shape even in grayscale.
            let tip_x = start_x;
            let pts = vec![
                Pos2::new(tip_x, overlay_rect.top()),
                Pos2::new(tip_x + 6.0, overlay_rect.top()),
                Pos2::new(tip_x + 3.0, overlay_rect.top() + 5.0),
            ];
            painter.add(egui::Shape::convex_polygon(pts, border_color, Stroke::NONE));
        }
        if end_x >= overlay_rect.left() && end_x <= overlay_rect.right() {
            painter.line_segment(
                [
                    Pos2::new(end_x, overlay_rect.top()),
                    Pos2::new(end_x, overlay_rect.bottom()),
                ],
                Stroke::new(1.0_f32, border_color),
            );
        }

        // Event name label (at top of the rectangle, clipped to visible)
        let label_width = visible_end - visible_start;
        if label_width > 20.0 {
            let label_x = ((start_x + end_x) / 2.0)
                .clamp(overlay_rect.left() + 10.0, overlay_rect.right() - 10.0);
            painter.text(
                Pos2::new(label_x, overlay_rect.top() + 2.0),
                egui::Align2::CENTER_TOP,
                &event.name,
                egui::FontId::proportional(9.0),
                tl_colors::event_label(),
            );
        }
    }
}
