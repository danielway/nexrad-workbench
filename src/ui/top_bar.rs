//! Top bar UI: app title and status.

use crate::state::AppState;
use eframe::egui::{self, Color32, RichText};

pub fn render_top_bar(ctx: &egui::Context, state: &AppState) {
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

                // Status text
                ui.label(
                    RichText::new(&state.status_message)
                        .size(13.0)
                        .color(Color32::GRAY),
                );
            });
        });
}
