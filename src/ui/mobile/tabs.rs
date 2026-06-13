//! Mobile bottom chrome: icon-only action bar + scrubber.
//!
//! Two stacked `TopBottomPanel::bottom` panels:
//!   1. Action bar (56px) — bottommost. Four icon buttons:
//!      Radar → open site modal. Crosshair → geolocate and pick nearest
//!      site. Broadcast → toggle live mode. Ellipsis → open settings modal
//!      (Playback / Product / Layers / More).
//!   2. Scrubber (32px) — topmost, always visible.

use crate::state::{AppState, LiveExitReason, MobileSettingsTab, PlaybackSpeed};
use eframe::egui::{self, Color32};

const ACTION_BAR_HEIGHT: f32 = 56.0;
const SCRUBBER_AREA_HEIGHT: f32 = super::scrubber::SCRUBBER_HEIGHT + 4.0;

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
    // under the home indicator reservation. Pad the action bar below the
    // icons so they don't sit flush with the bottom edge of the screen.
    let (_t, _r, inset_bottom, _l) = super::safe_area_insets();

    // Bottommost — the icon action bar.
    egui::TopBottomPanel::bottom("mobile_action_bar")
        .resizable(false)
        .exact_height(ACTION_BAR_HEIGHT + inset_bottom)
        .show(ctx, |ui| {
            render_action_bar(ui, state, live, playback, chrome);
        });

    // Scrubber — sits just above the action bar.
    egui::TopBottomPanel::bottom("mobile_scrubber")
        .resizable(false)
        .exact_height(SCRUBBER_AREA_HEIGHT)
        .show(ctx, |ui| {
            ui.add_space(2.0);
            super::scrubber::render_scrubber(ui, state, timeline, live, playback);
        });
}

/// Four equal-width icon buttons. Each reserves a full-width slot so the
/// touch target is ~25% of the viewport width regardless of icon size.
///
/// The slot height stays fixed at `ACTION_BAR_HEIGHT` even when the hosting
/// panel is taller (iOS safe-area bottom inset); the extra space falls
/// below the icons as blank panel padding clearing the home indicator.
fn render_action_bar(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    let total_w = ui.available_width();
    let slot_h = ACTION_BAR_HEIGHT;
    let slot_w = total_w / 4.0;
    let icon_size = ((slot_h - 10.0) * 0.55).clamp(18.0, 24.0);

    let is_streaming = live.mode_state.is_active();
    let is_attached = live.app_mode == crate::state::AppMode::Live;
    let live_color = if is_streaming {
        Color32::from_rgb(220, 60, 60)
    } else {
        ui.visuals().strong_text_color()
    };
    let settings_open = chrome.mobile_settings_open;

    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.spacing_mut().item_spacing.y = 0.0;

        // 1. Radar → open site modal.
        if icon_slot(
            ui,
            slot_w,
            slot_h,
            egui_phosphor::regular::CELL_TOWER,
            icon_size,
            ui.visuals().strong_text_color(),
            false,
        )
        .clicked()
        {
            chrome.site_modal_open = true;
            // Close the settings modal if it was open so the site modal
            // isn't sitting on top of two backdrops.
            chrome.mobile_settings_open = false;
        }

        // 2. Crosshair → trigger geolocation immediately. The modal's
        // polling loop (see `SiteModalLayer`) handles the result and
        // applies the nearest site or surfaces an error.
        if icon_slot(
            ui,
            slot_w,
            slot_h,
            egui_phosphor::regular::CROSSHAIR,
            icon_size,
            ui.visuals().strong_text_color(),
            false,
        )
        .clicked()
        {
            chrome.mobile_geolocate_requested = true;
            chrome.mobile_settings_open = false;
        }

        // 3. Broadcast → live control, detach-aware: attached stops the
        // stream; detached (background ingest while browsing) re-pins
        // instantly; idle starts a stream.
        if icon_slot(
            ui,
            slot_w,
            slot_h,
            egui_phosphor::regular::BROADCAST,
            icon_size,
            live_color,
            is_streaming,
        )
        .clicked()
        {
            if is_attached {
                live.stop(LiveExitReason::UserStopped);
                playback.state.exit_live(crate::state::FreezeAt::Keep);
                playback.state.playing = false;
            } else if is_streaming {
                state.push_command(crate::state::AppCommand::ReturnToLive);
            } else {
                state.push_command(crate::state::AppCommand::StartLive);
                playback.state.speed = PlaybackSpeed::Realtime;
            }
        }

        // 4. Ellipsis → open/close the settings modal.
        if icon_slot(
            ui,
            slot_w,
            slot_h,
            egui_phosphor::regular::DOTS_THREE,
            icon_size,
            ui.visuals().strong_text_color(),
            settings_open,
        )
        .clicked()
        {
            chrome.mobile_settings_open = !settings_open;
            if chrome.mobile_settings_open {
                chrome.mobile_settings_tab = MobileSettingsTab::default();
            }
        }
    });
}

/// One icon slot in the action bar. Returns the click response. Draws an
/// optional "active" underline for toggles like Live or Settings.
fn icon_slot(
    ui: &mut egui::Ui,
    slot_w: f32,
    slot_h: f32,
    icon: &str,
    icon_size: f32,
    color: Color32,
    active: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(slot_w, slot_h), egui::Sense::click());
    let painter = ui.painter_at(rect);

    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(icon_size),
        color,
    );

    if active {
        let underline_y = rect.bottom() - 4.0;
        painter.line_segment(
            [
                egui::pos2(rect.left() + 16.0, underline_y),
                egui::pos2(rect.right() - 16.0, underline_y),
            ],
            egui::Stroke::new(2.0, color),
        );
    }

    resp
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
    // Same decoupled play/pause as desktop: in live this toggles the lookback
    // replay; stopping the stream is the broadcast button's job.
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
