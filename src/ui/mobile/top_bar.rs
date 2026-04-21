//! Compact mobile top bar.
//!
//! Replaces the desktop top bar on mobile: drops the sidebar toggles, version
//! stamp, help button, and view-mode switcher (all irrelevant on mobile), and
//! trims to the essentials: mode accent, app mode badge, site chip, alerts,
//! and worker error banner.

use crate::state::{AppMode, AppState};
use eframe::egui::{self, Color32, Frame, RichText};

pub(crate) fn render_mobile_top_bar(ctx: &egui::Context, state: &mut AppState) {
    // Same mode accent bar as desktop — the colored stripe doubles as the
    // app icon / status indicator.
    egui::TopBottomPanel::top("mobile_mode_accent")
        .resizable(false)
        .exact_height(3.0)
        .frame(Frame::NONE.fill(state.app_mode.color()))
        .show(ctx, |ui| {
            ui.allocate_space(ui.available_size());
        });

    egui::TopBottomPanel::top("mobile_top_bar")
        .exact_height(44.0)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                // Site chip — primary control, tapping opens the site modal.
                let site_label = format!(
                    "{} {}",
                    egui_phosphor::regular::MAP_PIN,
                    state.viz_state.site_id
                );
                if ui
                    .button(RichText::new(&site_label).size(14.0).strong())
                    .clicked()
                {
                    state.site_modal_open = true;
                }

                ui.separator();

                // Compact mode badge (Idle / Archive / Live).
                let mode = state.app_mode;
                let color = mode.color();
                let icon = match mode {
                    AppMode::Idle => egui_phosphor::regular::PAUSE_CIRCLE,
                    AppMode::Archive => egui_phosphor::regular::ARCHIVE_BOX,
                    AppMode::Live => egui_phosphor::regular::BROADCAST,
                };
                ui.label(RichText::new(icon).size(14.0).color(color));

                // Alerts chip (reuses the desktop helper).
                super::super::top_bar::render_alerts_chip(ui, state);

                // Worker error banner — critical, must be visible on mobile too.
                if let Some(ref error_msg) = state.worker_init_error {
                    let error_color = Color32::from_rgb(220, 60, 60);
                    ui.label(
                        RichText::new(egui_phosphor::regular::WARNING)
                            .size(14.0)
                            .color(error_color),
                    );
                    ui.label(RichText::new(error_msg).size(11.0).color(error_color));
                    if ui
                        .small_button(RichText::new("Retry").size(11.0).color(error_color))
                        .clicked()
                    {
                        state.push_command(crate::state::AppCommand::RetryWorker);
                    }
                }
            });
        });
}
