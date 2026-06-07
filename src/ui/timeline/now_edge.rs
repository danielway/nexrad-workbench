//! The "now" affordance — the live edge of the timeline.
//!
//! A single element does all three jobs: it *represents* now, it *is* the
//! streaming indicator, and it *is* the control to start/stop streaming.
//!
//! - When "now" is within the visible window it renders as a vertical
//!   now-line topped by a clickable cap (`◉ GO LIVE` when idle, `◉ LIVE`
//!   while streaming). Clicking the cap starts streaming; clicking it while
//!   live stops.
//! - When "now" is scrolled off-screen (browsing the archive) it collapses
//!   into a chip pinned to the timeline edge that points back toward now.
//!   Clicking the chip scrolls the view to now and starts streaming in one
//!   action.
//!
//! Red is reserved exclusively for this concept: muted red ([`NOW_IDLE`]) as
//! an invitation, bright pulsing red ([`LIVE_ACTIVE`]) while live.

use super::current_timestamp_secs;
use crate::state::{AppCommand, AppState, LiveExitReason, PlaybackSpeed};
use crate::ui::colors::timeline::{LIVE_ACTIVE, NOW_IDLE};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Sense, Stroke, StrokeKind};

/// Render the now affordance and handle its interaction.
///
/// Returns the interactive rect (cap or chip) when one was drawn, so the
/// caller can suppress an ordinary timeline seek when the click landed on it.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_now_affordance(
    ui: &mut egui::Ui,
    painter: &Painter,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    overlay_rect: &Rect,
    view_start: f64,
    zoom: f64,
) -> Option<Rect> {
    let now_ts = current_timestamp_secs();
    let now_x = overlay_rect.left() + ((now_ts - view_start) * zoom) as f32;
    let is_live = live.mode_state.is_active();
    let pulse = live.mode_state.pulse_alpha();

    if now_x >= overlay_rect.left() && now_x <= overlay_rect.right() {
        render_inline_now(
            ui,
            painter,
            state,
            live,
            playback,
            overlay_rect,
            now_x,
            is_live,
            pulse,
        )
    } else {
        // "Now" is off-screen — pin a "jump to live" chip to the nearest edge,
        // pointing back toward now.
        let on_left = now_x < overlay_rect.left();
        render_edge_chip(
            ui,
            painter,
            state,
            live,
            playback,
            overlay_rect,
            now_ts,
            is_live,
            on_left,
        )
    }
}

/// Draw the inline now-line plus its clickable cap. Returns the cap rect.
#[allow(clippy::too_many_arguments)]
fn render_inline_now(
    ui: &mut egui::Ui,
    painter: &Painter,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    overlay_rect: &Rect,
    now_x: f32,
    is_live: bool,
    pulse: f32,
) -> Option<Rect> {
    let base = if is_live { LIVE_ACTIVE } else { NOW_IDLE };

    // Faint wash over the "future" region (right of now) so the live edge —
    // the boundary between recorded history and not-yet-existing time — reads
    // at a glance even before the eye finds the line.
    let future = Rect::from_min_max(Pos2::new(now_x, overlay_rect.top()), overlay_rect.max);
    if future.width() > 0.5 {
        painter.rect_filled(
            future,
            0.0,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 10),
        );
    }

    // The now-line itself: bold and pulsing while live, calmer when idle.
    let (stroke_w, line_alpha) = if is_live {
        (2.5_f32, (150.0 + 105.0 * pulse) as u8)
    } else {
        (1.5_f32, 150_u8)
    };
    painter.line_segment(
        [
            Pos2::new(now_x, overlay_rect.top()),
            Pos2::new(now_x, overlay_rect.bottom()),
        ],
        Stroke::new(
            stroke_w,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), line_alpha),
        ),
    );

    // The cap — the clickable handle. Always present when now is in view, so
    // the affordance is glanceable and its hit target no longer depends on an
    // invisible, zoom-dependent band.
    let label = if is_live {
        format!("{} LIVE", egui_phosphor::regular::BROADCAST)
    } else {
        format!("{} GO LIVE", egui_phosphor::regular::BROADCAST)
    };
    let cap_rect = cap_geometry(painter, overlay_rect, now_x, &label);

    let resp = ui
        .interact(cap_rect, ui.id().with("now_live_cap"), Sense::click())
        .on_hover_text(if is_live {
            "Streaming live — click to stop"
        } else {
            "Stream live from now"
        });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let fill = cap_fill(base, is_live, resp.hovered(), pulse);
    draw_cap(painter, cap_rect, &label, fill);

    if resp.clicked() {
        if is_live {
            stop_live(state, live, playback);
        } else {
            go_live(state, playback);
        }
    }

    Some(cap_rect)
}

/// Draw the off-screen "jump to live" chip pinned to a timeline edge. Returns
/// the chip rect.
#[allow(clippy::too_many_arguments)]
fn render_edge_chip(
    ui: &mut egui::Ui,
    painter: &Painter,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    overlay_rect: &Rect,
    now_ts: f64,
    is_live: bool,
    on_left: bool,
) -> Option<Rect> {
    let base = if is_live { LIVE_ACTIVE } else { NOW_IDLE };
    let label = if on_left {
        format!("\u{2190} {} LIVE", egui_phosphor::regular::BROADCAST)
    } else {
        format!("{} LIVE \u{2192}", egui_phosphor::regular::BROADCAST)
    };

    // Anchor the chip flush against the edge that points toward now.
    let (galley_size, font) = label_size(painter, &label);
    let pad = egui::vec2(6.0, 2.5);
    let size = galley_size + pad * 2.0;
    let left = if on_left {
        overlay_rect.left() + 4.0
    } else {
        overlay_rect.right() - 4.0 - size.x
    };
    let chip_rect = Rect::from_min_size(Pos2::new(left, overlay_rect.top() + 1.0), size);

    let resp = ui
        .interact(chip_rect, ui.id().with("now_live_chip"), Sense::click())
        .on_hover_text("Jump to now and stream live");
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let fill = cap_fill(base, is_live, resp.hovered(), live.mode_state.pulse_alpha());
    painter.rect_filled(chip_rect, 3.0, fill);
    painter.rect_stroke(
        chip_rect,
        3.0,
        Stroke::new(1.0, edge_color(fill)),
        StrokeKind::Inside,
    );
    let galley = painter.layout_no_wrap(label, font, Color32::WHITE);
    painter.galley(chip_rect.min + pad, galley, Color32::WHITE);

    if resp.clicked() {
        // Scroll the view to now, then stream — one action from anywhere in
        // the archive (the chosen "scroll + stream" behavior).
        playback.state.center_view_on(now_ts);
        if !is_live {
            go_live(state, playback);
        }
    }

    Some(chip_rect)
}

/// Compute the cap rect for a label anchored just beside the now-line, clamped
/// to stay inside the timeline.
fn cap_geometry(painter: &Painter, overlay_rect: &Rect, now_x: f32, label: &str) -> Rect {
    let (galley_size, _) = label_size(painter, label);
    let pad = egui::vec2(6.0, 2.5);
    let size = galley_size + pad * 2.0;
    // Prefer the right of the line; flip left if it would overflow the edge.
    let mut left = now_x + 4.0;
    if left + size.x > overlay_rect.right() {
        left = now_x - 4.0 - size.x;
    }
    left = left.max(overlay_rect.left());
    Rect::from_min_size(Pos2::new(left, overlay_rect.top() + 1.0), size)
}

/// Paint a cap pill (fill + subtle border + white label).
fn draw_cap(painter: &Painter, rect: Rect, label: &str, fill: Color32) {
    let (_, font) = label_size(painter, label);
    painter.rect_filled(rect, 3.0, fill);
    painter.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.0, edge_color(fill)),
        StrokeKind::Inside,
    );
    let pad = egui::vec2(6.0, 2.5);
    let galley = painter.layout_no_wrap(label.to_string(), font, Color32::WHITE);
    painter.galley(rect.min + pad, galley, Color32::WHITE);
}

/// Fill color for a cap/chip: muted red when idle, brightening on hover, and
/// gently pulsing while live.
fn cap_fill(base: Color32, is_live: bool, hovered: bool, pulse: f32) -> Color32 {
    if is_live {
        let a = (200.0 + 55.0 * pulse) as u8;
        Color32::from_rgba_unmultiplied(LIVE_ACTIVE.r(), LIVE_ACTIVE.g(), LIVE_ACTIVE.b(), a)
    } else if hovered {
        LIVE_ACTIVE
    } else {
        base
    }
}

/// A darker shade of a fill, used for a 1px definition border.
fn edge_color(fill: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        (fill.r() as u16 * 6 / 10) as u8,
        (fill.g() as u16 * 6 / 10) as u8,
        (fill.b() as u16 * 6 / 10) as u8,
        220,
    )
}

/// Lay out a label and report its size plus the font used (kept in one place
/// so geometry and painting agree).
fn label_size(painter: &Painter, label: &str) -> (egui::Vec2, egui::FontId) {
    let font = egui::FontId::proportional(10.0);
    let galley = painter.layout_no_wrap(label.to_string(), font.clone(), Color32::WHITE);
    (galley.size(), font)
}

/// Enter live streaming from the current position. `start_live_mode` (run by
/// the `StartLive` command) owns position/lock/playing and bumps zoom +
/// re-centers as needed, so we only queue the command and set realtime speed.
fn go_live(state: &mut AppState, playback: &mut crate::subsystem::Playback) {
    playback.state.clear_selection();
    state.push_command(AppCommand::StartLive);
    playback.state.speed = PlaybackSpeed::Realtime;
}

/// Stop streaming and freeze on the current (last live) frame — drops to
/// Archive. Mirrors the pause/seek exit used elsewhere.
fn stop_live(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
    live.mode_state.stop(LiveExitReason::UserStopped);
    playback.state.time_model.disable_realtime_lock();
    state.status_message = live
        .mode_state
        .last_exit_reason
        .map(|r| r.message().to_string())
        .unwrap_or_default();
}
