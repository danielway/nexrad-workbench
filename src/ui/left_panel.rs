//! Left panel UI: file upload controls.

use crate::file_ops::FilePickerChannel;
use crate::state::AppState;
use eframe::egui::{self, RichText};

pub fn render_left_panel(
    ctx: &egui::Context,
    state: &mut AppState,
    file_picker: &FilePickerChannel,
) {
    egui::SidePanel::left("left_panel")
        .resizable(true)
        .default_width(250.0)
        .min_width(200.0)
        .max_width(400.0)
        .show(ctx, |ui| {
            ui.heading("Load Data");
            ui.separator();

            let is_loading = state.upload_state.loading;

            ui.add_enabled_ui(!is_loading, |ui| {
                if ui.button("Choose file...").clicked() {
                    state.upload_state.loading = true;
                    state.status_message = "Opening file dialog...".to_string();
                    file_picker.pick_file(ctx.clone());
                }
            });

            if is_loading {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Selecting file...");
                });
            }

            ui.add_space(10.0);

            if let Some(ref name) = state.upload_state.file_name {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("\u{2713}").color(egui::Color32::from_rgb(100, 200, 100)),
                        );
                        ui.label(RichText::new("File loaded").small());
                    });
                    ui.label(RichText::new(name).strong().monospace());

                    if let Some(size) = state.upload_state.file_size {
                        ui.label(format_file_size(size));
                    }

                    if state.upload_state.file_data.is_some() {
                        ui.label(
                            RichText::new("Ready for processing")
                                .small()
                                .color(egui::Color32::from_rgb(100, 200, 100)),
                        );
                    }
                });
            }
        });
}

fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}
