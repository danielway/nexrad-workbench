//! Top bar UI: app title, status, and alert summary.

use crate::state::{AlertSummary, AppState};
use eframe::egui::{self, Color32, RichText};

/// Warning color (red).
const WARNING_COLOR: Color32 = Color32::from_rgb(255, 80, 80);
/// Watch color (orange).
const WATCH_COLOR: Color32 = Color32::from_rgb(255, 180, 50);
/// Advisory color (yellow).
const ADVISORY_COLOR: Color32 = Color32::from_rgb(200, 200, 100);
/// Statement color (blue-gray).
const STATEMENT_COLOR: Color32 = Color32::from_rgb(140, 140, 180);
/// Muted label color.
const LABEL_COLOR: Color32 = Color32::from_rgb(120, 120, 120);

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

                // Push alert summary to the right
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let current_time = state
                        .playback_state
                        .selected_timestamp
                        .unwrap_or(1714564800.0);
                    let summary = state.alerts_state.count_by_severity(current_time);
                    render_alert_summary(ui, &summary);
                });
            });
        });
}

/// Render the NWS alert summary (right-aligned in top bar).
fn render_alert_summary(ui: &mut egui::Ui, summary: &AlertSummary) {
    if !summary.has_alerts() {
        ui.label(
            RichText::new("No active alerts")
                .size(12.0)
                .color(LABEL_COLOR),
        );
        return;
    }

    // Show counts for each severity level (in reverse order due to right-to-left layout)
    if summary.statements > 0 {
        ui.label(
            RichText::new(format!("{} STS", summary.statements))
                .size(12.0)
                .color(STATEMENT_COLOR),
        );
    }

    if summary.advisories > 0 {
        ui.label(
            RichText::new(format!("{} ADV", summary.advisories))
                .size(12.0)
                .color(ADVISORY_COLOR),
        );
    }

    if summary.watches > 0 {
        ui.label(
            RichText::new(format!("{} WCH", summary.watches))
                .size(12.0)
                .color(WATCH_COLOR),
        );
    }

    if summary.warnings > 0 {
        ui.label(
            RichText::new(format!("{} WRN", summary.warnings))
                .size(12.0)
                .strong()
                .color(WARNING_COLOR),
        );
    }

    ui.label(RichText::new("Alerts:").size(12.0).color(LABEL_COLOR));
}
