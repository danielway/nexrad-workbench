//! NWS alert list and detail modals.

use crate::nws;
use crate::state::AppState;
use eframe::egui::{self, Color32, RichText, ScrollArea, Vec2};

/// Render the alert list modal showing all active alerts.
pub fn render_alert_list(ctx: &egui::Context, state: &mut AppState) {
    if !state.alert_list_open {
        return;
    }

    if super::modal_helper::modal_backdrop(ctx, "alert_list_backdrop", 180) {
        state.alert_list_open = false;
        return;
    }

    egui::Window::new("Active Weather Alerts")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(480.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            if state.nws_alert_state.alerts.is_empty() {
                ui.label(
                    RichText::new("No active alerts for this area.")
                        .size(13.0)
                        .weak(),
                );
            } else {
                ui.label(
                    RichText::new(format!(
                        "{} active alert{}",
                        state.nws_alert_state.alerts.len(),
                        if state.nws_alert_state.alerts.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ))
                    .size(13.0)
                    .weak(),
                );
                ui.add_space(4.0);

                let max_height = 400.0;
                ScrollArea::vertical()
                    .max_height(max_height)
                    .show(ui, |ui| {
                        let mut clicked_index = None;
                        for (i, alert) in state.nws_alert_state.alerts.iter().enumerate() {
                            let color = nws::event_color(&alert.event, alert.severity);

                            let response = ui
                                .horizontal(|ui| {
                                    // Colored severity pip
                                    let (rect, _) = ui.allocate_exact_size(
                                        Vec2::new(6.0, 16.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(rect, 2.0, color);

                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(&alert.event)
                                                .size(13.0)
                                                .strong()
                                                .color(color),
                                        );
                                        if let Some(ref headline) = alert.headline {
                                            let display = if headline.len() > 80 {
                                                format!("{}...", &headline[..77])
                                            } else {
                                                headline.clone()
                                            };
                                            ui.label(
                                                RichText::new(display)
                                                    .size(11.0)
                                                    .color(Color32::from_rgb(180, 180, 180)),
                                            );
                                        }
                                    });
                                })
                                .response;

                            if response
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .interact(egui::Sense::click())
                                .clicked()
                            {
                                clicked_index = Some(i);
                            }

                            if i + 1 < state.nws_alert_state.alerts.len() {
                                ui.separator();
                            }
                        }

                        if let Some(idx) = clicked_index {
                            state.nws_alert_state.selected_alert_index = Some(idx);
                            state.alert_list_open = false;
                            state.alert_detail_open = true;
                        }
                    });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            if ui.button("Close").clicked() {
                state.alert_list_open = false;
            }

            ui.add_space(4.0);
        });
}

/// Render the alert detail modal for the selected alert.
pub fn render_alert_detail(ctx: &egui::Context, state: &mut AppState) {
    if !state.alert_detail_open {
        return;
    }

    let alert = match state.nws_alert_state.selected_alert_index {
        Some(idx) if idx < state.nws_alert_state.alerts.len() => {
            state.nws_alert_state.alerts[idx].clone()
        }
        _ => {
            state.alert_detail_open = false;
            return;
        }
    };

    if super::modal_helper::modal_backdrop(ctx, "alert_detail_backdrop", 180) {
        state.alert_detail_open = false;
        return;
    }

    let color = nws::event_color(&alert.event, alert.severity);

    egui::Window::new("Weather Alert Detail")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(520.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // Header with event name and severity
            ui.horizontal(|ui| {
                ui.label(RichText::new(&alert.event).size(18.0).strong().color(color));
                ui.label(
                    RichText::new(format!("{:?}", alert.severity))
                        .size(12.0)
                        .color(Color32::from_rgb(180, 180, 180)),
                );
            });

            ui.add_space(4.0);

            // Headline
            if let Some(ref headline) = alert.headline {
                ui.label(RichText::new(headline).size(13.0).strong());
                ui.add_space(4.0);
            }

            ui.separator();
            ui.add_space(4.0);

            // Timing
            ui.horizontal(|ui| {
                ui.label(RichText::new("Effective:").size(12.0).strong());
                ui.label(RichText::new(&alert.effective).size(12.0));
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Expires:").size(12.0).strong());
                ui.label(RichText::new(&alert.expires).size(12.0));
            });
            if let Some(ref onset) = alert.onset {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Onset:").size(12.0).strong());
                    ui.label(RichText::new(onset).size(12.0));
                });
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("Urgency:").size(12.0).strong());
                ui.label(RichText::new(&alert.urgency).size(12.0));
            });

            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);

            // Description
            ui.label(RichText::new("Description").size(13.0).strong());
            ui.add_space(2.0);
            ScrollArea::vertical()
                .max_height(250.0)
                .id_salt("alert_description")
                .show(ui, |ui| {
                    ui.label(RichText::new(&alert.description).size(12.0));
                });

            // Instruction
            if let Some(ref instruction) = alert.instruction {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label(RichText::new("Instructions").size(13.0).strong());
                ui.add_space(2.0);
                ScrollArea::vertical()
                    .max_height(150.0)
                    .id_salt("alert_instruction")
                    .show(ui, |ui| {
                        ui.label(RichText::new(instruction).size(12.0));
                    });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                if ui.button("Back to list").clicked() {
                    state.alert_detail_open = false;
                    state.alert_list_open = true;
                }
                if ui.button("Close").clicked() {
                    state.alert_detail_open = false;
                }
            });

            ui.add_space(4.0);
        });
}
