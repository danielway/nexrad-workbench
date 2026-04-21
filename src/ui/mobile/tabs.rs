//! Mobile bottom chrome: tab bar + tab content + scrubber.
//!
//! Implemented as three stacked `TopBottomPanel::bottom` panels so each one
//! takes an exact slice of viewport height and the layout math doesn't fight
//! egui's internal frame padding. Panels render from bottom up in call order:
//!   1. Tab bar (42px)     — bottommost
//!   2. Tab content (160px) — scrollable, content for the active tab
//!   3. Scrubber (32px)    — topmost, always visible

use crate::state::{AppState, LiveExitReason, MobileTab, PlaybackMode, PlaybackSpeed};
use eframe::egui::{self, Color32, RichText};

const TAB_BAR_HEIGHT: f32 = 42.0;
const TAB_CONTENT_HEIGHT: f32 = 160.0;
const SCRUBBER_AREA_HEIGHT: f32 = super::scrubber::SCRUBBER_HEIGHT + 4.0;

pub(crate) fn render_mobile_chrome(ctx: &egui::Context, state: &mut AppState) {
    // Bottommost — tab bar. Because egui stacks bottom panels in call order
    // (first-added = outermost-bottom), this panel sits flush with the
    // viewport's bottom edge and can't be pushed off-screen by sibling content.
    egui::TopBottomPanel::bottom("mobile_tab_bar")
        .resizable(false)
        .exact_height(TAB_BAR_HEIGHT)
        .show(ctx, |ui| {
            render_tab_bar(ui, state);
        });

    // Tab content — above the tab bar.
    egui::TopBottomPanel::bottom("mobile_tab_content")
        .resizable(false)
        .exact_height(TAB_CONTENT_HEIGHT)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("mobile_tab_content_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| match state.mobile_active_tab {
                    MobileTab::Playback => render_playback_tab(ui, state),
                    MobileTab::Product => render_product_tab(ui, state),
                    MobileTab::Layers => render_layers_tab(ui, state),
                    MobileTab::More => render_more_tab(ui, state),
                });

            // Datetime picker popup (opens when the user taps the current-time
            // label in the Playback tab). It's an Area and renders on top.
            super::super::playback_controls::render_datetime_picker_popup(ui, state);
        });

    // Scrubber — topmost of the three bottom panels.
    egui::TopBottomPanel::bottom("mobile_scrubber")
        .resizable(false)
        .exact_height(SCRUBBER_AREA_HEIGHT)
        .show(ctx, |ui| {
            ui.add_space(2.0);
            super::scrubber::render_scrubber(ui, state);
        });
}

fn render_tab_bar(ui: &mut egui::Ui, state: &mut AppState) {
    let active = state.mobile_active_tab;
    let total_w = ui.available_width();
    let total_h = ui.available_height();
    let tab_w = total_w / MobileTab::all().len() as f32;
    // Vertical budget: icon + small gap + label, plus a bottom stripe for
    // the active-tab indicator. Size the icon relative to the available
    // height so the bar doesn't overflow when we tweak its height.
    let icon_size = ((total_h - 14.0) * 0.65).clamp(14.0, 18.0);
    let label_size = 10.0f32;
    let indicator_gap = 2.0f32;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        for tab in MobileTab::all() {
            let is_active = tab == active;
            let text_color = if is_active {
                ui.visuals().strong_text_color()
            } else {
                Color32::from_rgb(130, 130, 130)
            };
            let icon = tab_icon(tab);
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(tab_w, total_h), egui::Sense::click());
            if resp.clicked() {
                state.mobile_active_tab = tab;
            }
            let painter = ui.painter_at(rect);

            // Stack icon above label, anchored to the top edge so they never
            // extend above the tab bar's rect.
            let icon_center_y = rect.top() + 2.0 + icon_size * 0.5;
            let label_center_y = icon_center_y + icon_size * 0.5 + label_size * 0.5 + 1.0;

            painter.text(
                egui::pos2(rect.center().x, icon_center_y),
                egui::Align2::CENTER_CENTER,
                icon,
                egui::FontId::proportional(icon_size),
                text_color,
            );
            painter.text(
                egui::pos2(rect.center().x, label_center_y),
                egui::Align2::CENTER_CENTER,
                tab.label(),
                egui::FontId::proportional(label_size),
                text_color,
            );

            if is_active {
                let underline_y = rect.bottom() - indicator_gap;
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + 12.0, underline_y),
                        egui::pos2(rect.right() - 12.0, underline_y),
                    ],
                    egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
                );
            }
        }
    });
}

fn tab_icon(tab: MobileTab) -> &'static str {
    match tab {
        MobileTab::Playback => egui_phosphor::regular::PLAY_CIRCLE,
        MobileTab::Product => egui_phosphor::regular::STACK,
        MobileTab::Layers => egui_phosphor::regular::STACK_SIMPLE,
        MobileTab::More => egui_phosphor::regular::DOTS_THREE,
    }
}

// ---------------------------------------------------------------------------
// Tab content
// ---------------------------------------------------------------------------

fn render_playback_tab(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(8.0);

    // Current time label (tap-to-jump opens datetime picker).
    let selected_ts = state.playback_state.playback_position();
    let use_local = state.use_local_time;
    let tz = if use_local { "Local" } else { "UTC" };
    let time_label = format!(
        "{} {}",
        super::super::timeline::format_timestamp_full(selected_ts, use_local),
        tz
    );
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        if ui
            .add(egui::Button::new(RichText::new(&time_label).monospace().size(14.0)).frame(false))
            .clicked()
        {
            state
                .datetime_picker
                .init_from_timestamp(selected_ts, use_local);
        }
    });

    ui.add_space(8.0);

    // Primary transport: big play/pause, prev/next, live toggle.
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        let play_icon = if state.playback_state.playing {
            egui_phosphor::regular::PAUSE
        } else {
            egui_phosphor::regular::PLAY
        };
        if ui
            .add_sized(
                [64.0, 48.0],
                egui::Button::new(RichText::new(play_icon).size(22.0)),
            )
            .clicked()
        {
            toggle_play(state);
        }

        ui.add_space(8.0);

        if ui
            .add_sized(
                [48.0, 48.0],
                egui::Button::new(RichText::new(egui_phosphor::regular::SKIP_BACK).size(18.0)),
            )
            .clicked()
        {
            step_frame(state, -1);
        }

        if ui
            .add_sized(
                [48.0, 48.0],
                egui::Button::new(RichText::new(egui_phosphor::regular::SKIP_FORWARD).size(18.0)),
            )
            .clicked()
        {
            step_frame(state, 1);
        }

        ui.add_space(8.0);

        // Live toggle.
        let is_live = state.live_mode_state.is_active();
        let live_color = if is_live {
            Color32::from_rgb(220, 60, 60)
        } else {
            Color32::from_rgb(130, 130, 130)
        };
        if ui
            .add_sized(
                [48.0, 48.0],
                egui::Button::new(
                    RichText::new(egui_phosphor::regular::BROADCAST)
                        .size(18.0)
                        .color(live_color),
                ),
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
    });

    ui.add_space(14.0);

    // Speed picker — three common options; long presses fall back to the
    // full menu via the tiny "All…" button.
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(RichText::new("Speed:").size(12.0));
        let mode = state.playback_state.playback_mode();
        let common: &[PlaybackSpeed] = &[
            PlaybackSpeed::Half,
            PlaybackSpeed::Normal,
            PlaybackSpeed::Quadruple,
        ];
        for speed in common {
            let label = match mode {
                PlaybackMode::Macro => speed.macro_label(),
                PlaybackMode::Micro => speed.label(),
            };
            let is_selected = state.playback_state.speed == *speed;
            if ui
                .selectable_label(is_selected, RichText::new(label).size(13.0))
                .clicked()
            {
                state.playback_state.speed = *speed;
            }
        }
        // "All" dropdown for power users who want the full speed range.
        egui::ComboBox::from_id_salt("mobile_speed_all")
            .selected_text(RichText::new(egui_phosphor::regular::DOTS_THREE).size(13.0))
            .width(40.0)
            .show_ui(ui, |ui| {
                let speeds: &[PlaybackSpeed] = match mode {
                    PlaybackMode::Macro => PlaybackSpeed::macro_speeds(),
                    PlaybackMode::Micro => PlaybackSpeed::all(),
                };
                for s in speeds {
                    let label = match mode {
                        PlaybackMode::Macro => s.macro_label(),
                        PlaybackMode::Micro => s.label(),
                    };
                    ui.selectable_value(&mut state.playback_state.speed, *s, label);
                }
            });
    });

    ui.add_space(8.0);

    // UTC/Local toggle.
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        let label = if state.use_local_time { "Local" } else { "UTC" };
        if ui
            .selectable_label(false, RichText::new(format!("Time: {}", label)).size(12.0))
            .clicked()
        {
            state.use_local_time = !state.use_local_time;
        }
    });
}

fn toggle_play(state: &mut AppState) {
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

fn step_frame(state: &mut AppState, direction: isize) {
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

fn render_product_tab(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(6.0);
    super::super::right_panel::render_product_section(ui, state);
}

fn render_layers_tab(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(6.0);
    super::super::right_panel::render_layers_section(ui, state);
}

fn render_more_tab(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(6.0);
    super::super::right_panel::render_rendering_section(ui, state);
    ui.add_space(4.0);
    super::super::right_panel::render_tools_section(ui, state);
    ui.add_space(4.0);
    super::super::right_panel::render_events_section(ui, state);
    ui.add_space(4.0);
    super::super::right_panel::render_storage_section(ui, state);
}
