//! Compact mobile top bar (one-instrument §13): the same instrument at
//! 390 px. The site, the frame time with its zone and the age pill, and the
//! alerts chip as a count — all read from the same
//! [`crate::core::top_bar::TopBarVm`] as the desktop bar. The activity chip
//! sits in the transport, as on the desktop. No mode badge, no
//! mode-colored line and no version stamp (the version lives in the desktop
//! bar's ⋯ › About).

use super::super::layout::{Layer, LayerKind, LayoutCtx};
use super::super::top_bar::{alerts_chip, emit, readout_pill, worker_error};
use crate::core::top_bar::{TopBarIntent, TopBarVm};
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, Frame, Margin, RichText};

const TOP_BAR_CONTENT_HEIGHT: f32 = 44.0;

pub(in crate::ui) struct MobileTopBarLayer;

impl Layer for MobileTopBarLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        10
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        // Slides away with the bottom chrome while playback runs (§13): the
        // decision is `chrome.mobile_auto_hide`, resolved once per frame.
        let shown = !ctx.chrome.mobile_auto_hide.hidden;
        draw_mobile_top_bar(ctx.root_ui, shown, ctx.top_bar_vm, ctx.state);
    }
}

/// The frame time and zone; a tap toggles local / UTC.
fn mobile_readout(ui: &mut egui::Ui, vm: &TopBarVm, state: &mut AppState) {
    let r = &vm.readout;
    let color = if r.has_frame {
        theme::TEXT
    } else {
        theme::TEXT_FAINT
    };
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 4.0;
    let mut tapped = ui
        .add(egui::Button::new(theme::readout(&r.time).color(color)).frame(false))
        .clicked();
    tapped |= ui
        .add(
            egui::Button::new(
                RichText::new(&r.zone)
                    .size(theme::CAPTION)
                    .color(theme::TEXT_DIM),
            )
            .frame(false),
        )
        .clicked();
    ui.spacing_mut().item_spacing.x = spacing;
    if tapped {
        emit(state, TopBarIntent::ToggleTimeZone);
    }
}

fn draw_mobile_top_bar(root_ui: &mut egui::Ui, shown: bool, vm: &TopBarVm, state: &mut AppState) {
    let ctx = root_ui.ctx().clone();
    // iOS safe area: when installed as a home-screen PWA, the canvas extends
    // under the translucent status bar / notch. Pad the top so OS icons
    // don't overlap our content.
    let (inset_top, _inset_right, _inset_bottom, _inset_left) = super::safe_area_insets();

    let panel_fill = ctx.global_style().visuals.panel_fill;

    // Zero inner-margin frame so the content sits as high as possible —
    // egui's default top-panel frame adds a couple of pixels of padding on
    // every side, which is noticeable below the iOS status bar.
    let frame = Frame::NONE.fill(panel_fill).inner_margin(Margin::ZERO);

    // `show_collapsible` slides the bar in and out over egui's 200 ms
    // animation (core::phone::CHROME_SLIDE_SECS).
    root_ui.style_mut().animation_time = crate::core::phone::CHROME_SLIDE_SECS;
    let mut expanded = shown;
    let response = egui::Panel::top("mobile_top_bar")
        .resizable(false)
        .exact_size(inset_top + TOP_BAR_CONTENT_HEIGHT)
        .frame(frame)
        .show_collapsible(root_ui, &mut expanded, |ui| {
            if inset_top > 0.0 {
                ui.add_space(inset_top);
            }

            ui.horizontal_centered(|ui| {
                ui.add_space(8.0);

                // Site — tapping opens the site dialog.
                if ui
                    .button(
                        RichText::new(format!(
                            "{} {}",
                            vm.site.id,
                            egui_phosphor::regular::CARET_DOWN
                        ))
                        .strong(),
                    )
                    .clicked()
                {
                    emit(state, TopBarIntent::OpenSitePicker);
                }

                mobile_readout(ui, vm, state);
                if let Some(pill) = &vm.pill {
                    readout_pill(ui, pill);
                }
                alerts_chip(ui, &vm.alerts, state, true);

                // Worker error — critical, must be visible on mobile too.
                if let Some(err) = &vm.worker_error {
                    worker_error(ui, err, state);
                }
            });
        });
    let anchor = response.map_or(inset_top, |r| r.response.rect.bottom());
    super::super::toast::set_anchor(&ctx, anchor);
}
