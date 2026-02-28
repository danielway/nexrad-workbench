//! Top bar UI: app title, status, and site context.

use super::colors::live;
use crate::state::{AppState, LivePhase};
use eframe::egui::{self, Color32, RichText};

pub fn render_top_bar(ctx: &egui::Context, state: &mut AppState) {
    egui::TopBottomPanel::top("top_bar")
        .exact_height(36.0)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                // App title
                ui.label(
                    RichText::new("NEXRAD Workbench")
                        .strong()
                        .size(16.0)
                        .color(Color32::WHITE),
                );

                ui.separator();

                // Site context button — opens site selection modal
                let site_label = format!("Site: {}", state.viz_state.site_id);
                if ui
                    .button(RichText::new(&site_label).size(14.0).strong())
                    .on_hover_text("Click to change radar site")
                    .clicked()
                {
                    state.site_modal_open = true;
                }

                ui.separator();

                // Show live status or regular status message
                if state.live_mode_state.is_active() {
                    render_live_status(ui, state);
                } else {
                    ui.label(
                        RichText::new(&state.status_message)
                            .size(13.0)
                            .color(Color32::GRAY),
                    );
                }
            });
        });
}

/// Render live mode status in the top bar.
fn render_live_status(ui: &mut egui::Ui, state: &AppState) {
    let phase = state.live_mode_state.phase;
    let pulse_alpha = state.live_mode_state.pulse_alpha();

    let now = state.playback_state.playback_position();

    match phase {
        LivePhase::AcquiringLock => {
            let pulsed_color = Color32::from_rgba_unmultiplied(
                live::ACQUIRING.r(),
                live::ACQUIRING.g(),
                live::ACQUIRING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(RichText::new("\u{2022}").size(16.0).color(pulsed_color));

            let elapsed = state.live_mode_state.phase_elapsed_secs(now) as i32;
            ui.label(
                RichText::new(format!("Acquiring lock... {}s", elapsed))
                    .size(13.0)
                    .color(live::ACQUIRING),
            );
        }
        LivePhase::Streaming | LivePhase::WaitingForChunk => {
            let pulsed_color = Color32::from_rgba_unmultiplied(
                live::STREAMING.r(),
                live::STREAMING.g(),
                live::STREAMING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(RichText::new("\u{2022}").size(16.0).color(pulsed_color));
            ui.label(
                RichText::new("LIVE")
                    .size(13.0)
                    .strong()
                    .color(live::STREAMING),
            );

            let status = if phase == LivePhase::Streaming {
                format!(
                    "({} chunks) receiving...",
                    state.live_mode_state.chunks_received
                )
            } else if let Some(remaining) = state.live_mode_state.countdown_remaining_secs(now) {
                format!(
                    "({} chunks) next in {}s",
                    state.live_mode_state.chunks_received,
                    remaining.ceil() as i32
                )
            } else {
                format!("({} chunks)", state.live_mode_state.chunks_received)
            };

            ui.label(
                RichText::new(status)
                    .size(12.0)
                    .color(Color32::from_rgb(180, 180, 180)),
            );
        }
        _ => {}
    }
}
