//! The ambient activity chip — a 1:1 projection of [`ActivityVm::headline`].
//!
//! One renderer serves both the desktop transport row and the mobile top bar,
//! so the two can't drift apart the way the old duplicated status chips did.
//!
//! Design constraints this encodes (PRODUCT §5.2, §11.2, §14, §15):
//!
//! - **Always visible, including idle.** A chip that disappears when idle is
//!   ambiguous between "nothing to do" and "something is broken". "Up to date"
//!   is rendered in the muted label tone, not an accent, so it stays ambient.
//! - **Shape carries state, not hue.** The glyph (check / arrow / gear / wave /
//!   pause) plus the count is the whole signal; colour is decoration and the
//!   chip must still read in grayscale.
//! - **Red means failure, only.** The one red element is the failure tick.
//! - **Motion means data is moving.** Under reduced motion the pulse becomes a
//!   static tint and we stop requesting repaints.
//! - **One chip for every site.** With more than one site the words are the
//!   sum ([`ActivityHeadline::words`](crate::core::activity::ActivityHeadline::words)).
//! - **L0 restraint.** A casual viewer with nothing happening sees the glyph
//!   alone; the words appear on hover or once Advanced is on.

use crate::core::activity::{ActivityGlyph, ActivityVm};
use crate::core::Intent;
use crate::state::AppState;
use crate::ui::icons;
use crate::ui::theme;
use eframe::egui::{self, Color32, RichText};

/// Sine period for the activity pulse, in radians per second.
const PULSE_RATE: f64 = 3.0;

fn glyph_icon(glyph: ActivityGlyph) -> &'static str {
    match glyph {
        ActivityGlyph::Check => icons::CHECK_CIRCLE,
        ActivityGlyph::ArrowDown => icons::ARROW_DOWN,
        ActivityGlyph::Gear => icons::GEAR,
        ActivityGlyph::Wave => icons::BROADCAST,
        ActivityGlyph::Pause => icons::PAUSE,
        ActivityGlyph::Listing => icons::ARROWS_CLOCKWISE,
    }
}

/// Render the chip. The desktop transport draws it at 11 with its words; the
/// phone's transport draws it at 13 with its words `folded` into the hover,
/// leaving the glyph, the count and the ⚠ tick (the activity dot).
pub(super) fn render_activity_chip(
    ui: &mut egui::Ui,
    vm: &ActivityVm,
    state: &mut AppState,
    font_size: f32,
    folded: bool,
) {
    let idle = vm.chip_idle();

    let animate = vm.headline.animate && !super::timeline::reduced_motion(ui.ctx());
    let tint = chip_tint(ui, idle, animate);

    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;

        let mut text = glyph_icon(vm.chip_glyph()).to_string();
        if let Some(count) = vm.headline.count {
            text.push(' ');
            text.push_str(&count.to_string());
        }
        // The chip is always present, so "Up to date" is a state, not an
        // absence: the label shows at rest too — except on the phone, where
        // it is folded into the hover.
        if !folded {
            text.push(' ');
            text.push_str(&vm.chip_words());
        }

        let resp = ui.add(
            egui::Button::new(RichText::new(text).size(font_size).strong().color(tint))
                .frame(false),
        );
        if resp.clicked() {
            clicked = true;
        }
        if folded {
            resp.on_hover_text(format!("{} — {}", vm.chip_words(), vm.hover()));
        } else {
            resp.on_hover_text(vm.hover());
        }

        // The ⚠ tick — independent of the activity state, because a failed
        // scan or an error coexists with work in progress. It counts failed
        // downloads and the errors the sheet has not shown yet.
        if let Some((count, hover)) = vm.warning() {
            let label = format!("{} {}", icons::WARNING, count);
            let resp = ui.add(
                egui::Button::new(
                    RichText::new(label)
                        .size(font_size)
                        .strong()
                        .color(theme::acquisition::FAILED),
                )
                .frame(false),
            );
            if resp.clicked() {
                clicked = true;
            }
            resp.on_hover_text(hover);
        }

        if animate {
            ui.ctx().request_repaint();
        }
    });

    if clicked {
        state.push_command(Intent::SetActivitySheetOpen(true));
    }
}

/// The chip's colour.
///
/// Idle uses the muted label tone so an always-present chip doesn't spend one
/// of the three accent slots. Busy uses the single ACTIVE accent, pulsing in
/// alpha only — under reduced motion the same accent renders at a fixed
/// mid-alpha, so the state still reads without any movement.
fn chip_tint(ui: &egui::Ui, idle: bool, animate: bool) -> Color32 {
    if idle {
        return theme::TEXT_FAINT;
    }
    let base = theme::legacy::ACTIVE_BLUE;
    let alpha = if animate {
        let t = ui.ctx().input(|i| i.time);
        let pulse = (0.5 + 0.5 * (t * PULSE_RATE).sin()) as f32;
        (150.0 + 105.0 * pulse) as u8
    } else {
        220
    };
    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha)
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Every state maps to a distinct glyph, so the chip reads in grayscale
    /// without relying on colour to disambiguate.
    #[wasm_bindgen_test]
    fn every_glyph_maps_to_a_distinct_icon() {
        let glyphs = [
            ActivityGlyph::Check,
            ActivityGlyph::ArrowDown,
            ActivityGlyph::Gear,
            ActivityGlyph::Wave,
            ActivityGlyph::Pause,
            ActivityGlyph::Listing,
        ];
        let icons: Vec<&str> = glyphs.iter().copied().map(glyph_icon).collect();
        for (i, a) in icons.iter().enumerate() {
            assert!(!a.is_empty());
            for b in icons.iter().skip(i + 1) {
                assert_ne!(a, b, "glyphs must not share an icon");
            }
        }
    }
}
