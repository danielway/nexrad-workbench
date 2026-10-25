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
//! Both are the one dialog component ([`super::dialog::Dialog`]). They read
//! as one dialog: the detail opened from the list has a back arrow, and
//! Escape returns to the list (the stack closes the detail first).

use super::dialog::{self, Dialog, Width};
use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::alerts::{event_color, Alert};
use crate::core::diagnostics::{DiagnosticsIntent, DiagnosticsVm};
use crate::core::dialogs::{DialogId, DialogIntent};
use crate::core::Intent;
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, ScrollArea, Vec2};

const ALERT_ROW_HEIGHT: f32 = 68.0;

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
        if ctx.diagnostics.alerts.selected_alert_id.is_some() {
            render_detail_modal(ctx.ctx, ctx.state, ctx.diagnostics);
        } else if ctx.diagnostics.alerts.list_modal_open {
            render_list_modal(ctx.ctx, ctx.state, ctx.diagnostics_vm, ctx.derived);
        }
    }
}

fn render_list_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    vm: &DiagnosticsVm,
    derived: &crate::subsystem::Derived,
) {
    // The severity-sorted visible-alert list is the view-model; no recompute here.
    let visible = &vm.visible_alerts;
    let list_h = (ctx.input(|i| i.viewport_rect().height()) * 0.6).max(160.0);

    let out = Dialog::new(DialogId::AlertsList, "Alerts in view")
        .width(Width::Wide)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} alert{} in view",
                        visible.len(),
                        if visible.len() == 1 { "" } else { "s" }
                    ))
                    .size(13.0)
                    .color(theme::TEXT_DIM),
                );
                // The feed's last check, moved off the canvas (§7): each
                // row's dot is the event's map color, so the list is the
                // alert layer's legend.
                if let Some(checked) = vm.alerts_last_checked_secs {
                    use crate::core::session::Clock;
                    let clock = crate::ui::time_format::ZoneClock {
                        use_local: state.use_local_time,
                    };
                    ui.label(
                        RichText::new(format!("· checked {}", clock.hm(checked)))
                            .size(13.0)
                            .color(theme::TEXT_FAINT),
                    )
                    .on_hover_text("When the NWS alerts feed was last read");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button(RichText::new(format!(
                            "{} Refresh",
                            crate::ui::icons::ARROWS_CLOCKWISE
                        )))
                        .on_hover_text("Re-fetch the NWS alerts feed")
                        .clicked()
                    {
                        state.push_command(Intent::Diagnostics(DiagnosticsIntent::RefreshAlerts));
                    }
                });
            });
            ui.separator();

            if visible.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("No active alerts in the current view.")
                            .color(theme::TEXT_DIM),
                    );
                });
                ui.add_space(12.0);
                return;
            }

            ScrollArea::vertical()
                .id_salt("alerts_list_scroll")
                .max_height(list_h)
                .auto_shrink([false, false])
                .show_rows(ui, ALERT_ROW_HEIGHT, visible.len(), |ui, rows| {
                    let rows_rect = ui.max_rect();
                    let row_spacing = ui.spacing().item_spacing.y;
                    for (visible_row, index) in rows.enumerate() {
                        let row_rect = alert_row_rect(rows_rect, visible_row, row_spacing);
                        render_alert_row(ui, row_rect, &visible[index], derived, state);
                    }
                });
        });
    if out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::AlertsList)));
    }
}

fn render_detail_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    diagnostics: &crate::subsystem::Diagnostics,
) {
    let alert: &Alert = match diagnostics
        .alerts
        .selected_alert_id
        .as_ref()
        .and_then(|id| diagnostics.alerts.find(id))
    {
        Some(a) => a,
        None => {
            // Stale selection (e.g. alert expired while modal was open).
            state.push_command(Intent::Diagnostics(DiagnosticsIntent::ClearAlertSelection));
            return;
        }
    };
    let from_list = diagnostics.alerts.list_modal_open;
    let in_2d = state.viz_state.view_mode() == crate::geo::ViewMode::Flat2D;
    let body_height = (ctx.input(|i| i.viewport_rect().height()) * 0.6).max(160.0);

    let out = Dialog::new(DialogId::AlertDetail, &alert.event)
        .width(Width::Wide)
        .show(ctx, |ui| {
            if from_list
                && ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("{} All alerts", crate::ui::icons::ARROW_LEFT))
                                .color(theme::TEXT_DIM),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Back to the list (Esc)")
                    .clicked()
            {
                state.push_command(Intent::Diagnostics(DiagnosticsIntent::ClearAlertSelection));
            }
            ScrollArea::vertical()
                .id_salt(("alert_detail_scroll", &alert.id))
                .max_height(body_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        severity_badge(ui, alert);
                        if !alert.urgency.is_empty() {
                            chip_badge(
                                ui,
                                &format!("Urgency: {}", alert.urgency),
                                theme::TEXT_FAINT,
                            );
                        }
                        if !alert.certainty.is_empty() {
                            chip_badge(
                                ui,
                                &format!("Certainty: {}", alert.certainty),
                                theme::TEXT_FAINT,
                            );
                        }
                    });

                    if !alert.headline.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(&alert.headline).size(13.0).strong());
                    }

                    ui.add_space(6.0);
                    ui.separator();
                    render_timing_meta(ui, alert, state.use_local_time);

                    ui.separator();
                    ui.add_space(4.0);
                    if !alert.area_desc.is_empty() {
                        ui.label(RichText::new("Area").strong().size(12.0));
                        ui.label(
                            RichText::new(&alert.area_desc)
                                .size(12.0)
                                .color(theme::TEXT),
                        );
                        ui.add_space(8.0);
                    }
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
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new(&alert.instruction)
                                .size(12.0)
                                .color(theme::TEXT),
                        );
                    }
                });

            // "Show on map" centers the 2D map, so it is offered only in 2D.
            if in_2d {
                dialog::footer(ui, |ui| {
                    if dialog::primary(
                        ui,
                        &format!("{} Show on map", crate::ui::icons::MAP_PIN_LINE),
                    )
                    .on_hover_text("Center the map on the alert and turn on the alerts overlay")
                    .clicked()
                    {
                        // The handler centers the view, enables the class
                        // layer, and closes this dialog — all via the pure
                        // `compute_alert_focus`.
                        state.push_command(Intent::ShowAlertOnMap(alert.id.clone()));
                    }
                });
            }
        });
    if out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::AlertDetail)));
    }
}

fn render_alert_row(
    ui: &mut egui::Ui,
    rect: Rect,
    alert: &crate::core::diagnostics::VisibleAlert,
    derived: &crate::subsystem::Derived,
    state: &mut AppState,
) {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("alert_row", &alert.id)),
        egui::Sense::click(),
    );
    let card_rect = rect.shrink2(Vec2::new(0.0, 2.0));
    if response.hovered() {
        ui.painter().rect_filled(
            card_rect,
            egui::CornerRadius::same(4),
            theme::alpha(theme::TEXT, 8),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.painter().rect_stroke(
        card_rect,
        egui::CornerRadius::same(4),
        event_stroke(&alert.event),
        egui::StrokeKind::Inside,
    );

    let content_rect = card_rect.shrink2(Vec2::new(10.0, 7.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(&alert.id)
            .max_rect(content_rect),
        |ui| {
            ui.horizontal(|ui| {
                event_dot(ui, &alert.event);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(RichText::new(&alert.event).size(14.0).strong())
                            .truncate(),
                    );
                    if !alert.area_desc.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&alert.area_desc)
                                    .size(11.0)
                                    .color(theme::TEXT_DIM),
                            )
                            .truncate(),
                        );
                    }
                    if let Some(exp) = alert.expires_secs {
                        ui.label(
                            RichText::new(format!(
                                "Expires {}",
                                format_relative(derived.frame_now_secs, exp)
                            ))
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                        );
                    }
                });
            });
        },
    );

    if response.clicked() {
        state.push_command(Intent::Diagnostics(DiagnosticsIntent::SelectAlert(
            alert.id.clone(),
        )));
    }
}

fn alert_row_rect(rows_rect: Rect, visible_row: usize, row_spacing: f32) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            rows_rect.left(),
            rows_rect.top() + visible_row as f32 * (ALERT_ROW_HEIGHT + row_spacing),
        ),
        Vec2::new(rows_rect.width(), ALERT_ROW_HEIGHT),
    )
}

fn render_timing_meta(ui: &mut egui::Ui, alert: &Alert, use_local_time: bool) {
    if ui.available_width() >= 420.0 {
        ui.columns(2, |columns| {
            if let Some(t) = alert.effective_secs {
                meta_row(
                    &mut columns[0],
                    "Effective",
                    &format_absolute(t, use_local_time),
                );
            }
            if let Some(t) = alert.onset_secs {
                meta_row(
                    &mut columns[0],
                    "Onset",
                    &format_absolute(t, use_local_time),
                );
            }
            if let Some(t) = alert.expires_secs {
                meta_row(
                    &mut columns[1],
                    "Expires",
                    &format_absolute(t, use_local_time),
                );
            }
            if let Some(t) = alert.ends_secs {
                meta_row(&mut columns[1], "Ends", &format_absolute(t, use_local_time));
            }
        });
    } else {
        for (label, time) in [
            ("Effective", alert.effective_secs),
            ("Onset", alert.onset_secs),
            ("Expires", alert.expires_secs),
            ("Ends", alert.ends_secs),
        ] {
            if let Some(time) = time {
                meta_row(ui, label, &format_absolute(time, use_local_time));
            }
        }
    }
    if !alert.sender.is_empty() {
        meta_row(ui, "Sender", &alert.sender);
    }
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
                .color(theme::TEXT_DIM),
        );
        ui.label(RichText::new(value).size(11.0));
    });
}

fn format_absolute(ts_secs: f64, use_local_time: bool) -> String {
    let p = super::time_format::parts(ts_secs, use_local_time);
    let zone = if use_local_time { "local" } else { "UTC" };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} {zone}",
        p.year, p.month, p.day, p.hour, p.minute,
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn virtual_alert_card_borders_do_not_overlap() {
        let rows_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 500.0));
        let first = alert_row_rect(rows_rect, 0, 0.0).shrink2(Vec2::new(0.0, 2.0));
        let second = alert_row_rect(rows_rect, 1, 0.0).shrink2(Vec2::new(0.0, 2.0));

        assert!(first.bottom() < second.top());
    }
}
