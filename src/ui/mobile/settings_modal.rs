//! Mobile settings modal — the full-page view opened by the ellipsis in the
//! mobile action bar.
//!
//! Hosts a top tab strip (Playback / Product / Layers / More) and the
//! matching content body. Reuses the existing right-panel section renderers
//! so the mobile surface stays in sync with desktop automatically.

use super::super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::{PlaybackMode, PlaybackSpeed};
use crate::state::{AppState, MobileSettingsTab};
use eframe::egui::{self, Color32, RichText, Vec2};

pub(in crate::ui) struct MobileSettingsModalLayer;

impl Layer for MobileSettingsModalLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        100
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.mobile_settings_open && ctx.state.is_mobile
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_mobile_settings_modal(
            ctx.ctx,
            ctx.state,
            ctx.timeline,
            ctx.live,
            ctx.playback,
            ctx.diagnostics,
            ctx.derived,
            ctx.chrome,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_mobile_settings_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    diagnostics: &mut crate::subsystem::Diagnostics,
    derived: &crate::subsystem::Derived,
    chrome: &mut crate::subsystem::Chrome,
) {
    if super::super::modal_helper::modal_backdrop(ctx, "mobile_settings_backdrop", 160) {
        chrome.mobile_settings_open = false;
        return;
    }

    let viewport = ctx.input(|i| i.viewport_rect());
    // The modal takes most of the viewport — leaves a small gutter on each
    // side so the backdrop edge is still tappable as a dismiss affordance.
    let modal_w = (viewport.width() - 16.0).max(240.0);
    let modal_h = (viewport.height() * 0.78).clamp(360.0, viewport.height() - 40.0);

    egui::Window::new("Settings")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(modal_w, modal_h))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            render_header(ui, state, chrome);
            ui.separator();
            render_tab_strip(ui, state, chrome);
            ui.separator();
            ui.add_space(4.0);

            let body_h = ui.available_height() - 4.0;
            egui::ScrollArea::vertical()
                .id_salt("mobile_settings_body_scroll")
                .max_height(body_h)
                .auto_shrink([false, false])
                .show(ui, |ui| match chrome.mobile_settings_tab {
                    MobileSettingsTab::Playback => render_playback_body(ui, state, playback),
                    MobileSettingsTab::Product => {
                        render_product_body(ui, state, timeline, live, playback)
                    }
                    MobileSettingsTab::Layers => {
                        render_layers_body(ui, state, playback, diagnostics, derived, chrome)
                    }
                    MobileSettingsTab::More => render_more_body(ui, state, playback, chrome),
                });

            // The datetime picker popup (opened by tapping the current-time
            // label in the Playback tab) renders as an Area, so it must be
            // spawned from within the window.
            super::super::playback_controls::render_datetime_picker_popup(
                ui, state, live, playback,
            );
        });
}

fn render_header(ui: &mut egui::Ui, _state: &mut AppState, chrome: &mut crate::subsystem::Chrome) {
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.heading(chrome.mobile_settings_tab.label());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let close = ui.add(
                egui::Button::new(RichText::new(egui_phosphor::regular::X).size(20.0)).frame(false),
            );
            if close.clicked() {
                chrome.mobile_settings_open = false;
            }
        });
    });
}

fn render_tab_strip(
    ui: &mut egui::Ui,
    _state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
) {
    let total_w = ui.available_width();
    let tab_count = MobileSettingsTab::all().len() as f32;
    let tab_w = total_w / tab_count;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for tab in MobileSettingsTab::all() {
            let is_active = chrome.mobile_settings_tab == tab;
            let text_color = if is_active {
                ui.visuals().strong_text_color()
            } else {
                Color32::from_rgb(130, 130, 130)
            };
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(tab_w, 36.0), egui::Sense::click());
            if resp.clicked() {
                chrome.mobile_settings_tab = tab;
            }
            let painter = ui.painter_at(rect);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tab.label(),
                egui::FontId::proportional(14.0),
                text_color,
            );
            if is_active {
                let underline_y = rect.bottom() - 2.0;
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + 10.0, underline_y),
                        egui::pos2(rect.right() - 10.0, underline_y),
                    ],
                    egui::Stroke::new(2.0_f32, ui.visuals().strong_text_color()),
                );
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tab bodies
// ---------------------------------------------------------------------------

fn render_playback_body(
    ui: &mut egui::Ui,
    state: &mut AppState,
    playback: &mut crate::subsystem::Playback,
) {
    ui.add_space(6.0);

    // Current time — tap to open the datetime picker.
    let selected_ts = playback.state.playback_position();
    let use_local = state.use_local_time;
    let tz = if use_local { "Local" } else { "UTC" };
    let time_label = format!(
        "{} {}",
        super::super::timeline::format_timestamp_full(selected_ts, use_local),
        tz
    );
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        if ui
            .add(egui::Button::new(RichText::new(&time_label).monospace().size(15.0)).frame(false))
            .clicked()
        {
            state
                .datetime_picker
                .init_from_timestamp(selected_ts, use_local);
        }
    });

    ui.add_space(10.0);

    // No play/pause or frame-step row here: the always-visible condensed
    // transport row (mobile/tabs.rs) owns those (spec §13). This tab is the
    // Level-2 power surface — it only adds the controls the transport row lacks
    // (datetime jump above, the full speed list below, and the UTC/Local
    // toggle).

    // Speed picker (the full list — the transport row only tap-cycles common
    // speeds).
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new("Speed:").size(13.0));
        let mode = playback.state.effective_playback_mode();
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
            let is_selected = playback.state.speed == *speed;
            if ui
                .selectable_label(is_selected, RichText::new(label).size(13.0))
                .clicked()
            {
                playback.state.speed = *speed;
            }
        }
        egui::ComboBox::from_id_salt("mobile_settings_speed_all")
            .selected_text(RichText::new(egui_phosphor::regular::DOTS_THREE).size(13.0))
            .width(44.0)
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
                    ui.selectable_value(&mut playback.state.speed, *s, label);
                }
            });
    });

    ui.add_space(8.0);

    // No loop-preset chips here either: the transport row's loop-preset menu
    // (mobile/tabs.rs) owns loop creation on mobile (spec §13/§15 cut #2).

    // UTC/Local toggle.
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let label = if state.use_local_time { "Local" } else { "UTC" };
        if ui
            .selectable_label(false, RichText::new(format!("Time: {}", label)).size(13.0))
            .clicked()
        {
            state.use_local_time = !state.use_local_time;
        }
    });
}

fn render_product_body(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
) {
    ui.add_space(4.0);
    super::super::right_panel::render_product_section(ui, state, timeline, live, playback);
}

#[allow(clippy::too_many_arguments)]
fn render_layers_body(
    ui: &mut egui::Ui,
    state: &mut AppState,
    playback: &crate::subsystem::Playback,
    diagnostics: &mut crate::subsystem::Diagnostics,
    derived: &crate::subsystem::Derived,
    chrome: &mut crate::subsystem::Chrome,
) {
    ui.add_space(4.0);
    super::super::right_panel::render_layers_section(
        ui,
        state,
        diagnostics,
        playback,
        derived,
        chrome,
    );
}

fn render_more_body(
    ui: &mut egui::Ui,
    state: &mut AppState,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    ui.add_space(4.0);
    super::super::right_panel::render_rendering_section(ui, state, playback, chrome);
    ui.add_space(4.0);
    super::super::right_panel::render_tools_section(ui, state, chrome);
    ui.add_space(4.0);
    super::super::right_panel::render_events_section(ui, state, playback, chrome);
    ui.add_space(4.0);
    super::super::right_panel::render_storage_section(ui, state, chrome);
}
