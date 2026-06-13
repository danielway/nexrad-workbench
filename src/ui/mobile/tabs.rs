//! Mobile bottom chrome: a condensed transport row over the scrubber strip
//! (spec §13 phone: "Condensed transport (play · LIVE · speed · loop preset)
//! over ~44px strip").
//!
//! Two stacked `TopBottomPanel::bottom` panels:
//!   1. Transport row (~48px) — bottommost. The primary controls, directly
//!      visible per spec: frame step ‹ ›, Play/Pause, a stateful LIVE button
//!      (re-tether / go-live / freeze), a compact speed tap-cycle, and a loop
//!      preset menu. A slim trailing cluster keeps geolocate + the settings
//!      sheet (deeper Level-2 controls) reachable; the radar-site picker lives
//!      in the top-bar site chip.
//!   2. Scrubber (~44px) — topmost. The coverage strip (faint=available,
//!      solid=cached, red=now, neutral thumb, REJOIN pill, long-press→inspector).

use crate::state::{AppState, LiveExitReason, LoopPreset, MobileSettingsTab, PlaybackMode};
use eframe::egui::{self, Color32, RichText};

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
    fn visible(&self, ctx: &super::super::layout::LayoutCtx) -> bool {
        // Auto-hide during playback (spec §13): when hidden the panel isn't
        // dispatched at all, so the canvas reclaims its space (full-bleed). The
        // hide decision is resolved once per frame (before layout) into
        // `mobile_auto_hide.hidden`; reading it here keeps the top bar, bottom
        // chrome, and canvas reveal-tap perfectly in lockstep.
        !ctx.chrome.mobile_auto_hide.hidden
    }
    fn render(&self, ctx: &mut super::super::layout::LayoutCtx) {
        draw_mobile_chrome(
            ctx.ctx,
            ctx.state,
            ctx.timeline,
            ctx.live,
            ctx.playback,
            ctx.chrome,
        );
    }
}

fn draw_mobile_chrome(
    ctx: &egui::Context,
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

    // Bottommost — the condensed transport row.
    let transport = egui::TopBottomPanel::bottom("mobile_transport_bar")
        .resizable(false)
        .exact_height(TRANSPORT_BAR_HEIGHT + inset_bottom)
        .show(ctx, |ui| {
            render_transport_row(ui, state, timeline, live, playback, chrome);
        });

    // Scrubber — sits just above the transport row.
    let scrubber = egui::TopBottomPanel::bottom("mobile_scrubber")
        .resizable(false)
        .exact_height(SCRUBBER_AREA_HEIGHT)
        .show(ctx, |ui| {
            ui.add_space(2.0);
            super::scrubber::render_scrubber(ui, state, timeline, live, playback, chrome);
        });

    // Touching the chrome keeps it on: bump the auto-hide idle timer whenever a
    // pointer is down over either bottom panel, so using the transport/scrubber
    // doesn't let it slide away mid-interaction (and resets the idle window when
    // the touch lifts). The `gesture_active` guard holds it up during the press;
    // this is what extends the window afterward.
    let pressed_chrome = ctx.input(|i| i.pointer.any_down())
        && (transport.response.contains_pointer() || scrubber.response.contains_pointer());
    if pressed_chrome {
        chrome.mobile_auto_hide.touch(ctx.input(|i| i.time));
    }
}

/// The condensed transport row. Left cluster is the spec's directly-visible
/// transport (step · play · LIVE · speed · loop); a slim trailing cluster keeps
/// geolocate + the settings sheet reachable without burying the transport.
fn render_transport_row(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    let interactive = live.app_mode != crate::state::AppMode::Idle;
    let btn_h = 38.0;

    ui.horizontal_centered(|ui| {
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.x = 4.0;

        // Step back — a seek, so it detaches first (handled in `step_frame`).
        if ui
            .add_enabled(
                interactive,
                egui::Button::new(RichText::new(egui_phosphor::regular::CARET_LEFT).size(15.0))
                    .min_size(egui::vec2(34.0, btn_h)),
            )
            .clicked()
        {
            step_frame(state, timeline, live, playback, -1);
        }

        // Play/Pause — the primary control (spec §13). While tethered the feed
        // is conceptually playing, so this reads PAUSE and freezes; in archive
        // it's ordinary play/pause. All branching lives in
        // `transport::toggle_play_pause`.
        let tethered = playback.state.time_model.is_pinned();
        let play_icon = if tethered || playback.state.playing {
            egui_phosphor::regular::PAUSE
        } else {
            egui_phosphor::regular::PLAY
        };
        if ui
            .add_enabled(
                interactive,
                egui::Button::new(RichText::new(play_icon).size(20.0))
                    .min_size(egui::vec2(52.0, btn_h)),
            )
            .clicked()
        {
            toggle_play(state, timeline, live, playback);
        }

        // Step forward.
        if ui
            .add_enabled(
                interactive,
                egui::Button::new(RichText::new(egui_phosphor::regular::CARET_RIGHT).size(15.0))
                    .min_size(egui::vec2(34.0, btn_h)),
            )
            .clicked()
        {
            step_frame(state, timeline, live, playback, 1);
        }

        ui.add_space(2.0);
        render_live_button(ui, state, live, playback, btn_h);
        render_speed_button(ui, playback, btn_h);
        render_loop_button(ui, state, playback, interactive);

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

/// Stateful LIVE button (spec §7, mobile twin of the desktop transport's). One
/// glance/one tap to the live edge wherever the user is:
/// - **Tethered** (`AppMode::Live`): solid red "● LIVE"; tap freezes the feed
///   (mobile has no now-line cap, so the LIVE button owns stop here).
/// - **Detached** (stream running in background): hollow "● LIVE"; tap rejoins.
/// - **No stream** (idle): hollow "● GO LIVE"; tap starts a stream.
fn render_live_button(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    btn_h: f32,
) {
    use crate::ui::colors::live;
    let dot = egui_phosphor::regular::BROADCAST;
    let tethered = live.app_mode == crate::state::AppMode::Live;
    let streaming = live.mode_state.is_active();

    if tethered {
        let pulse = live.mode_state.pulse_alpha();
        let alpha = (200.0 + 55.0 * pulse) as u8;
        let fill = Color32::from_rgba_unmultiplied(
            live::STREAMING.r(),
            live::STREAMING.g(),
            live::STREAMING.b(),
            alpha,
        );
        let label = RichText::new(format!("{dot} LIVE"))
            .size(12.0)
            .strong()
            .color(Color32::WHITE);
        if ui
            .add(
                egui::Button::new(label)
                    .fill(fill)
                    .min_size(egui::vec2(0.0, btn_h)),
            )
            .on_hover_text("Tethered to live — tap to freeze")
            .clicked()
        {
            live.stop(LiveExitReason::UserStopped);
            playback.state.exit_live(crate::state::FreezeAt::Keep);
            playback.state.playing = false;
        }
        return;
    }

    if streaming {
        let label = RichText::new(format!("{dot} LIVE"))
            .size(12.0)
            .strong()
            .color(live::STREAMING);
        if ui
            .add(
                egui::Button::new(label)
                    .fill(Color32::TRANSPARENT)
                    .min_size(egui::vec2(0.0, btn_h)),
            )
            .on_hover_text("Stream running in background — tap to rejoin live")
            .clicked()
        {
            state.push_command(crate::state::AppCommand::ReturnToLive);
        }
        return;
    }

    // No stream: hollow GO LIVE invitation.
    let label = RichText::new(format!("{dot} GO LIVE"))
        .size(12.0)
        .strong()
        .color(crate::ui::colors::timeline::NOW_IDLE);
    if ui
        .add(
            egui::Button::new(label)
                .fill(Color32::TRANSPARENT)
                .min_size(egui::vec2(0.0, btn_h)),
        )
        .on_hover_text("Stream live from now")
        .clicked()
    {
        playback.state.clear_selection();
        state.push_command(crate::state::AppCommand::StartLive);
        playback.state.speed = crate::state::PlaybackSpeed::Realtime;
    }
}

/// Compact speed control: a tap-cycle button showing the current speed in the
/// mode's notation (× in Micro, fps in Macro) that walks a curated ladder
/// ([`PlaybackSpeed::mobile_cycle`]) on each tap. The full speed list stays in
/// the settings sheet for users who want a specific value.
fn render_speed_button(ui: &mut egui::Ui, playback: &mut crate::subsystem::Playback, btn_h: f32) {
    let mode = playback.state.effective_playback_mode();
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

/// Loop-preset control (spec §13 "loop preset"; §15 cut #2 keeps presets on
/// mobile). A menu button reusing the same `ApplyLoopPreset` / `ClearLoop`
/// commands as desktop — moved out of the settings sheet into the transport row.
fn render_loop_button(
    ui: &mut egui::Ui,
    state: &mut AppState,
    playback: &crate::subsystem::Playback,
    interactive: bool,
) {
    use crate::ui::colors::{live, ui as ui_colors};
    let has_loop =
        playback.state.loop_window.is_some() || playback.state.time_model.playback_bounds.is_some();
    let tint = if has_loop {
        live::STREAMING
    } else {
        ui_colors::value(state.is_dark)
    };
    ui.add_enabled_ui(interactive, |ui| {
        ui.menu_button(
            RichText::new(egui_phosphor::regular::REPEAT)
                .size(16.0)
                .color(tint),
            |ui| {
                let header = match playback.state.loop_window {
                    Some(w) => {
                        let pin = if w.pinned { " · pinned" } else { "" };
                        format!("Loop · {}{}", w.basis.label(), pin)
                    }
                    None => "Loop".to_string(),
                };
                ui.label(RichText::new(header).size(11.0).weak());
                for preset in LoopPreset::menu() {
                    if ui.button(preset.label()).clicked() {
                        state.push_command(crate::state::AppCommand::ApplyLoopPreset(*preset));
                        ui.close();
                    }
                }
                if has_loop {
                    ui.separator();
                    if ui.button("Clear loop").clicked() {
                        state.push_command(crate::state::AppCommand::ClearLoop);
                        ui.close();
                    }
                }
            },
        )
        .response
        .on_hover_text("Loop presets")
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    });
}

// ---------------------------------------------------------------------------
// Playback helpers (shared with the settings modal).
// ---------------------------------------------------------------------------

pub(super) fn toggle_play(
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
    // Same decoupled play/pause as desktop: in live this freezes the feed;
    // stopping the stream is the LIVE button's job.
    crate::ui::transport::toggle_play_pause(state, timeline, live, playback);
}

pub(super) fn step_frame(
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    direction: isize,
) {
    use crate::state::PlaybackMode;

    let current_pos = playback.state.playback_position();
    // Jogging detaches the playhead; a running stream keeps ingesting unless
    // the data-saver policy stops it.
    live.detach_playhead(
        &mut playback.state,
        state.frame_now.secs(),
        state.pause_stream_while_reviewing,
    );
    match playback.state.playback_mode() {
        PlaybackMode::Macro => {
            playback.state.step_macro_frame(direction);
        }
        PlaybackMode::Micro => {
            let step = playback.state.speed.timeline_seconds_per_real_second();
            let fallback = current_pos + step * direction as f64;
            let new_pos = match &state.viz_state.elevation_selection {
                crate::state::ElevationSelection::Fixed {
                    elevation_number, ..
                } => {
                    if direction < 0 {
                        timeline
                            .scans
                            .prev_matching_sweep_end_by_number(current_pos, *elevation_number)
                            .unwrap_or(fallback)
                    } else {
                        timeline
                            .scans
                            .next_matching_sweep_end_by_number(current_pos, *elevation_number)
                            .unwrap_or(fallback)
                    }
                }
                crate::state::ElevationSelection::Latest => {
                    if direction < 0 {
                        timeline
                            .scans
                            .prev_any_sweep_end(current_pos)
                            .unwrap_or(fallback)
                    } else {
                        timeline
                            .scans
                            .next_any_sweep_end(current_pos)
                            .unwrap_or(fallback)
                    }
                }
            };
            playback.state.set_playback_position(new_pos);
        }
    }
}
