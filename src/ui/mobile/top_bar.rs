//! Compact mobile top bar.
//!
//! Replaces the desktop top bar on mobile: drops the sidebar toggles, help
//! button, and view-mode switcher (all irrelevant on mobile), and trims to
//! the essentials: mode accent, app mode badge, site chip, alerts, worker
//! error banner, and a small version stamp in the top-right.

use crate::state::{AppMode, AppState};
use eframe::egui::{self, Align, Color32, Frame, Layout, RichText};

const TOP_BAR_CONTENT_HEIGHT: f32 = 44.0;

pub(crate) fn render_mobile_top_bar(ctx: &egui::Context, state: &mut AppState) {
    // iOS safe area: when installed as a home-screen PWA, the canvas extends
    // under the translucent status bar. Pad the top so OS icons don't
    // overlap our content.
    let (inset_top, _inset_right, _inset_bottom, _inset_left) = super::safe_area_insets();

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
        .exact_height(TOP_BAR_CONTENT_HEIGHT + inset_top)
        .show(ctx, |ui| {
            // Push the top-bar content below the iOS status bar reservation.
            if inset_top > 0.0 {
                ui.add_space(inset_top);
            }
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

                // Version stamp — right-aligned. Useful for cross-referencing
                // a deployed build against git history when reporting bugs.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    const MAX_LEN: usize = 18;
                    let version = env!("NEXRAD_VERSION");
                    let display = if version.len() > MAX_LEN {
                        let mut truncated = String::with_capacity(MAX_LEN + 3);
                        for (i, ch) in version.char_indices() {
                            if i >= MAX_LEN {
                                break;
                            }
                            truncated.push(ch);
                        }
                        truncated.push('\u{2026}');
                        truncated
                    } else {
                        version.to_string()
                    };
                    ui.label(
                        RichText::new(display)
                            .size(10.0)
                            .color(Color32::from_rgb(120, 120, 120)),
                    );
                });
            });
        });
}
