//! The "now" affordance — the live edge of the timeline.
//!
//! A single element does all three jobs: it *represents* now, it *is* the
//! streaming indicator, and it *is* the control to start/stop streaming.
//!
//! - When "now" is within the visible window it renders as a vertical
//!   now-line topped by a clickable cap. Three states: `◉ GO LIVE` when no
//!   stream is running (click starts one), `◉ LIVE` while streaming with
//!   the playhead attached (click stops the stream), and `◉ REJOIN` while
//!   the stream ingests in the background with the playhead detached
//!   (click re-pins instantly — no re-acquisition).
//! - When "now" is scrolled off-screen (browsing the archive) it collapses
//!   into a chip pinned to the timeline edge that points back toward now.
//!   Clicking the chip scrolls the view to now and attaches to live in one
//!   action (instant if a stream is already running).
//!
//! Red is reserved exclusively for this concept: muted red ([`NOW_IDLE`]) as
//! an invitation, bright pulsing red ([`LIVE_ACTIVE`]) while live.

use crate::core::Intent;
use crate::core::LiveExitReason;
use crate::core::PlaybackSpeed;
use crate::state::AppState;
use crate::ui::colors::timeline::{LIVE_ACTIVE, NOW_IDLE};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Sense, Stroke, StrokeKind};

/// Render the now affordance and handle its interaction.
///
/// Returns the interactive rect (cap or chip) when one was drawn, so the
/// caller can suppress an ordinary timeline seek when the click landed on it.
pub(super) fn render_now_affordance(
    ui: &mut egui::Ui,
    painter: &Painter,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    frame: &super::TimelineFrame<'_>,
) -> Option<Rect> {
    let overlay_rect = &frame.rects.overlay;
    let now_ts = frame.now_secs;
    let now_x = frame.ts_to_x(now_ts);
    let live_state = if live.is_detached(&playback.state) {
        NowCapState::Detached
    } else if live.mode_state.is_active() {
        NowCapState::Attached
    } else {
        NowCapState::Idle
    };
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
            live_state,
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
            live_state,
            on_left,
        )
    }
}

/// The three states of the live-edge affordance.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NowCapState {
    /// No stream session.
    Idle,
    /// Streaming with the playhead attached (pinned to now or replaying).
    Attached,
    /// Streaming in the background with the playhead detached (browsing).
    Detached,
}

impl NowCapState {
    fn is_streaming(self) -> bool {
        !matches!(self, NowCapState::Idle)
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
    live_state: NowCapState,
    pulse: f32,
) -> Option<Rect> {
    let is_live = live_state.is_streaming();
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
    let label = match live_state {
        NowCapState::Attached => format!("{} LIVE", egui_phosphor::regular::BROADCAST),
        NowCapState::Detached => format!("{} REJOIN", egui_phosphor::regular::BROADCAST),
        NowCapState::Idle => format!("{} GO LIVE", egui_phosphor::regular::BROADCAST),
    };
    let cap_rect = cap_geometry(painter, overlay_rect, now_x, &label);

    let resp = ui
        .interact(cap_rect, ui.id().with("now_live_cap"), Sense::click())
        .on_hover_text(match live_state {
            NowCapState::Attached => "Streaming live — click to stop",
            NowCapState::Detached => "Stream running in background — click to rejoin live",
            NowCapState::Idle => "Stream live from now",
        });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let fill = cap_fill(base, is_live, resp.hovered(), pulse);
    draw_cap(painter, cap_rect, &label, fill);

    if resp.clicked() {
        match live_state {
            NowCapState::Attached => stop_live(state, live, playback),
            // Instant re-pin — the stream never stopped.
            NowCapState::Detached => state.push_command(Intent::ReturnToLive),
            NowCapState::Idle => go_live(state, playback),
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
    live_state: NowCapState,
    on_left: bool,
) -> Option<Rect> {
    let is_live = live_state.is_streaming();
    let base = if is_live { LIVE_ACTIVE } else { NOW_IDLE };
    let label = if on_left {
        format!(
            "{} {} LIVE",
            egui_phosphor::regular::ARROW_LEFT,
            egui_phosphor::regular::BROADCAST
        )
    } else {
        format!(
            "{} LIVE {}",
            egui_phosphor::regular::BROADCAST,
            egui_phosphor::regular::ARROW_RIGHT
        )
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
        .on_hover_text(match live_state {
            NowCapState::Detached => "Stream running — jump back to live",
            _ => "Jump to now and stream live",
        });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let fill = cap_fill(base, is_live, resp.hovered(), live.mode_state.pulse_alpha());
    painter.rect_filled(chip_rect, 3.0, fill);
    painter.rect_stroke(
        chip_rect,
        3.0,
        Stroke::new(1.0_f32, edge_color(fill)),
        StrokeKind::Inside,
    );
    let galley = painter.layout_no_wrap(label, font, Color32::WHITE);
    painter.galley(chip_rect.min + pad, galley, Color32::WHITE);

    if resp.clicked() {
        // Scroll the view to now, then attach — one action from anywhere in
        // the archive. Instant when a background stream is already running.
        playback.state.center_view_on(now_ts);
        match live_state {
            NowCapState::Detached => state.push_command(Intent::ReturnToLive),
            NowCapState::Idle => go_live(state, playback),
            NowCapState::Attached => {}
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
        Stroke::new(1.0_f32, edge_color(fill)),
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
    state.push_command(Intent::StartLive);
    playback.state.speed = PlaybackSpeed::Realtime;
}

/// Stop streaming and freeze on the current (last live) frame — drops to
/// Archive. Mirrors the pause/seek exit used elsewhere.
fn stop_live(
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
    live.stop(LiveExitReason::UserStopped);
    playback.state.playing = false;
    // Freeze on the latest (live-edge) frame for a predictable stop, whether we
    // were pinned to now or mid-replay.
    playback
        .state
        .exit_live(crate::core::FreezeAt::Now(state.frame_now.secs()));
    state.status_message = live
        .mode_state
        .last_exit_reason
        .map(|r| r.message().to_string())
        .unwrap_or_default();
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── NowCapState::is_streaming ──────────────────────────────────────
    // Idle is the only non-streaming state.

    #[wasm_bindgen_test]
    fn is_streaming_idle_is_false() {
        assert!(!NowCapState::Idle.is_streaming());
    }

    #[wasm_bindgen_test]
    fn is_streaming_attached_and_detached_are_true() {
        assert!(NowCapState::Attached.is_streaming());
        assert!(NowCapState::Detached.is_streaming());
    }

    // ── cap_fill ───────────────────────────────────────────────────────
    // Only assert the deterministic (alpha == 255, or alpha-only) branches.
    // `from_rgba_unmultiplied` premultiplies r/g/b when 0 < alpha < 255, so
    // those channels are not hand-computable; alpha itself is preserved.

    #[wasm_bindgen_test]
    fn cap_fill_idle_unhovered_returns_base() {
        // Not live, not hovered → returns `base` unchanged.
        let out = cap_fill(NOW_IDLE, false, false, 0.5);
        assert!(out.to_array() == [200, 95, 95, 255]);

        // base is whatever was passed in — confirm with LIVE_ACTIVE too.
        let out2 = cap_fill(LIVE_ACTIVE, false, false, 0.0);
        assert!(out2.to_array() == [255, 80, 80, 255]);
    }

    #[wasm_bindgen_test]
    fn cap_fill_idle_hovered_brightens_to_live_active() {
        // Not live but hovered → LIVE_ACTIVE regardless of `base`/`pulse`.
        let out = cap_fill(NOW_IDLE, false, true, 0.5);
        assert!(out.to_array() == [255, 80, 80, 255]);
    }

    #[wasm_bindgen_test]
    fn cap_fill_live_full_pulse_is_opaque_live_active() {
        // is_live=true, pulse=1.0 → alpha = (200 + 55) = 255 → opaque
        // LIVE_ACTIVE (no premultiplication on the 255 fast-path).
        // hovered is ignored once is_live.
        let out = cap_fill(NOW_IDLE, true, false, 1.0);
        assert!(out.to_array() == [255, 80, 80, 255]);
    }

    #[wasm_bindgen_test]
    fn cap_fill_live_alpha_tracks_pulse() {
        // alpha = (200.0 + 55.0 * pulse) as u8. Assert the alpha channel only,
        // which is preserved exactly even when r/g/b get premultiplied.
        assert!(cap_fill(NOW_IDLE, true, false, 0.0).a() == 200);
        assert!(cap_fill(NOW_IDLE, true, true, 0.0).a() == 200);
        // 200 + 55*0.5 = 227.5 → truncates to 227.
        assert!(cap_fill(NOW_IDLE, true, false, 0.5).a() == 227);
    }

    // ── edge_color ─────────────────────────────────────────────────────
    // Alpha is fixed at 220; channels are `c * 6 / 10` (u16 math) then
    // premultiplied, so only assert deterministic cases.

    #[wasm_bindgen_test]
    fn edge_color_alpha_is_fixed_220() {
        assert!(edge_color(Color32::from_rgb(255, 80, 80)).a() == 220);
        assert!(edge_color(Color32::from_rgb(0, 0, 0)).a() == 220);
        assert!(edge_color(Color32::from_rgb(123, 200, 17)).a() == 220);
    }

    #[wasm_bindgen_test]
    fn edge_color_of_black_is_black_with_alpha() {
        // 0 * 6 / 10 = 0 for every channel; premultiplying 0 stays 0.
        let out = edge_color(Color32::from_rgb(0, 0, 0));
        assert!(out.to_array() == [0, 0, 0, 220]);
    }

    #[wasm_bindgen_test]
    fn edge_color_is_darker_than_input() {
        // The border is a 6/10 shade of the fill: the *unmultiplied* channels
        // must be no brighter than the source. Use to_srgba_unmultiplied to
        // undo premultiplication and compare straight channel values.
        let fill = Color32::from_rgb(200, 100, 50);
        let edge = edge_color(fill);
        let [er, eg, eb, _] = edge.to_srgba_unmultiplied();
        // 200*6/10 = 120, 100*6/10 = 60, 50*6/10 = 30 (pre-premult targets).
        // Allow ±2 for sRGB premult/unmult round-trip rounding.
        assert!((er as i32 - 120).abs() <= 2);
        assert!((eg as i32 - 60).abs() <= 2);
        assert!((eb as i32 - 30).abs() <= 2);
    }
}
