//! The legacy backdrop, kept only for the phone's settings sheet until the
//! phone PR moves it onto [`super::dialog::Dialog`].
//!
//! Escape is no longer handled here: it closes the top entry of the dialog
//! stack, once, centrally (`core::dialogs`).

use crate::ui::theme;
use eframe::egui::{self};

/// A dimmed full-screen backdrop. Returns `true` when it was clicked.
pub(super) fn modal_backdrop(ctx: &egui::Context, id: &str) -> bool {
    let mut close = false;
    egui::Area::new(egui::Id::unique(id))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(screen_rect, 0.0, theme::dialog::backdrop());
            if response.clicked() {
                close = true;
            }
        });
    close
}
