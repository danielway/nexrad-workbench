//! Left panel UI: data source controls that vary by mode.

use crate::file_ops::FilePickerChannel;
use crate::state::{AppState, DataSourceMode};
use eframe::egui::{self, RichText, ScrollArea, Ui};

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
            ui.heading("Data Source");
            ui.separator();

            match state.data_source_mode {
                DataSourceMode::UploadFile => render_upload_mode(ctx, ui, state, file_picker),
                DataSourceMode::ArchiveBrowser => render_archive_mode(ui, state),
                DataSourceMode::RealtimeStream => render_realtime_mode(ui, state),
            }
        });
}

fn render_upload_mode(
    ctx: &egui::Context,
    ui: &mut Ui,
    state: &mut AppState,
    file_picker: &FilePickerChannel,
) {
    ui.label(RichText::new("Upload File").strong());
    ui.add_space(10.0);

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
                ui.label(RichText::new("\u{2713}").color(egui::Color32::from_rgb(100, 200, 100)));
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
    } else if !is_loading {
        ui.label(RichText::new("No file selected").italics().weak());
    }
}

fn render_archive_mode(ui: &mut Ui, state: &mut AppState) {
    ui.label(RichText::new("AWS Archive Browser").strong());
    ui.add_space(10.0);

    // Radar site input
    ui.horizontal(|ui| {
        ui.label("Site:");
        ui.text_edit_singleline(&mut state.archive_state.site_id)
            .on_hover_text("Enter radar site ID (e.g., KTLX)");
    });

    ui.add_space(5.0);

    // Date input
    ui.horizontal(|ui| {
        ui.label("Date:");
        ui.text_edit_singleline(&mut state.archive_state.date_string)
            .on_hover_text("Enter date (e.g., 2024-05-20)");
    });

    ui.add_space(10.0);

    if ui.button("Search archive").clicked() {
        // Placeholder: populate with dummy times
        state.archive_state.available_times = vec![
            "00:05:32 UTC".to_string(),
            "00:10:15 UTC".to_string(),
            "00:15:47 UTC".to_string(),
            "00:20:22 UTC".to_string(),
            "00:25:58 UTC".to_string(),
            "00:30:33 UTC".to_string(),
        ];
        state.archive_state.selected_time_index = None;
        state.status_message = "Archive search complete (placeholder)".to_string();
    }

    ui.add_space(10.0);

    // Time list
    if !state.archive_state.available_times.is_empty() {
        ui.label(RichText::new("Available times:").small());

        ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
            for (idx, time) in state.archive_state.available_times.iter().enumerate() {
                let is_selected = state.archive_state.selected_time_index == Some(idx);
                if ui.selectable_label(is_selected, time).clicked() {
                    state.archive_state.selected_time_index = Some(idx);
                }
            }
        });

        ui.add_space(10.0);

        let can_load = state.archive_state.selected_time_index.is_some();
        ui.add_enabled_ui(can_load, |ui| {
            if ui.button("Load selection").clicked() {
                state.status_message = "Loading archive data... (placeholder)".to_string();
            }
        });
    }
}

fn render_realtime_mode(ui: &mut Ui, state: &mut AppState) {
    ui.label(RichText::new("Realtime Stream").strong());
    ui.add_space(10.0);

    // Site selection
    ui.horizontal(|ui| {
        ui.label("Site:");
        ui.text_edit_singleline(&mut state.realtime_state.site_id)
            .on_hover_text("Enter radar site ID (e.g., KTLX)");
    });

    ui.add_space(10.0);

    // Connection controls
    ui.horizontal(|ui| {
        if state.realtime_state.connected {
            if ui.button("Disconnect").clicked() {
                state.realtime_state.connected = false;
                state.realtime_state.status = "Disconnected".to_string();
                state.status_message = "Disconnected from stream".to_string();
            }
        } else {
            let can_connect = !state.realtime_state.site_id.is_empty();
            ui.add_enabled_ui(can_connect, |ui| {
                if ui.button("Connect").clicked() {
                    state.realtime_state.connected = true;
                    state.realtime_state.status = "Connected".to_string();
                    state.status_message = format!(
                        "Connected to {} (placeholder)",
                        state.realtime_state.site_id
                    );
                }
            });
        }
    });

    ui.add_space(10.0);

    // Status indicator
    ui.group(|ui| {
        ui.label(RichText::new("Status:").small());

        let (status_text, status_color) = if state.realtime_state.connected {
            (
                &state.realtime_state.status,
                egui::Color32::from_rgb(100, 200, 100),
            )
        } else {
            (
                &state.realtime_state.status,
                egui::Color32::from_rgb(150, 150, 150),
            )
        };

        ui.label(RichText::new(status_text).color(status_color).strong());
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
