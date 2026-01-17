//! Top bar UI: app title, status, and data source mode selector.

use crate::state::{AppState, DataSourceMode};
use eframe::egui::{self, Color32, RichText, Ui};

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

                // Status text
                ui.label(
                    RichText::new(&state.status_message)
                        .size(13.0)
                        .color(Color32::GRAY),
                );

                // Right-aligned data source selector
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    render_mode_selector(ui, state);
                });
            });
        });
}

fn render_mode_selector(ui: &mut Ui, state: &mut AppState) {
    let modes = [
        DataSourceMode::UploadFile,
        DataSourceMode::ArchiveBrowser,
        DataSourceMode::RealtimeStream,
    ];

    for mode in modes.iter().rev() {
        let is_selected = state.data_source_mode == *mode;
        let text = RichText::new(mode.label()).size(12.0);

        if ui
            .selectable_label(is_selected, text)
            .on_hover_text(format!("Switch to {} mode", mode.label()))
            .clicked()
        {
            state.data_source_mode = *mode;
        }
    }
}
