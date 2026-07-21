//! Modal UI for browsing and viewing NWS alert details.
//!
//! Two closely-related modals live here:
//!
//!  * **List modal** — opened from the top-bar alerts chip when multiple
//!    alerts intersect the current viewing area. Shows a scrollable list
//!    of affected alerts; clicking an item selects it for the detail modal.
//!  * **Detail modal** — shows full alert information (headline, severity,
//!    area, effective/expires times, description, instructions).
//!
//! Both follow the existing `modal_backdrop` + anchored `egui::Window` pattern
//! used by `site_modal`, `event_modal`, etc.

use super::layout::{Layer, LayerKind, LayoutCtx};
use super::modal_helper::modal_backdrop;
use crate::alerts::{event_color, Alert};
use crate::core::diagnostics::{DiagnosticsIntent, DiagnosticsVm};
use crate::state::{AppCommand, AppState};
use eframe::egui::{self, Color32, RichText, ScrollArea, Vec2};

pub(super) struct AlertsModalsLayer;

impl Layer for AlertsModalsLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        80
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.diagnostics.alerts.list_modal_open || ctx.diagnostics.alerts.selected_alert_id.is_some()
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        if ctx.diagnostics.alerts.list_modal_open {
            render_list_modal(ctx.ctx, ctx.state, ctx.diagnostics_vm, ctx.derived);
        }
        if ctx.diagnostics.alerts.selected_alert_id.is_some() {
            render_detail_modal(ctx.ctx, ctx.state, ctx.diagnostics);
        }
    }
}

fn render_list_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    vm: &DiagnosticsVm,
    derived: &crate::subsystem::Derived,
) {
    if modal_backdrop(ctx, "alerts_list_backdrop", 140) {
        state.push_command(AppCommand::Diagnostics(DiagnosticsIntent::CloseAlertList));
        return;
    }

    // The severity-sorted visible-alert list is the view-model; no recompute here.
    let visible = &vm.visible_alerts;

    egui::Window::new("Active Alerts in View")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(520.0, 560.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} alert(s) intersect the visible area",
                        visible.len()
                    ))
                    .size(13.0)
                    .color(Color32::from_rgb(180, 180, 180)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Close").clicked() {
                        state.push_command(AppCommand::Diagnostics(
                            DiagnosticsIntent::CloseAlertList,
                        ));
                    }
                    if ui
                        .small_button(RichText::new(format!(
                            "{} Refresh",
                            egui_phosphor::regular::ARROWS_CLOCKWISE
                        )))
                        .on_hover_text("Re-fetch the NWS alerts feed")
                        .clicked()
                    {
                        state.push_command(AppCommand::Diagnostics(
                            DiagnosticsIntent::RefreshAlerts,
                        ));
                    }
                });
            });
            ui.separator();

            if visible.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("No active alerts in the current view.").color(Color32::GRAY),
                    );
                });
                ui.add_space(12.0);
                return;
            }

            ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
                for va in visible {
                    ui.add_space(2.0);
                    let bg_stroke = event_stroke(&va.event);
                    let frame = egui::Frame::default()
                        .stroke(bg_stroke)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .corner_radius(egui::CornerRadius::same(4));
                    let response = frame
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                event_dot(ui, &va.event);
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&va.event).size(14.0).strong());
                                    if !va.area_desc.is_empty() {
                                        ui.label(
                                            RichText::new(truncate(&va.area_desc, 120))
                                                .size(11.0)
                                                .color(Color32::from_rgb(170, 170, 170)),
                                        );
                                    }
                                    if let Some(exp) = va.expires_secs {
                                        ui.label(
                                            RichText::new(format!(
                                                "Expires {}",
                                                format_relative(derived.frame_now_secs, exp)
                                            ))
                                            .size(10.0)
                                            .color(Color32::from_rgb(140, 140, 140)),
                                        );
                                    }
                                });
                            });
                        })
                        .response
                        .interact(egui::Sense::click());

                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if response.clicked() {
                        state.push_command(AppCommand::Diagnostics(
                            DiagnosticsIntent::SelectAlert(va.id.clone()),
                        ));
                    }
                    ui.add_space(2.0);
                }
            });
        });
}

fn render_detail_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    diagnostics: &crate::subsystem::Diagnostics,
) {
    if modal_backdrop(ctx, "alerts_detail_backdrop", 160) {
        state.push_command(AppCommand::Diagnostics(
            DiagnosticsIntent::ClearAlertSelection,
        ));
        return;
    }

    // Clone the alert once so we don't hold a borrow through the UI closure.
    let alert: Alert = match diagnostics
        .alerts
        .selected_alert_id
        .as_ref()
        .and_then(|id| diagnostics.alerts.find(id))
    {
        Some(a) => a.clone(),
        None => {
            // Stale selection (e.g. alert expired while modal was open).
            state.push_command(AppCommand::Diagnostics(
                DiagnosticsIntent::ClearAlertSelection,
            ));
            return;
        }
    };

    egui::Window::new(format!("{} — {}", alert.severity.label(), alert.event))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(560.0, 600.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Severity / urgency / certainty badges.
            ui.horizontal_wrapped(|ui| {
                severity_badge(ui, &alert);
                if !alert.urgency.is_empty() {
                    chip_badge(
                        ui,
                        &format!("Urgency: {}", alert.urgency),
                        Color32::from_rgb(90, 110, 140),
                    );
                }
                if !alert.certainty.is_empty() {
                    chip_badge(
                        ui,
                        &format!("Certainty: {}", alert.certainty),
                        Color32::from_rgb(90, 110, 140),
                    );
                }
            });

            if !alert.headline.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(&alert.headline).size(14.0).strong());
            }

            ui.add_space(6.0);
            ui.separator();

            // Timing + area meta block.
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    if let Some(t) = alert.effective_secs {
                        meta_row(ui, "Effective", &format_absolute(t));
                    }
                    if let Some(t) = alert.onset_secs {
                        meta_row(ui, "Onset", &format_absolute(t));
                    }
                    if let Some(t) = alert.expires_secs {
                        meta_row(ui, "Expires", &format_absolute(t));
                    }
                    if let Some(t) = alert.ends_secs {
                        meta_row(ui, "Ends", &format_absolute(t));
                    }
                    if !alert.sender.is_empty() {
                        meta_row(ui, "Sender", &alert.sender);
                    }
                });
            });

            ui.separator();
            ui.add_space(4.0);
            if !alert.area_desc.is_empty() {
                ui.label(RichText::new("Area").strong().size(12.0));
                ui.label(
                    RichText::new(&alert.area_desc)
                        .size(12.0)
                        .color(Color32::from_rgb(200, 200, 200)),
                );
                ui.add_space(6.0);
            }

            ScrollArea::vertical()
                .id_salt("alert_detail_scroll")
                .max_height(360.0)
                .show(ui, |ui| {
                    if !alert.description.is_empty() {
                        ui.label(RichText::new("Description").strong().size(12.0));
                        ui.label(RichText::new(&alert.description).size(12.0));
                        ui.add_space(8.0);
                    }
                    if !alert.instruction.is_empty() {
                        ui.label(
                            RichText::new("Instructions")
                                .strong()
                                .size(12.0)
                                .color(Color32::from_rgb(250, 220, 120)),
                        );
                        ui.label(
                            RichText::new(&alert.instruction)
                                .size(12.0)
                                .color(Color32::from_rgb(240, 220, 160)),
                        );
                    }
                });

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new(format!(
                        "{} Show on map",
                        egui_phosphor::regular::MAP_PIN_LINE
                    )))
                    .on_hover_text("Center the 2D map on the alert and enable the alerts overlay")
                    .clicked()
                {
                    // The handler centers the view, enables the class layer, and
                    // closes this modal — all via the pure `compute_alert_focus`.
                    state.push_command(AppCommand::ShowAlertOnMap(alert.id.clone()));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        state.push_command(AppCommand::Diagnostics(
                            DiagnosticsIntent::ClearAlertSelection,
                        ));
                    }
                });
            });
        });
}

fn event_dot(ui: &mut egui::Ui, event: &str) {
    let (r, g, b) = event_color(event);
    let color = Color32::from_rgb(r, g, b);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// Severity-label chip, tinted by the alert's event-type color so it matches the
/// map overlay and list dots.
fn severity_badge(ui: &mut egui::Ui, alert: &Alert) {
    let (r, g, b) = alert.color();
    let color = Color32::from_rgb(r, g, b);
    chip_badge(ui, alert.severity.label(), color);
}

fn chip_badge(ui: &mut egui::Ui, label: &str, color: Color32) {
    let text = RichText::new(label).size(11.0).strong().color(color);
    egui::Frame::default()
        .stroke(egui::Stroke::new(1.0_f32, color))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .corner_radius(egui::CornerRadius::same(3))
        .show(ui, |ui| {
            ui.label(text);
        });
}

fn event_stroke(event: &str) -> egui::Stroke {
    let (r, g, b) = event_color(event);
    egui::Stroke::new(1.0_f32, Color32::from_rgb(r, g, b))
}

fn meta_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{}:", label))
                .size(11.0)
                .color(Color32::from_rgb(150, 150, 150)),
        );
        ui.label(RichText::new(value).size(11.0));
    });
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('\u{2026}');
        out
    }
}

fn format_absolute(ts_secs: f64) -> String {
    // Show as local date-time; the user can mentally convert if needed.
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts_secs * 1000.0));
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} local",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
    )
}

fn format_relative(now_secs: f64, ts_secs: f64) -> String {
    let delta = ts_secs - now_secs;
    if delta < 0.0 {
        return "in the past".to_string();
    }
    let delta = delta as i64;
    if delta < 60 {
        format!("in {}s", delta)
    } else if delta < 3600 {
        format!("in {}m", delta / 60)
    } else if delta < 86400 {
        format!("in {}h{}m", delta / 3600, (delta % 3600) / 60)
    } else {
        format!("in {}d", delta / 86400)
    }
}
