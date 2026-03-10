//! Confirmation modal for wiping all application data.
//!
//! Clears IndexedDB stores, localStorage, and reloads the page.

use crate::state::AppState;
use eframe::egui::{self, Color32, RichText, Vec2};

/// Render the "wipe all data" confirmation modal if open.
pub fn render_wipe_modal(ctx: &egui::Context, state: &mut AppState) {
    if !state.wipe_modal_open {
        return;
    }

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.wipe_modal_open = false;
        return;
    }

    // Semi-transparent backdrop
    egui::Area::new(egui::Id::new("wipe_modal_backdrop"))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(
                screen_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 180),
            );
            if response.clicked() {
                state.wipe_modal_open = false;
            }
        });

    // Modal window
    egui::Window::new("Reset Application")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(340.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(8.0);

            ui.label(RichText::new("This will permanently delete all application data:").strong());

            ui.add_space(8.0);

            ui.label("  \u{2022} All cached radar data (IndexedDB)");
            ui.label("  \u{2022} Settings and preferences (localStorage)");

            ui.add_space(8.0);

            ui.label(
                RichText::new("The page will reload after reset.")
                    .weak()
                    .italics(),
            );

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    state.wipe_modal_open = false;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let reset_btn = ui.add(
                        egui::Button::new(RichText::new("Reset Everything").color(Color32::WHITE))
                            .fill(Color32::from_rgb(200, 60, 60)),
                    );
                    if reset_btn.clicked() {
                        state.wipe_modal_open = false;
                        state.push_command(crate::state::AppCommand::WipeAll);
                    }
                });
            });

            ui.add_space(4.0);
        });
}
