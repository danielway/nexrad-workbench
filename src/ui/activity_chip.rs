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
//! - **L0 restraint.** A casual viewer with nothing happening sees the glyph
//!   alone; the words appear on hover or once Advanced is on.

use super::colors::{acquisition as acq_colors, ui as ui_colors};
use crate::core::activity::{ActivityGlyph, ActivityState, ActivityVm};
use crate::core::Intent;
use crate::state::AppState;
use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icons;

/// Sine period for the activity pulse, in radians per second.
const PULSE_RATE: f64 = 3.0;

fn glyph_icon(glyph: ActivityGlyph) -> &'static str {
    match glyph {
        ActivityGlyph::Check => icons::CHECK_CIRCLE,
        ActivityGlyph::ArrowDown => icons::ARROW_DOWN,
        ActivityGlyph::Gear => icons::GEAR,
        ActivityGlyph::Wave => icons::WAVE_SINE,
        ActivityGlyph::Pause => icons::PAUSE,
    }
}

/// Hover text explaining the current state in plain language.
fn hover_text(state: ActivityState) -> &'static str {
    match state {
        ActivityState::UpToDate => "Everything you're viewing is downloaded",
        ActivityState::Downloading { .. } => "Downloading scans — click for details",
        ActivityState::Processing => "Decoding downloaded data — click for details",
        ActivityState::Streaming => "Receiving live data — click for details",
        ActivityState::Paused { .. } => "Downloads paused — click to resume",
    }
}

/// Render the chip. `font_size` differs between the desktop transport (11) and
/// the mobile top bar (12), which is the only thing that varies between them.
pub(super) fn render_activity_chip(
    ui: &mut egui::Ui,
    vm: &ActivityVm,
    state: &mut AppState,
    font_size: f32,
) {
    let idle = vm.state == ActivityState::UpToDate;
    // Level-0 viewers get a bare glyph when nothing is happening; the label
    // would be chrome without information.
    let show_label = !idle || state.show_advanced();

    let animate = vm.headline.animate && !super::timeline::reduced_motion(ui.ctx());
    let tint = chip_tint(ui, idle, animate);

    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;

        let mut text = glyph_icon(vm.headline.glyph).to_string();
        if let Some(count) = vm.headline.count {
            text.push(' ');
            text.push_str(&count.to_string());
        }
        if show_label {
            text.push(' ');
            text.push_str(vm.headline.label);
        }

        let resp = ui.add(
            egui::Button::new(RichText::new(text).size(font_size).strong().color(tint))
                .frame(false),
        );
        if resp.clicked() {
            clicked = true;
        }
        resp.on_hover_text(hover_text(vm.state));

        // Failure tick — independent of the activity state, because a failed
        // scan coexists with an in-progress one.
        if vm.failed.count > 0 {
            let label = format!("{} {}", icons::WARNING, vm.failed.count);
            let resp = ui.add(
                egui::Button::new(
                    RichText::new(label)
                        .size(font_size)
                        .strong()
                        .color(acq_colors::FAILED),
                )
                .frame(false),
            );
            if resp.clicked() {
                clicked = true;
            }
            resp.on_hover_text(match vm.failed.first_error.as_deref() {
                Some(err) => format!("Some downloads failed ({err}) — click to retry"),
                None => "Some downloads failed — click to retry".to_string(),
            });
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
        return ui_colors::label(true);
    }
    let base = ui_colors::ACTIVE;
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
        ];
        let icons: Vec<&str> = glyphs.iter().copied().map(glyph_icon).collect();
        for (i, a) in icons.iter().enumerate() {
            assert!(!a.is_empty());
            for b in icons.iter().skip(i + 1) {
                assert_ne!(a, b, "glyphs must not share an icon");
            }
        }
    }

    /// Every state has non-empty hover text — the chip is the only always-on
    /// acquisition affordance, so it must always explain itself.
    #[wasm_bindgen_test]
    fn every_state_has_hover_text() {
        for state in [
            ActivityState::UpToDate,
            ActivityState::Downloading { scans: 1 },
            ActivityState::Processing,
            ActivityState::Streaming,
            ActivityState::Paused { queued: 1 },
        ] {
            assert!(!hover_text(state).is_empty());
        }
    }
}
