//! Bottom panel UI: orchestrates the timeline, playback controls, and session statistics.

use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::state::AppState;
use crate::subsystem::Acquisition;
use eframe::egui;

use super::playback_controls::render_playback_controls;
use super::timeline::render_timeline;

/// Desktop bottom panel: timeline + playback controls + session stats.
///
/// Only rendered in the desktop layout — the mobile layout omits this
/// layer entirely and instead places `MobileChromeLayer` at the bottom.
/// The per-frame pulse-animation tick and the playback tick run in the
/// main update loop so they run in both layouts.
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
            ctx.root_ui,
            ctx.state,
            ctx.timeline,
            ctx.sites,
            ctx.live,
            ctx.playback,
            ctx.acquisition,
            ctx.derived,
            ctx.chrome,
            ctx.activity_vm,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bottom_panel(
    root_ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    sites: &mut crate::subsystem::Sites,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    acquisition: &mut Acquisition,
    derived: &crate::subsystem::Derived,
    chrome: &mut crate::subsystem::Chrome,
    activity_vm: &crate::core::activity::ActivityVm,
) {
    let ctx = root_ui.ctx().clone();

    // Handle spacebar to toggle playback (only when no text input is focused).
    // Decoupled from the stream: while tethered this freezes the live feed
    // (pause-while-tethered); going live is handled by the LIVE button /
    // now-line cap / the `L` key, not the spacebar.
    let space_pressed = ctx.input(|i| i.key_pressed(egui::Key::Space) && !i.modifiers.any());
    let has_focus = ctx.memory(|m| m.focused().is_some());
    // Suppressed under a dialog or popover (core::dialogs).
    if space_pressed && !has_focus && chrome.dialogs.allows(None) {
        state.push_command(crate::core::Intent::TogglePlayPause);
    }

    // Playback advances in the main loop (core::playback_manager::
    // playback_tick), so it runs in the phone layout too.

    // Sized to the content layout (timeline + spacing + controls ~24 +
    // frame margins). Stays constant across macro/micro because the
    // timeline locks its own height in render_timeline.
    let controls_height = super::timeline::style::TIMELINE_TOTAL_H + 35.0;
    let top_bar_height = 36.0;
    let min_central_height = 100.0;
    let max_panel_height =
        ctx.input(|i| i.viewport_rect().height()) - top_bar_height - min_central_height;

    // The panel is exactly the transport row plus the timeline strip. The
    // resizable dev acquisition drawer that used to stack above it is gone —
    // its queue and network content lives in the activity sheet now.
    let _ = max_panel_height;
    let total_height = controls_height;

    egui::Panel::bottom("bottom_panel")
        .exact_size(total_height)
        .show(root_ui, |ui| {
            ui.vertical(|ui| {
                // Transport/controls row ABOVE the timeline strip (spec §5
                // bottom cluster: "transport row … above the timeline").
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    render_playback_controls(ui, state, live, playback, activity_vm);
                });

                ui.add_space(2.0);

                // Timeline strip row. A scrub seeks once the strip is
                // drawn, over every site's stream.
                let scrub_to = render_timeline(
                    ui,
                    state,
                    timeline,
                    sites.focus(),
                    live,
                    playback,
                    acquisition,
                    derived,
                    chrome,
                );
                if let Some(ts) = scrub_to {
                    live.seek(
                        sites,
                        &mut playback.state,
                        state.frame_now.secs(),
                        state.pause_stream_while_reviewing,
                        ts,
                        crate::core::transport::SeekGesture::Scrub,
                    );
                }
            });
        });

    // Performance diagnostics live in the activity sheet's Details section.
}
