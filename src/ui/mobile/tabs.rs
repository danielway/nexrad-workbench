//! Mobile bottom chrome: a condensed transport row over the scrubber strip
//! (spec §13 phone: "Condensed transport (play · LIVE · speed · loop preset)
//! over ~44px strip").
//!
//! Two stacked `Panel::bottom` panels:
//!   1. Transport row (~48px) — bottommost. The primary controls, directly
//!      visible per spec: frame step ‹ ›, Play/Pause, a stateful LIVE button
//!      (re-tether / go-live / freeze), a compact speed tap-cycle, and a loop
//!      preset menu. A slim trailing cluster keeps geolocate + the settings
//!      sheet (deeper Level-2 controls) reachable; the radar-site picker lives
//!      in the top-bar site chip.
//!   2. Scrubber (~44px) — topmost. The coverage strip (faint=available,
//!      solid=cached, red=now, neutral thumb, REJOIN pill, long-press→inspector).

use crate::core::transport::StepDir;
use crate::core::PlaybackMode;
use crate::state::{AppState, MobileSettingsTab};
use eframe::egui::{self, RichText};

const TRANSPORT_BAR_HEIGHT: f32 = 48.0;
const SCRUBBER_AREA_HEIGHT: f32 = super::scrubber::SCRUBBER_HEIGHT + 6.0;

pub(in crate::ui) struct MobileChromeLayer;

impl super::super::layout::Layer for MobileChromeLayer {
    fn kind(&self) -> super::super::layout::LayerKind {
        super::super::layout::LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        20
    }
    fn render(&self, ctx: &mut super::super::layout::LayoutCtx) {
        draw_mobile_chrome(
            ctx.root_ui,
            ctx.state,
            ctx.timeline,
            ctx.live,
            ctx.playback,
            ctx.chrome,
        );
    }
}

fn draw_mobile_chrome(
    root_ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    // iOS safe area: when installed as a home-screen PWA, the canvas extends
    // under the home indicator reservation. Pad the transport row below so the
    // controls don't sit flush with the bottom edge of the screen.
    let (_t, _r, inset_bottom, _l) = super::safe_area_insets();

    // Both panels slide away together while playback runs (§13); the
    // decision is `chrome.mobile_auto_hide`, resolved once per frame. A press
    // anywhere resets its clock, so using the transport keeps it up.
    let shown = !chrome.mobile_auto_hide.hidden;

    root_ui.style_mut().animation_time = crate::core::phone::CHROME_SLIDE_SECS;
    // Bottommost — the condensed transport row.
    let mut expanded = shown;
    egui::Panel::bottom("mobile_transport_bar")
        .resizable(false)
        .exact_size(TRANSPORT_BAR_HEIGHT + inset_bottom)
        .show_collapsible(root_ui, &mut expanded, |ui| {
            render_transport_row(ui, state, timeline, live, playback, chrome);
        });

    // Scrubber — sits just above the transport row.
    let mut expanded = shown;
    egui::Panel::bottom("mobile_scrubber")
        .resizable(false)
        .exact_size(SCRUBBER_AREA_HEIGHT)
        .show_collapsible(root_ui, &mut expanded, |ui| {
            ui.add_space(2.0);
            super::scrubber::render_scrubber(ui, state, timeline, live, playback, chrome);
        });
}

/// The condensed transport row. Left cluster is the spec's directly-visible
/// transport (step · play · LIVE · speed · loop); a slim trailing cluster keeps
/// geolocate + the settings sheet reachable without burying the transport.
fn render_transport_row(
    ui: &mut egui::Ui,
    state: &mut AppState,
    _timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    // Never disabled: the core decides what each press means (§2).
    let btn_h = 38.0;

    ui.horizontal_centered(|ui| {
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.x = 4.0;

        // Step back — a seek, so it detaches first (handled in `step_frame`).
        if ui
            .add(
                egui::Button::new(RichText::new(egui_phosphor::regular::CARET_LEFT).size(15.0))
                    .min_size(egui::vec2(34.0, btn_h)),
            )
            .clicked()
        {
            step_frame(state, StepDir::Back);
        }

        // Play/Pause — the primary control (spec §13). While tethered the feed
        // is conceptually playing, so this reads PAUSE and freezes; in archive
        // it's ordinary play/pause. All branching lives in
        // `core::transport::reduce_toggle_play_pause`, reached via
        // `Intent::TogglePlayPause`.
        let tethered = playback.state.is_pinned();
        let play_icon = if tethered || playback.state.playing {
            egui_phosphor::regular::PAUSE
        } else {
            egui_phosphor::regular::PLAY
        };
        if ui
            .add(
                egui::Button::new(RichText::new(play_icon).size(20.0))
                    .min_size(egui::vec2(52.0, btn_h)),
            )
            .clicked()
        {
            toggle_play(state);
        }

        // Step forward.
        if ui
            .add(
                egui::Button::new(RichText::new(egui_phosphor::regular::CARET_RIGHT).size(15.0))
                    .min_size(egui::vec2(34.0, btn_h)),
            )
            .clicked()
        {
            step_frame(state, StepDir::Forward);
        }

        ui.add_space(2.0);
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
                // Mobile has no now-line cap: stopping leaves the playhead
                // where it is.
                stop_placement: crate::core::transport::LiveStopPlacement::InPlace,
                compact: true,
            },
        );
        render_stream_health(ui, live);
        render_speed_button(ui, playback, btn_h);

        // Trailing cluster: geolocate + settings sheet, right-aligned so the
        // transport keeps the leading edge. Kept compact so they never crowd
        // the directly-visible transport above.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.x = 2.0;

            let settings_open = chrome.mobile_settings_open;
            let settings_tint = if settings_open {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().weak_text_color()
            };
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(egui_phosphor::regular::SLIDERS)
                            .size(18.0)
                            .color(settings_tint),
                    )
                    .frame(false)
                    .min_size(egui::vec2(34.0, btn_h)),
                )
                .on_hover_text("More controls")
                .clicked()
            {
                chrome.mobile_settings_open = !settings_open;
                if chrome.mobile_settings_open {
                    chrome.mobile_settings_tab = MobileSettingsTab::default();
                }
            }

            if ui
                .add(
                    egui::Button::new(
                        RichText::new(egui_phosphor::regular::CROSSHAIR)
                            .size(18.0)
                            .color(ui.visuals().weak_text_color()),
                    )
                    .frame(false)
                    .min_size(egui::vec2(34.0, btn_h)),
                )
                .on_hover_text("Use my location")
                .clicked()
            {
                chrome.mobile_geolocate_requested = true;
                chrome.mobile_settings_open = false;
            }
        });
    });
}

/// Compact stream-health readout beside the mobile LIVE button. The ~360 px
/// transport row has no room for the desktop activity chip, so only the states
/// that demand attention surface here: a stalled or failed stream. Healthy activity
/// (receiving / waiting) is carried by the LIVE button itself.
fn render_stream_health(ui: &mut egui::Ui, live: &crate::subsystem::Live) {
    use crate::core::session::Stream;
    let session = &live.session;
    let word = match session.stream {
        Stream::Stalled => "stalled",
        Stream::Failed => "failed",
        _ => return,
    };
    let resp = ui.label(
        RichText::new(egui_phosphor::regular::BROADCAST)
            .size(10.0)
            .color(crate::ui::theme::legacy::WARN),
    );
    ui.label(
        RichText::new(word)
            .size(10.0)
            .monospace()
            .color(crate::ui::theme::legacy::WARN),
    );
    if let Some(hover) = session.activity_hover {
        resp.on_hover_text(hover);
    }
}

/// Compact speed control: a tap-cycle button showing the current speed in the
/// mode's notation (× in Micro, fps in Macro) that walks a curated ladder
/// ([`PlaybackSpeed::mobile_cycle`]) on each tap. The full speed list stays in
/// the settings sheet for users who want a specific value.
fn render_speed_button(ui: &mut egui::Ui, playback: &mut crate::subsystem::Playback, btn_h: f32) {
    let mode = playback.state.playback_mode();
    let label = match mode {
        PlaybackMode::Macro => playback.state.speed.macro_label(),
        PlaybackMode::Micro => playback.state.speed.label(),
    };
    if ui
        .add(egui::Button::new(RichText::new(label).size(13.0)).min_size(egui::vec2(46.0, btn_h)))
        .on_hover_text("Playback speed (tap to cycle)")
        .clicked()
    {
        playback.state.speed = playback.state.speed.mobile_cycle(mode);
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
