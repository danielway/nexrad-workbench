//! Performance detail modal showing timing breakdowns per pipeline group.
//!
//! Opened by clicking the pipeline indicator in the bottom status bar.
//! Shows sub-phase timings for Download, Processing, and Rendering.

use super::colors::ui as ui_colors;
use crate::state::AppState;
use eframe::egui::{self, Color32, RichText, Vec2};

/// Render the performance detail modal if open.
pub fn render_stats_modal(ctx: &egui::Context, state: &mut AppState) {
    if !state.stats_detail_open {
        return;
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.stats_detail_open = false;
        return;
    }

    // Semi-transparent backdrop
    egui::Area::new(egui::Id::new("stats_modal_backdrop"))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(
                screen_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 160),
            );
            if response.clicked() {
                state.stats_detail_open = false;
            }
        });

    // Modal window
    egui::Window::new("Performance")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(320.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let dark = state.is_dark;
            let stats = &state.session_stats;
            let label_color = ui_colors::label(dark);
            let value_color = ui_colors::value(dark);
            let heading_color = ui_colors::ACTIVE;

            // --- Download section ---
            ui.add_space(4.0);
            ui.label(
                RichText::new("Download")
                    .size(12.0)
                    .strong()
                    .color(heading_color),
            );
            ui.indent("dl_section", |ui| {
                stat_row(
                    ui,
                    "Fetch latency",
                    &format_ms_avg(stats.median_chunk_latency_ms),
                    label_color,
                    value_color,
                );
                stat_row(
                    ui,
                    "Requests",
                    &format!(
                        "{} total ({} active)",
                        stats.session_request_count, stats.active_request_count
                    ),
                    label_color,
                    value_color,
                );
                stat_row(
                    ui,
                    "Transferred",
                    &stats.format_transferred(),
                    label_color,
                    value_color,
                );
            });

            ui.separator();

            // --- Processing section ---
            ui.label(
                RichText::new("Processing")
                    .size(12.0)
                    .strong()
                    .color(heading_color),
            );
            ui.indent("proc_section", |ui| {
                if let Some(ref d) = stats.last_ingest_detail {
                    stat_row(
                        ui,
                        "Split",
                        &format_ms(d.split_ms),
                        label_color,
                        value_color,
                    );
                    stat_row(
                        ui,
                        "Decompress",
                        &format_ms(d.decompress_ms),
                        label_color,
                        value_color,
                    );
                    stat_row(
                        ui,
                        "Decode",
                        &format_ms(d.decode_ms),
                        label_color,
                        value_color,
                    );
                    stat_row(
                        ui,
                        "Extract",
                        &format_ms(d.extract_ms),
                        label_color,
                        value_color,
                    );
                    stat_row(
                        ui,
                        "Store (IDB)",
                        &format_ms(d.store_ms),
                        label_color,
                        value_color,
                    );
                    stat_row(
                        ui,
                        "Index",
                        &format_ms(d.index_ms),
                        label_color,
                        value_color,
                    );
                }
                stat_row(
                    ui,
                    "Total (avg)",
                    &format_ms_avg(stats.median_processing_time_ms),
                    label_color,
                    value_color,
                );
            });

            ui.separator();

            // --- Rendering section ---
            ui.label(
                RichText::new("Rendering")
                    .size(12.0)
                    .strong()
                    .color(heading_color),
            );
            ui.indent("gpu_section", |ui| {
                if let Some(ref d) = stats.last_render_detail {
                    stat_row(
                        ui,
                        "IDB Fetch",
                        &format_ms(d.fetch_ms),
                        label_color,
                        value_color,
                    );
                    stat_row(
                        ui,
                        "Deserialize",
                        &format_ms(d.deser_ms),
                        label_color,
                        value_color,
                    );
                    stat_row(
                        ui,
                        "Marshal",
                        &format_ms(d.marshal_ms),
                        label_color,
                        value_color,
                    );
                    stat_row(
                        ui,
                        "GPU Upload",
                        &format_ms(d.gpu_upload_ms),
                        label_color,
                        value_color,
                    );
                }
                stat_row(
                    ui,
                    "Total (avg)",
                    &format_ms_avg(stats.avg_render_time_ms),
                    label_color,
                    value_color,
                );
                if let Some(fps) = stats.avg_fps {
                    stat_row(
                        ui,
                        "Frame rate",
                        &format!("{:.0} fps", fps),
                        label_color,
                        value_color,
                    );
                }
            });

            ui.separator();

            // --- Cache ---
            stat_row(
                ui,
                "Cache",
                &stats.format_cache_size(),
                label_color,
                value_color,
            );

            ui.add_space(4.0);
        });
}

/// Render a single label–value row with right-aligned value.
fn stat_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    label_color: Color32,
    value_color: Color32,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(11.0).color(label_color));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(value)
                    .size(11.0)
                    .monospace()
                    .color(value_color),
            );
        });
    });
}

/// Format a millisecond value for display.
fn format_ms(ms: f64) -> String {
    if ms < 1.0 {
        format!("{:.2} ms", ms)
    } else if ms < 10.0 {
        format!("{:.1} ms", ms)
    } else {
        format!("{:.0} ms", ms)
    }
}

/// Format an optional EMA average for display.
fn format_ms_avg(v: Option<f64>) -> String {
    match v {
        Some(ms) => format!("{} avg", format_ms(ms)),
        None => "\u{2014}".to_string(),
    }
}
