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

pub(crate) fn render_mobile_chrome(ctx: &egui::Context, state: &mut AppState) {
    // Bottommost — the icon action bar.
    egui::TopBottomPanel::bottom("mobile_action_bar")
        .resizable(false)
        .exact_height(ACTION_BAR_HEIGHT)
        .show(ctx, |ui| {
            render_action_bar(ui, state);
        });

    // Scrubber — sits just above the action bar.
    egui::TopBottomPanel::bottom("mobile_scrubber")
        .resizable(false)
        .exact_height(SCRUBBER_AREA_HEIGHT)
        .show(ctx, |ui| {
            ui.add_space(2.0);
            super::scrubber::render_scrubber(ui, state);
        });
}

/// Four equal-width icon buttons. Each reserves a full-width slot so the
/// touch target is ~25% of the viewport width regardless of icon size.
fn render_action_bar(ui: &mut egui::Ui, state: &mut AppState) {
    let total_w = ui.available_width();
    let total_h = ui.available_height();
    let slot_w = total_w / 4.0;
    let icon_size = ((total_h - 10.0) * 0.55).clamp(18.0, 24.0);

    let is_live = state.live_mode_state.is_active();
    let live_color = if is_live {
        Color32::from_rgb(220, 60, 60)
    } else {
        ui.visuals().strong_text_color()
    };
    let settings_open = state.mobile_settings_open;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.spacing_mut().item_spacing.y = 0.0;

        // 1. Radar → open site modal.
        if icon_slot(
            ui,
            slot_w,
            total_h,
            egui_phosphor::regular::CELL_TOWER,
            icon_size,
            ui.visuals().strong_text_color(),
            false,
        )
        .clicked()
        {
            state.site_modal_open = true;
            // Close the settings modal if it was open so the site modal
            // isn't sitting on top of two backdrops.
            state.mobile_settings_open = false;
        }

        // 2. Crosshair → trigger geolocation immediately. The modal's
        // polling loop (see `render_site_modal`) handles the result and
        // applies the nearest site or surfaces an error.
        if icon_slot(
            ui,
            slot_w,
            total_h,
            egui_phosphor::regular::CROSSHAIR,
            icon_size,
            ui.visuals().strong_text_color(),
            false,
        )
        .clicked()
        {
            state.mobile_geolocate_requested = true;
            state.mobile_settings_open = false;
        }

        // 3. Broadcast → toggle live mode.
        if icon_slot(
            ui,
            slot_w,
            total_h,
            egui_phosphor::regular::BROADCAST,
            icon_size,
            live_color,
            is_live,
        )
        .clicked()
        {
            if is_live {
                state.live_mode_state.stop(LiveExitReason::UserStopped);
                state.playback_state.time_model.disable_realtime_lock();
                state.playback_state.playing = false;
            } else {
                state.push_command(crate::state::AppCommand::StartLive);
                state.playback_state.speed = PlaybackSpeed::Realtime;
            }
        }

        // 4. Ellipsis → open/close the settings modal.
        if icon_slot(
            ui,
            slot_w,
            total_h,
            egui_phosphor::regular::DOTS_THREE,
            icon_size,
            ui.visuals().strong_text_color(),
            settings_open,
        )
        .clicked()
        {
            state.mobile_settings_open = !settings_open;
            if state.mobile_settings_open {
                state.mobile_settings_tab = MobileSettingsTab::default();
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

pub(super) fn toggle_play(state: &mut AppState) {
    if state.playback_state.playing {
        if state.live_mode_state.is_active() {
            state.live_mode_state.stop(LiveExitReason::UserStopped);
            state.playback_state.time_model.disable_realtime_lock();
        }
        state.playback_state.playing = false;
    } else if state.playback_state.is_playback_allowed() {
        state.playback_state.playing = true;
    }
}

pub(super) fn step_frame(state: &mut AppState, direction: isize) {
    use crate::state::PlaybackMode;

    let current_pos = state.playback_state.playback_position();
    if state.live_mode_state.is_active() {
        state.live_mode_state.stop(LiveExitReason::UserJogged);
        state.playback_state.time_model.disable_realtime_lock();
    }
    match state.playback_state.playback_mode() {
        PlaybackMode::Macro => {
            state.playback_state.step_macro_frame(direction);
        }
        PlaybackMode::Micro => {
            let step = state
                .playback_state
                .speed
                .timeline_seconds_per_real_second();
            let fallback = current_pos + step * direction as f64;
            let new_pos = match &state.viz_state.elevation_selection {
                crate::state::ElevationSelection::Fixed {
                    elevation_number, ..
                } => {
                    if direction < 0 {
                        state
                            .radar_timeline
                            .prev_matching_sweep_end_by_number(current_pos, *elevation_number)
                            .unwrap_or(fallback)
                    } else {
                        state
                            .radar_timeline
                            .next_matching_sweep_end_by_number(current_pos, *elevation_number)
                            .unwrap_or(fallback)
                    }
                }
                crate::state::ElevationSelection::Latest => {
                    if direction < 0 {
                        state
                            .radar_timeline
                            .prev_any_sweep_end(current_pos)
                            .unwrap_or(fallback)
                    } else {
                        state
                            .radar_timeline
                            .next_any_sweep_end(current_pos)
                            .unwrap_or(fallback)
                    }
                }
            };
            state.playback_state.set_playback_position(new_pos);
        }
    }
}
