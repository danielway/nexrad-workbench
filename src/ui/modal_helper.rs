//! Shared modal backdrop pattern used by all modal overlays.

use eframe::egui::{self, Color32};

/// Renders a semi-transparent backdrop overlay that closes a modal on click or Escape.
///
/// Call at the top of any modal render function. Returns `true` if the modal
/// should close (Escape pressed or backdrop clicked).
pub(super) fn modal_backdrop(ctx: &egui::Context, id: &str, alpha: u8) -> bool {
    let mut close = false;

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }

    egui::Area::new(egui::Id::new(id))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(
                screen_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, alpha),
            );
            if response.clicked() {
                close = true;
            }
        });

    close
}
