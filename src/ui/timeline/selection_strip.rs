//! The selection strip: the thin row between the tick rail and the frames
//! where ranges are made (round-4 design). Drag along it to draw a range,
//! drag a handle to resize it, drop the right handle on the live edge to pin
//! it to now, click empty strip to clear it. A thin projection of
//! [`crate::core::selection_strip`]: the grab, the drag point and what a
//! release selects are decided there.
//!
//! The strip is its own interact rect (a distinct id over the strip widget),
//! and the frames' press-seek ignores presses inside it, so the two never
//! fight over a drag.

use crate::core::selection_strip::{
    drag_anchor, drag_point, grab_at, intent_for, release, StripGrab, StripRelease,
};
use crate::state::AppState;
use crate::ui::colors::timeline::{self as tl_colors, LIVE_ACTIVE};
use eframe::egui::{self, Pos2, Rect, Sense, Stroke};

/// Painted handle size: a small tab at each end of the range bar.
const HANDLE_W: f32 = 7.0;
/// Height of the range bar inside the strip.
const BAR_H: f32 = 3.0;

/// The press that started the current strip drag, kept in egui memory for
/// the drag's lifetime.
#[derive(Clone, Copy)]
struct StripDrag {
    grab: StripGrab,
    press_x: f32,
    moved: bool,
}

/// Draw the strip in `rect` and run its gestures.
pub(super) fn render_selection_strip(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    frame: &super::TimelineFrame<'_>,
    rect: Rect,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, tl_colors::selection_strip_bg(frame.dark));

    let response = ui.interact(
        rect,
        ui.id().with("selection_strip"),
        Sense::click_and_drag(),
    );
    let now = frame.now_secs;
    let now_x = frame.ts_to_x(now);
    let stream_active = live.mode_state.is_active();
    let range = playback.state.playback_bounds();
    let pinned = playback.state.is_live_range();
    let edges_px = range.map(|(s, e)| (frame.ts_to_x(s), frame.ts_to_x(e)));
    let point_at = |x: f32| drag_point(frame.x_to_ts(x), now, x - now_x, stream_active);
    let drag_id = response.id.with("drag");

    if response.drag_started_by(egui::PointerButton::Primary) {
        let press_x = ui
            .input(|i| i.pointer.press_origin())
            .map_or(rect.left(), |p| p.x);
        let grab = grab_at(edges_px, press_x);
        if let Some(anchor) = drag_anchor(grab, range, point_at(press_x), pinned, now) {
            playback.state.begin_selection_drag(anchor);
            ui.ctx().memory_mut(|m| {
                m.data.insert_temp(
                    drag_id,
                    StripDrag {
                        grab,
                        press_x,
                        moved: false,
                    },
                )
            });
        }
    }

    let drag: Option<StripDrag> = ui.ctx().memory(|m| m.data.get_temp(drag_id));
    if let (Some(mut d), Some(pos)) = (drag, response.interact_pointer_pos()) {
        if response.dragged() {
            playback.state.update_selection_drag(point_at(pos.x));
            d.moved |= (pos.x - d.press_x).abs() > 2.0;
            ui.ctx().memory_mut(|m| m.data.insert_temp(drag_id, d));
        }
    }

    if response.drag_stopped() {
        if let Some(d) = drag {
            ui.ctx().memory_mut(|m| m.data.remove::<StripDrag>(drag_id));
            let drawn = playback.state.end_selection_drag();
            finish(
                state,
                release(d.grab, drawn, d.moved, pinned, now, stream_active),
                range,
            );
        }
    } else if response.clicked() {
        let x = response.interact_pointer_pos().map_or(rect.left(), |p| p.x);
        finish(
            state,
            release(
                grab_at(edges_px, x),
                None,
                false,
                pinned,
                now,
                stream_active,
            ),
            range,
        );
    }

    let over_handle = response
        .hover_pos()
        .is_some_and(|h| grab_at(edges_px, h.x) != StripGrab::New);
    if over_handle || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    let response = match (range.is_some(), over_handle, response.dragged()) {
        (_, _, true) | (false, _, _) => response,
        (true, true, false) => response
            .on_hover_text("Drag to resize. Drop the right handle on the live edge to pin to now."),
        (true, false, false) => {
            response.on_hover_text("Drag to select a new range. Click to clear this one.")
        }
    };

    paint_range(&painter, frame, rect, playback, response.hovered());
}

/// Push the intent a release maps to (and say why when a range is refused).
fn finish(state: &mut AppState, rel: StripRelease, range: Option<(f64, f64)>) {
    if rel == StripRelease::TooLong {
        state.status_message = format!(
            "Range too long — zoom in and select at most {} days",
            (crate::core::MAX_SELECTION_SPAN_SECS / 86_400.0) as i64
        );
    }
    if let Some(intent) = intent_for(rel, range.is_some()) {
        state.push_command(intent);
    }
}

/// The range bar with a handle at each end; the right handle turns red when
/// the range is pinned to now. Shows the in-progress drag while drawing.
fn paint_range(
    painter: &egui::Painter,
    frame: &super::TimelineFrame<'_>,
    rect: Rect,
    playback: &crate::subsystem::Playback,
    hovered: bool,
) {
    let Some((start, end)) = playback.state.selection_range() else {
        if hovered {
            painter.text(
                Pos2::new(rect.left() + 6.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "drag here to select a range",
                super::style::tick_font(),
                tl_colors::SELECTION_LABEL,
            );
        }
        return;
    };
    let (sx, ex) = (frame.ts_to_x(start), frame.ts_to_x(end));
    if ex < rect.left() || sx > rect.right() {
        return;
    }
    let pinned = playback.state.is_live_range() && !playback.state.selection_in_progress();
    let cy = rect.center().y;
    let bar = Rect::from_min_max(
        Pos2::new(sx.max(rect.left()), cy - BAR_H / 2.0),
        Pos2::new(ex.min(rect.right()), cy + BAR_H / 2.0),
    );
    painter.rect_filled(bar, 0.0, tl_colors::RANGE);
    for (x, color) in [
        (sx, tl_colors::RANGE),
        (
            ex,
            if pinned {
                LIVE_ACTIVE
            } else {
                tl_colors::RANGE
            },
        ),
    ] {
        if x < rect.left() - HANDLE_W || x > rect.right() + HANDLE_W {
            continue;
        }
        let tab =
            Rect::from_center_size(Pos2::new(x, cy), egui::vec2(HANDLE_W, rect.height() - 2.0));
        painter.rect_filled(tab, 2.0, color);
        painter.rect_stroke(
            tab,
            2.0,
            Stroke::new(1.0, color.gamma_multiply(0.6)),
            egui::StrokeKind::Inside,
        );
    }
}
