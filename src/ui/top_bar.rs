//! Top bar UI: app title, status, and alert summary.

use crate::state::{AlertSummary, AppState, LivePhase};
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

/// Live mode colors
const LIVE_COLOR_ACQUIRING: Color32 = Color32::from_rgb(255, 180, 50); // Orange
const LIVE_COLOR_STREAMING: Color32 = Color32::from_rgb(255, 80, 80); // Red

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

                // Show live status or regular status message
                if state.live_mode_state.is_active() {
                    render_live_status(ui, state);
                } else {
                    // Regular status text
                    ui.label(
                        RichText::new(&state.status_message)
                            .size(13.0)
                            .color(Color32::GRAY),
                    );
                }

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

/// Render live mode status in the top bar.
fn render_live_status(ui: &mut egui::Ui, state: &AppState) {
    let phase = state.live_mode_state.phase;
    let pulse_alpha = state.live_mode_state.pulse_alpha();

    // Get current time for status text
    let now = state
        .playback_state
        .selected_timestamp
        .unwrap_or(1714564800.0);

    match phase {
        LivePhase::AcquiringLock => {
            // Show "CONNECTING" with orange pulsing
            let pulsed_color = Color32::from_rgba_unmultiplied(
                LIVE_COLOR_ACQUIRING.r(),
                LIVE_COLOR_ACQUIRING.g(),
                LIVE_COLOR_ACQUIRING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(RichText::new("\u{2022}").size(16.0).color(pulsed_color)); // •

            let elapsed = state.live_mode_state.phase_elapsed_secs(now) as i32;
            ui.label(
                RichText::new(format!("Acquiring lock... {}s", elapsed))
                    .size(13.0)
                    .color(LIVE_COLOR_ACQUIRING),
            );
        }
        LivePhase::Streaming | LivePhase::WaitingForChunk => {
            // Show red "LIVE" indicator (always visible once streaming)
            let pulsed_color = Color32::from_rgba_unmultiplied(
                LIVE_COLOR_STREAMING.r(),
                LIVE_COLOR_STREAMING.g(),
                LIVE_COLOR_STREAMING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(RichText::new("\u{2022}").size(16.0).color(pulsed_color)); // •
            ui.label(
                RichText::new("LIVE")
                    .size(13.0)
                    .strong()
                    .color(LIVE_COLOR_STREAMING),
            );

            // Show chunk count and status
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
