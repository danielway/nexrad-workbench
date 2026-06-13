//! Bottom panel UI: orchestrates the timeline, playback controls, and session statistics.

use super::acquisition_drawer::render_acquisition_drawer;
use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::state::{AppState, PlaybackMode};
use crate::subsystem::Acquisition;
use eframe::egui;

use super::playback_controls::render_playback_controls;
use super::timeline::render_timeline;

/// Desktop bottom panel: timeline + playback controls + session stats.
///
/// Only rendered in the desktop layout — the mobile layout omits this
/// layer entirely and instead places `MobileChromeLayer` at the bottom.
/// The per-frame pulse-animation tick is hoisted into the main update
/// loop so it runs in both layouts.
pub(super) struct BottomPanelLayer;

impl Layer for BottomPanelLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        20
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_bottom_panel(
            ctx.ctx,
            ctx.state,
            ctx.timeline,
            ctx.live,
            ctx.playback,
            ctx.acquisition,
            ctx.derived,
            ctx.chrome,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bottom_panel(
    ctx: &egui::Context,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    acquisition: &mut Acquisition,
    derived: &crate::subsystem::Derived,
    chrome: &mut crate::subsystem::Chrome,
) {
    let dt = ctx.input(|i| i.stable_dt);

    // Handle spacebar to toggle playback (only when no text input is focused).
    // Decoupled from the stream: while tethered this freezes the live feed
    // (pause-while-tethered); going live is handled by the LIVE button /
    // now-line cap / Ctrl+L, not the spacebar.
    let space_pressed = ctx.input(|i| i.key_pressed(egui::Key::Space) && !i.modifiers.any());
    let has_focus = ctx.memory(|m| m.focused().is_some());
    if space_pressed && !has_focus {
        super::transport::toggle_play_pause(state, timeline, live, playback);
    }

    // Advance playback position when playing.
    // Cadence preservation across a Micro↔Macro snap is owned by the tier
    // state machine (PlaybackState::set_timeline_zoom / reconcile_tier), so it
    // runs on every transition regardless of `playing` — paused transitions and
    // mobile get it too. Here we only dispatch to the right advance path.
    if playback.state.playing {
        // Effective mode: a lookback replay frame-steps (Macro) regardless of
        // tier, so it dispatches to advance_macro and gets the macro fps speeds.
        match playback.state.effective_playback_mode() {
            PlaybackMode::Micro => playback.state.advance(dt as f64),
            PlaybackMode::Macro => playback.state.advance_macro(dt as f64),
        }

        // Keeping the live edge on-screen now lives in `tick_live`
        // (App::keep_now_on_screen), so it runs in LIVE-NOW too — not just
        // while `playing`.

        // Repaint at 30 FPS while playing — smooth for continuous micro-mode
        // advances and well above the 1–15 FPS frame cadence macro mode emits.
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    // The acquisition drawer is reachable only via the dev-mode-only network
    // metric label. Force it closed when dev mode is off so a previously
    // expanded drawer doesn't linger after the user toggles dev off.
    let drawer_expanded = state.dev_mode && acquisition.state.drawer_expanded;
    // Sized to the content layout (timeline + spacing + controls ~24 +
    // frame margins). Stays constant across macro/micro because the
    // timeline locks its own height in render_timeline.
    let controls_height = super::timeline::style::TIMELINE_TOTAL_H + 35.0;
    let top_bar_height = 36.0;
    let min_central_height = 100.0;
    let max_panel_height =
        ctx.input(|i| i.viewport_rect().height()) - top_bar_height - min_central_height;

    // When the drawer is expanded, a resize handle, separator, and inter-widget
    // spacing are rendered between the drawer and the controls. Account for that
    // overhead so the controls aren't pushed below the window edge.
    let drawer_spacing_overhead = 14.0;
    let drawer_height = if drawer_expanded {
        let max_drawer = (max_panel_height - controls_height - drawer_spacing_overhead).max(0.0);
        acquisition.state.drawer_height.min(max_drawer)
    } else {
        0.0
    };
    let total_height = if drawer_expanded {
        controls_height + drawer_spacing_overhead + drawer_height
    } else {
        controls_height
    };

    egui::TopBottomPanel::bottom("bottom_panel")
        .exact_height(total_height)
        .show(ctx, |ui| {
            // Render acquisition drawer above normal controls when expanded
            if drawer_expanded {
                // Resize handle: thin draggable strip
                let resize_response = ui.allocate_response(
                    egui::Vec2::new(ui.available_width(), 4.0),
                    egui::Sense::drag(),
                );
                if resize_response.dragged() {
                    // Dragging up increases height, dragging down decreases
                    let delta = -resize_response.drag_delta().y;
                    acquisition.state.drawer_height =
                        (acquisition.state.drawer_height + delta).clamp(100.0, 600.0);
                }
                if resize_response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }

                render_acquisition_drawer(
                    ui,
                    state,
                    live,
                    acquisition,
                    drawer_height - 4.0,
                    chrome,
                );
                ui.separator();
            }

            ui.vertical(|ui| {
                // Transport/controls row ABOVE the timeline strip (spec §5
                // bottom cluster: "transport row … above the timeline").
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    render_playback_controls(
                        ui,
                        state,
                        timeline,
                        live,
                        playback,
                        acquisition,
                        chrome,
                    );
                });

                ui.add_space(2.0);

                // Timeline strip row
                render_timeline(ui, state, timeline, live, playback, acquisition, derived);
            });
        });

    // Stats detail is now a proper modal rendered via `StatsModalLayer`.
}
