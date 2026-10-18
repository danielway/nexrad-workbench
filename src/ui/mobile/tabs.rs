//! Mobile bottom chrome: a condensed transport row under the mini timeline
//! (one-instrument §13).
//!
//! Two stacked `Panel::bottom` panels:
//!   1. Transport row (~48px) — bottommost. Frame step ‹ ›, Play/Pause, the
//!      selection chip, the speed, the activity dot and the sheet button —
//!      the desktop transport's controls from the same view-models.
//!   2. The mini timeline (~56px) — topmost. The desktop strip's real cells,
//!      marks, playhead and now cap at one of three zoom stops (cuts,
//!      volumes, the day), with its zoom pill (`ui::timeline::mini`).

use crate::core::transport::{PlayButton, SpeedControl, StepDir};
use crate::core::Intent;
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, RichText};

const TRANSPORT_BAR_HEIGHT: f32 = 48.0;
const MINI_AREA_HEIGHT: f32 = crate::ui::timeline::mini::MINI_TIMELINE_H + 4.0;

pub(in crate::ui) struct MobileChromeLayer;

impl super::super::layout::Layer for MobileChromeLayer {
    fn kind(&self) -> super::super::layout::LayerKind {
        super::super::layout::LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        20
    }
    fn render(&self, ctx: &mut super::super::layout::LayoutCtx) {
        draw_mobile_chrome(ctx);
    }
}

fn draw_mobile_chrome(ctx: &mut super::super::layout::LayoutCtx) {
    let super::super::layout::LayoutCtx {
        root_ui,
        state,
        timeline,
        live,
        playback,
        acquisition,
        chrome,
        activity_vm,
        ..
    } = ctx;
    // iOS safe area: when installed as a home-screen PWA, the canvas extends
    // under the home indicator reservation. Pad the transport row below so the
    // controls don't sit flush with the bottom edge of the screen.
    let (_t, _r, inset_bottom, _l) = super::safe_area_insets();

    // Both panels slide away together while playback runs (§13); the
    // decision is `chrome.mobile_auto_hide`, resolved once per frame. A press
    // anywhere resets its clock, so using the transport keeps it up.
    let shown = !chrome.mobile_auto_hide.hidden;
    // The strip claims its own pinch afresh each frame it draws.
    chrome.phone_strip_claims_touch = false;

    root_ui.style_mut().animation_time = crate::core::phone::CHROME_SLIDE_SECS;
    // Bottommost — the condensed transport row.
    let mut expanded = shown;
    egui::Panel::bottom("mobile_transport_bar")
        .resizable(false)
        .exact_size(TRANSPORT_BAR_HEIGHT + inset_bottom)
        .show_collapsible(root_ui, &mut expanded, |ui| {
            render_transport_row(ui, state, live, playback, chrome, activity_vm);
        });

    // The mini timeline — just above the transport row.
    let mut expanded = shown;
    egui::Panel::bottom("mobile_mini_timeline")
        .resizable(false)
        .exact_size(MINI_AREA_HEIGHT)
        .show_collapsible(root_ui, &mut expanded, |ui| {
            ui.add_space(2.0);
            crate::ui::timeline::mini::render_mini_timeline(
                ui,
                state,
                timeline,
                live,
                playback,
                acquisition,
                chrome,
            );
        });
}

/// The condensed transport row: step, play, step, the selection chip (with
/// the lag when detached) and the speed on the leading edge; the activity
/// dot and the sheet button on the trailing edge. The same controls and
/// view-models as the desktop transport, folded to 390 px.
fn render_transport_row(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    activity_vm: &crate::core::activity::ActivityVm,
) {
    // Never disabled: the core decides what each press means (§2).
    let btn_h = 38.0;
    let vm = crate::core::transport::transport_vm(&playback.state);

    // The trailing cluster (sheet button, activity dot / error badge) lays
    // out first, right to left, so its real width is known; the leading
    // controls then take what is left, and the chip gets that minus the
    // speed, so nothing can draw over anything else at 360 px.
    ui.horizontal_centered(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.x = 2.0;

            // The sheet: the rail's tabs. It opens on the rail's tab; its
            // backdrop owns closing.
            let open = chrome.mobile_settings_open;
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(egui_phosphor::regular::SLIDERS)
                            .size(18.0)
                            .color(if open {
                                theme::SELECTED
                            } else {
                                theme::TEXT_DIM
                            }),
                    )
                    .frame(false)
                    .min_size(egui::vec2(34.0, btn_h)),
                )
                .on_hover_text("View, layers, events and settings")
                .clicked()
            {
                chrome.mobile_settings_open = true;
            }

            // The activity dot: the desktop chip with its words folded.
            crate::ui::activity_chip::render_activity_chip(ui, activity_vm, state, 13.0, true);

            ui.add_space(4.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                render_leading_controls(ui, state, live, playback, &vm, btn_h);
            });
        });
    });
}

/// Step, play, step, the selection chip and the speed, in the width the
/// trailing cluster left.
fn render_leading_controls(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    vm: &crate::core::transport::TransportVm,
    btn_h: f32,
) {
    ui.add_space(4.0);
    let gap = 3.0;
    ui.spacing_mut().item_spacing.x = gap;

    if ui
        .add(
            egui::Button::new(RichText::new(egui_phosphor::regular::CARET_LEFT).size(15.0))
                .min_size(egui::vec2(30.0, btn_h)),
        )
        .clicked()
    {
        step_frame(state, StepDir::Back);
    }

    // Play/Pause: the same button as the desktop transport, from the same
    // view-model.
    let play_icon = match vm.play {
        PlayButton::Pause => egui_phosphor::regular::PAUSE,
        PlayButton::Play => egui_phosphor::regular::PLAY,
    };
    if ui
        .add(
            egui::Button::new(RichText::new(play_icon).size(20.0))
                .min_size(egui::vec2(44.0, btn_h)),
        )
        .clicked()
    {
        toggle_play(state);
    }

    if ui
        .add(
            egui::Button::new(RichText::new(egui_phosphor::regular::CARET_RIGHT).size(15.0))
                .min_size(egui::vec2(30.0, btn_h)),
        )
        .clicked()
    {
        step_frame(state, StepDir::Forward);
    }

    ui.add_space(2.0);
    // The chip's ceiling: the width left after the speed control.
    let chip_max = ui.available_width() - speed_width(ui, &vm.speed) - gap;
    let view_start = playback.state.timeline_view_start;
    crate::ui::selection_chip::render_selection_chip(
        ui,
        state,
        live,
        playback,
        (view_start, view_start + playback.state.view_width_secs()),
        &crate::ui::selection_chip::ChipStyle {
            text_size: 12.0,
            min_height: btn_h,
            // The phone's stop leaves the playhead where it is; the mini
            // timeline's cap is the way back to now.
            stop_placement: crate::core::transport::LiveStopPlacement::InPlace,
            compact: true,
            max_width: Some(chip_max),
        },
    );

    // The speed: the desktop's menu, or the fixed readout while pinned in
    // Rays.
    match &vm.speed {
        SpeedControl::Locked { label } => {
            ui.label(
                RichText::new(label)
                    .size(theme::LABEL)
                    .color(theme::TEXT_DIM),
            );
        }
        SpeedControl::Menu { selected, choices } => {
            egui::ComboBox::from_id_salt("mobile_speed")
                .selected_text(RichText::new(selected.as_str()).size(13.0))
                .width(SPEED_MENU_W)
                .height(320.0)
                .show_ui(ui, |ui| {
                    for (speed, label) in choices {
                        let on = *speed == playback.state.speed;
                        if ui
                            .add(
                                egui::Button::selectable(on, label.as_str())
                                    .min_size(egui::vec2(80.0, 32.0)),
                            )
                            .clicked()
                            && !on
                        {
                            state.push_command(Intent::SetSpeed(*speed));
                        }
                    }
                });
        }
    }
}

/// The speed menu's outer width (wider only if its label needs it).
const SPEED_MENU_W: f32 = 52.0;

/// The width the speed control will take, measured before it is drawn so
/// the chip can leave room for it.
fn speed_width(ui: &egui::Ui, speed: &SpeedControl) -> f32 {
    let text_w = |label: &str, size: f32| {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(
                label.to_owned(),
                egui::FontId::proportional(size),
                egui::Color32::WHITE,
            )
            .size()
            .x
        })
    };
    match speed {
        SpeedControl::Locked { label } => text_w(label, theme::LABEL),
        SpeedControl::Menu { selected, .. } => {
            let s = ui.spacing();
            let inner = text_w(selected, 13.0) + s.icon_spacing + s.icon_width;
            (inner + 2.0 * s.button_padding.x).max(SPEED_MENU_W)
        }
    }
}

// ---------------------------------------------------------------------------
// Playback helpers (shared with the settings modal).
// ---------------------------------------------------------------------------

pub(super) fn toggle_play(state: &mut AppState) {
    // Same decoupled play/pause as desktop: in live this freezes the feed;
    // stopping the stream is the LIVE button's job.
    state.push_command(crate::core::Intent::TogglePlayPause);
}

pub(super) fn step_frame(state: &mut AppState, dir: StepDir) {
    // The same step as the desktop buttons and the arrow keys.
    state.push_command(crate::core::Intent::Step {
        dir,
        unit: crate::core::transport::StepUnit::Frame,
    });
}
