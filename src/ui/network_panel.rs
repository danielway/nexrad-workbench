//! Network request log modal.
//!
//! Displays a scrollable table of recent network requests intercepted by the
//! service worker, along with aggregate statistics. Opened from the Performance
//! modal or the bottom status bar.

use super::colors::ui as ui_colors;
use crate::state::{format_bytes, AppState};
use eframe::egui::{self, Color32, RichText, ScrollArea, Vec2};

/// Render the network log modal if open.
pub fn render_network_log(ctx: &egui::Context, state: &mut AppState) {
    if !state.network_log_open {
        return;
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.network_log_open = false;
        return;
    }

    // Semi-transparent backdrop
    egui::Area::new(egui::Id::new("network_log_backdrop"))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(
                screen_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 160),
            );
            if response.clicked() {
                state.network_log_open = false;
            }
        });

    let dark = state.is_dark;
    let label_color = ui_colors::label(dark);
    let value_color = ui_colors::value(dark);
    let heading_color = ui_colors::ACTIVE;

    egui::Window::new("Network Log")
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_size(Vec2::new(600.0, 400.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // Aggregate summary row
            let agg = &state.network_aggregate;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} requests", agg.total_requests))
                        .size(11.0)
                        .color(value_color),
                );
                ui.separator();
                if agg.failed_requests > 0 {
                    ui.label(
                        RichText::new(format!("{} failed", agg.failed_requests))
                            .size(11.0)
                            .color(Color32::from_rgb(255, 100, 100)),
                    );
                    ui.separator();
                }
                ui.label(
                    RichText::new(format!("{} transferred", format_bytes(agg.total_bytes)))
                        .size(11.0)
                        .color(value_color),
                );
                ui.separator();
                let coi_text = if state.cross_origin_isolated {
                    "COI: active"
                } else {
                    "COI: inactive"
                };
                let coi_color = if state.cross_origin_isolated {
                    ui_colors::SUCCESS
                } else {
                    label_color
                };
                ui.label(RichText::new(coi_text).size(11.0).color(coi_color));
            });

            ui.separator();

            // Scrollable request list (newest at bottom)
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if state.recent_network_requests.is_empty() {
                        ui.label(
                            RichText::new("No network requests captured yet.")
                                .size(11.0)
                                .italics()
                                .color(label_color),
                        );
                        return;
                    }

                    egui::Grid::new("network_log_grid")
                        .num_columns(4)
                        .spacing([10.0, 2.0])
                        .striped(true)
                        .show(ui, |ui| {
                            // Column headers
                            ui.label(
                                RichText::new("Status")
                                    .size(10.0)
                                    .strong()
                                    .color(heading_color),
                            );
                            ui.label(
                                RichText::new("URL")
                                    .size(10.0)
                                    .strong()
                                    .color(heading_color),
                            );
                            ui.label(
                                RichText::new("Size")
                                    .size(10.0)
                                    .strong()
                                    .color(heading_color),
                            );
                            ui.label(
                                RichText::new("Duration")
                                    .size(10.0)
                                    .strong()
                                    .color(heading_color),
                            );
                            ui.end_row();

                            for req in &state.recent_network_requests {
                                let sc = status_color(req.status, req.ok);
                                let short_url = shorten_url(&req.url);

                                // Status code
                                let status_str = if req.status > 0 {
                                    format!("{}", req.status)
                                } else {
                                    "ERR".to_string()
                                };
                                ui.label(
                                    RichText::new(status_str).size(10.0).monospace().color(sc),
                                );

                                // URL (truncated, hover for full)
                                ui.label(
                                    RichText::new(&short_url)
                                        .size(10.0)
                                        .monospace()
                                        .color(value_color),
                                )
                                .on_hover_text(&req.url);

                                // Size
                                let size_str = if req.bytes > 0 {
                                    format_bytes(req.bytes)
                                } else {
                                    "\u{2014}".to_string()
                                };
                                ui.label(
                                    RichText::new(size_str)
                                        .size(10.0)
                                        .monospace()
                                        .color(value_color),
                                );

                                // Duration
                                ui.label(
                                    RichText::new(format!("{:.0}ms", req.duration_ms))
                                        .size(10.0)
                                        .monospace()
                                        .color(value_color),
                                );

                                ui.end_row();
                            }
                        });
                });
        });
}

/// Color code by HTTP status range.
fn status_color(status: u16, ok: bool) -> Color32 {
    if !ok || status == 0 || status >= 400 {
        Color32::from_rgb(255, 100, 100) // Red for errors / 4xx / 5xx
    } else if status >= 300 {
        Color32::from_rgb(255, 200, 80) // Yellow for 3xx
    } else {
        Color32::from_rgb(100, 200, 100) // Green for 2xx
    }
}

/// Shorten a URL for display, keeping host and full path visible up to a
/// generous limit. The full URL is still available on hover.
fn shorten_url(url: &str) -> String {
    // Strip the scheme to save space but keep the rest visible.
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    const MAX_LEN: usize = 120;
    if without_scheme.len() <= MAX_LEN {
        without_scheme.to_string()
    } else {
        // Keep the host and as much of the tail as possible.
        let parts: Vec<&str> = without_scheme.splitn(2, '/').collect();
        let host = parts[0];
        let path = parts.get(1).unwrap_or(&"");
        let budget = MAX_LEN.saturating_sub(host.len() + 5); // "/.../" = 5
        if budget > 0 && path.len() > budget {
            format!("{}/.../{}", host, &path[path.len() - budget..])
        } else {
            format!(
                "{}...{}",
                &without_scheme[..40],
                &without_scheme[without_scheme.len() - (MAX_LEN - 43)..]
            )
        }
    }
}
