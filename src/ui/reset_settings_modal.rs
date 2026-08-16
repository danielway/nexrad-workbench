//! Confirmation modal for restoring the default settings.
//!
//! Clears localStorage while retaining saved events, then reloads the page.

use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::state::AppState;
use eframe::egui::{self, Color32, RichText, Vec2};

pub(super) struct ResetSettingsModalLayer;

impl Layer for ResetSettingsModalLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        30
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.reset_settings_modal_open
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_reset_settings_modal(ctx.ctx, ctx.state, ctx.chrome);
    }
}

fn draw_reset_settings_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
) {
    if super::modal_helper::modal_backdrop(ctx, "reset_settings_modal_backdrop", 180) {
        chrome.reset_settings_modal_open = false;
        return;
    }

    // Modal window
    egui::Window::new("Reset Settings")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(340.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(8.0);

            ui.label(
                RichText::new("Restore all settings and preferences to their defaults?").strong(),
            );

            ui.add_space(8.0);

            ui.label("  \u{2022} Settings and preferences");
            ui.label("  \u{2022} Saved events and cached radar data will be kept");

            ui.add_space(8.0);

            ui.label(
                RichText::new("The page will reload after resetting settings.")
                    .weak()
                    .italics(),
            );

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    chrome.reset_settings_modal_open = false;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let reset_btn = ui.add(
                        egui::Button::new(RichText::new("Reset Settings").color(Color32::WHITE))
                            .fill(Color32::from_rgb(200, 60, 60)),
                    );
                    if reset_btn.clicked() {
                        chrome.reset_settings_modal_open = false;
                        state.push_command(crate::core::Intent::ResetSettings);
                    }
                });
            });

            ui.add_space(4.0);
        });
}
