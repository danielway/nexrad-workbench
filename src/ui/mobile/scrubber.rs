//! Compact mobile scrubber.
//!
//! A single-line (28px) horizontal track that:
//!   - Spans the overall data time range on the scrubber width
//!   - Draws scan boundaries as faint tick marks
//!   - Draws the current playback position as a draggable thumb
//!   - In live mode, draws a "now" marker at the right edge
//!
//! Interaction: tap-to-seek, drag-to-scrub. Scrubbing pauses playback so
//! the thumb stays where the user put it.

use crate::state::{AppState, LiveExitReason};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

/// Total scrubber height in egui logical pixels.
pub(super) const SCRUBBER_HEIGHT: f32 = 28.0;

pub(super) fn render_scrubber(ui: &mut egui::Ui, state: &mut AppState) {
    let available_w = ui.available_width();
    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_w, SCRUBBER_HEIGHT),
        Sense::click_and_drag(),
    );
    let full_rect = response.rect;

    // Track rect — a thin bar with a few pixels of vertical padding so the
    // hit area extends above and below for easier touch targeting.
    let track_y = full_rect.center().y;
    let track_rect = Rect::from_min_max(
        Pos2::new(full_rect.left() + 8.0, track_y - 2.0),
        Pos2::new(full_rect.right() - 8.0, track_y + 2.0),
    );
    let dark = state.is_dark;
    let bg = if dark {
        Color32::from_rgb(40, 40, 50)
    } else {
        Color32::from_rgb(220, 220, 225)
    };
    let fg = if dark {
        Color32::from_rgb(100, 140, 220)
    } else {
        Color32::from_rgb(70, 110, 200)
    };
    painter.rect_filled(track_rect, 2.0, bg);

    // Find the time range to render. Prefer actual data range when it exists;
    // otherwise fall back to a 1-hour window centered on `now` so the track
    // isn't zero-width.
    let (t_start, t_end) = match state.radar_timeline.overall_time_range() {
        Some((s, e)) if e > s => (s, e),
        _ => {
            let now = js_sys::Date::now() / 1000.0;
            (now - 1800.0, now + 1800.0)
        }
    };
    let span = t_end - t_start;

    let ts_to_x = |ts: f64| -> f32 {
        let t = ((ts - t_start) / span).clamp(0.0, 1.0) as f32;
        track_rect.left() + t * track_rect.width()
    };
    let x_to_ts = |x: f32| -> f64 {
        let t = ((x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0) as f64;
        t_start + t * span
    };

    // Faint ticks for each scan so the user sees where data exists.
    for scan in &state.radar_timeline.scans {
        let scan_ts = scan.start_time;
        if scan_ts < t_start || scan_ts > t_end {
            continue;
        }
        let x = ts_to_x(scan_ts);
        painter.line_segment(
            [
                Pos2::new(x, track_rect.top() - 3.0),
                Pos2::new(x, track_rect.bottom() + 3.0),
            ],
            Stroke::new(1.0, Color32::from_rgb(120, 130, 140)),
        );
    }

    let playback_ts = state.playback_state.playback_position();

    // Filled "played" region from start to thumb.
    let thumb_x = ts_to_x(playback_ts);
    let filled = Rect::from_min_max(track_rect.min, Pos2::new(thumb_x, track_rect.max.y));
    painter.rect_filled(filled, 2.0, fg);

    // "Now" marker in live mode.
    if state.live_mode_state.is_active() {
        let now = js_sys::Date::now() / 1000.0;
        if now >= t_start && now <= t_end {
            let x = ts_to_x(now);
            let color = Color32::from_rgb(220, 60, 60);
            painter.line_segment(
                [
                    Pos2::new(x, full_rect.top() + 2.0),
                    Pos2::new(x, full_rect.bottom() - 2.0),
                ],
                Stroke::new(1.5, color),
            );
        }
    }

    // Thumb — drawn last so it sits on top of the fill and ticks.
    let thumb_r = 8.0;
    painter.circle_filled(
        Pos2::new(thumb_x, track_y),
        thumb_r,
        ui.visuals().strong_text_color(),
    );
    painter.circle_stroke(Pos2::new(thumb_x, track_y), thumb_r, Stroke::new(1.5, fg));

    // Outline for the whole hit area (subtle so it reads as draggable).
    painter.rect_stroke(
        full_rect,
        4.0,
        Stroke::new(0.5, Color32::from_rgba_unmultiplied(128, 128, 128, 40)),
        StrokeKind::Inside,
    );

    // Interaction: drag or click to seek.
    let interact_pos = response
        .interact_pointer_pos()
        .filter(|_| response.dragged() || response.clicked());
    if let Some(pos) = interact_pos {
        let new_ts = x_to_ts(pos.x);
        // Exit live mode when the user seeks manually.
        if state.live_mode_state.is_active() {
            state.live_mode_state.stop(LiveExitReason::UserSeeked);
            state.playback_state.time_model.disable_realtime_lock();
        }
        // Scrubbing pauses playback so the thumb stays where the user dropped
        // it — otherwise a running playback loop would immediately snap it
        // forward on the next frame.
        if response.dragged() {
            state.playback_state.playing = false;
        }
        state.playback_state.set_playback_position(new_ts);
    }
}
