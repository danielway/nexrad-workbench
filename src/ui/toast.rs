//! The toast surface (one-instrument §10): draws
//! [`crate::core::toast::ToastQueue::view`] under the top bar, above dialogs,
//! at every width. A click dismisses it. The queue and its timing are the
//! core's; this only paints and emits [`TopBarIntent::DismissToast`].

use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::toast::{ToastKind, FADE_OUT_MS};
use crate::core::top_bar::TopBarIntent;
use crate::ui::theme;
use eframe::egui::{self, RichText};

/// Gap between the bar's bottom edge and the toast.
const GAP: f32 = 8.0;

fn anchor_id() -> egui::Id {
    egui::Id::unique("__toast_anchor_y")
}

/// The top bar that drew this frame records its bottom edge here.
pub(super) fn set_anchor(ctx: &egui::Context, bottom_y: f32) {
    ctx.data_mut(|d| d.insert_temp(anchor_id(), bottom_y));
}

pub(super) struct ToastLayer;

impl Layer for ToastLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        200
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        let now_ms = ctx.state.frame_now.millis();
        let Some(toast) = ctx.state.toasts.view(now_ms) else {
            return;
        };
        let egui_ctx = ctx.ctx;
        // A hidden phone bar leaves last frame's anchor; fall back to the top.
        let bar_bottom = egui_ctx
            .data(|d| d.get_temp::<f32>(anchor_id()))
            .unwrap_or(0.0);
        // At phone width a long failure wraps inside the screen.
        let max_w = (egui_ctx.content_rect().width() - 48.0).max(160.0);
        let area = egui::Area::new(egui::Id::unique("toast_surface"))
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, bar_bottom + GAP))
            .show(egui_ctx, |ui| {
                ui.multiply_opacity(toast.opacity);
                egui::Frame::new()
                    .fill(theme::RAISED)
                    .stroke(egui::Stroke::new(1.0, theme::LINE))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.set_max_width(max_w);
                        ui.horizontal_wrapped(|ui| {
                            if toast.kind == ToastKind::Failure {
                                ui.label(
                                    RichText::new(crate::ui::icons::WARNING).color(theme::TEXT),
                                );
                            }
                            ui.label(RichText::new(&toast.text).color(theme::TEXT));
                        });
                    })
                    .response
            });
        let resp = area.inner.interact(egui::Sense::click());
        if resp.on_hover_text("Click to dismiss").clicked() {
            ctx.state
                .push_command(crate::core::Intent::TopBar(TopBarIntent::DismissToast));
        }
        // Repaint through the fades; otherwise wake for the fade-out.
        if toast.opacity < 1.0 {
            egui_ctx.request_repaint();
        } else {
            let until_fade = (toast.remaining_ms - FADE_OUT_MS).max(0.0);
            egui_ctx.request_repaint_after(std::time::Duration::from_millis(until_fade as u64));
        }
    }
}
