//! Shared overflow-menu idiom for the responsive desktop bars.
//!
//! When the window is narrow (see [`crate::state::WidthTier`]), low-priority
//! controls are demoted from the top/bottom bars into a single `⋯` menu so the
//! bars never overflow and overlap. Both bars use the same trigger glyph and
//! popup styling, so it lives here once.

use eframe::egui::{self, RichText};

/// A framed `⋯` button that opens a vertical popup. Callers render their
/// demoted controls in `add_contents`; the returned [`egui::InnerResponse`]
/// exposes the menu's inner value (`None` while the menu is closed).
///
/// Works inside a `right_to_left` layout — the popup renders in a foreground
/// `Area` anchored to the button, independent of the parent layout direction.
pub(super) fn overflow_menu<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let result = ui.menu_button(
        RichText::new(egui_phosphor::regular::DOTS_THREE).size(14.0),
        |ui| {
            ui.set_min_width(180.0);
            add_contents(ui)
        },
    );
    let response = result.response.on_hover_text("More");
    egui::InnerResponse::new(result.inner, response)
}
