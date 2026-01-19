//! Top bar UI: app title, site selection, and status.

use crate::state::AppState;
use eframe::egui::{self, Color32, RichText};

pub fn render_top_bar(ctx: &egui::Context, state: &mut AppState) {
    egui::TopBottomPanel::top("top_bar")
        .exact_height(36.0)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                // Site input
                ui.label(RichText::new("Site:").size(12.0).color(Color32::GRAY));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.viz_state.site_id)
                        .desired_width(50.0)
                        .font(egui::FontId::monospace(12.0)),
                );
                // Convert to uppercase as user types
                if response.changed() {
                    state.viz_state.site_id = state.viz_state.site_id.to_uppercase();
                }

                ui.separator();

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
