//! The phone's sheet (one-instrument §13): the desktop rail's four tabs —
//! View, Layers, Events, Settings — drawn by the same renderers, a bottom
//! sheet in portrait and a right sheet in landscape
//! ([`crate::core::phone::sheet_rect`]).
//!
//! View holds the product row and the tilt ladder at touch size, then the
//! playback controls the transport row has no room for: the playhead's time
//! field, Frames or Rays, and the speed. Settings ends with About: the
//! version and the changelog, which the desktop keeps under ⋯. The open tab is the rail's own
//! (`chrome.rail_tab`), so the sheet and the desktop panel agree. The sheet
//! is an entry on the dialog stack: Esc, its close mark and a tap outside all
//! close it through [`DialogIntent::Close`].

use super::super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::dialogs::{DialogId, DialogIntent};
use crate::core::panels::RailTab;
use crate::core::transport::{SeekGesture, SpeedControl};
use crate::core::{Intent, PlaybackMode};
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, RichText, Vec2};

/// A touch-size row: the height every sheet control is at least.
const TOUCH_ROW_H: f32 = 36.0;

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
        draw_sheet(ctx);
    }
}

fn close(state: &mut AppState) {
    state.push_command(Intent::Dialog(DialogIntent::Close(
        DialogId::MobileSettings,
    )));
}

fn draw_sheet(ctx: &mut LayoutCtx) {
    let egui_ctx = ctx.ctx;
    if super::super::modal_helper::modal_backdrop(egui_ctx, "mobile_settings_backdrop") {
        close(ctx.state);
        return;
    }

    let viewport = egui_ctx.input(|i| i.viewport_rect());
    let sheet = crate::core::phone::sheet_rect(
        viewport.width(),
        viewport.height(),
        ctx.chrome.device_layout.is_landscape_phone(),
        super::safe_area_insets(),
    );
    // The window's frame adds its margins around the content size.
    let margin = egui_ctx.global_style().spacing.window_margin;
    let inner = Vec2::new(sheet.w - margin.sum().x, sheet.h - margin.sum().y);

    egui::Window::new("Sheet")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .fixed_pos(egui::pos2(sheet.x, sheet.y))
        .fixed_size(inner)
        .order(egui::Order::Foreground)
        .frame(
            egui::Frame::window(&egui_ctx.global_style())
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::LINE)),
        )
        .show(egui_ctx, |ui| {
            header(ui, ctx.state, ctx.chrome.rail_tab);
            tab_strip(ui, ctx.chrome);
            ui.add_space(4.0);

            let body_h = ui.available_height() - 4.0;
            egui::ScrollArea::vertical()
                .id_salt("mobile_sheet_body")
                .max_height(body_h)
                .auto_shrink([false, false])
                .show(ui, |ui| match ctx.chrome.rail_tab {
                    RailTab::View => {
                        super::super::right_panel::view_tab(
                            ui,
                            ctx.state,
                            ctx.top_bar_vm,
                            ctx.ladder_vm,
                            &super::super::ladder::TOUCH,
                        );
                        playback_section(ui, ctx.state, ctx.playback);
                    }
                    RailTab::Layers => super::super::right_panel::layers_tab(
                        ui,
                        ctx.state,
                        ctx.diagnostics,
                        ctx.playback,
                        ctx.derived,
                        ctx.chrome,
                    ),
                    RailTab::Events => super::super::right_panel::render_events_section(
                        ui,
                        ctx.state,
                        ctx.sites,
                        ctx.live,
                        ctx.playback,
                        ctx.chrome,
                    ),
                    RailTab::Settings => {
                        super::super::right_panel::settings_tab(ui, ctx.state, ctx.chrome);
                        about_section(
                            ui,
                            ctx.state,
                            &ctx.top_bar_vm.version,
                            &ctx.activity_vm.readout,
                        );
                    }
                });
        });
}

fn header(ui: &mut egui::Ui, state: &mut AppState, tab: RailTab) {
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(
            RichText::new(tab.label())
                .size(theme::TITLE)
                .strong()
                .color(theme::TEXT),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mark = ui.add(
                egui::Button::new(
                    RichText::new(crate::ui::icons::X)
                        .size(20.0)
                        .color(theme::TEXT_DIM),
                )
                .frame(false)
                .min_size(Vec2::splat(TOUCH_ROW_H)),
            );
            if mark.clicked() {
                close(state);
            }
        });
    });
}

/// The rail's tabs across the sheet's top, the open one underlined in the
/// selected color.
fn tab_strip(ui: &mut egui::Ui, chrome: &mut crate::subsystem::Chrome) {
    let tab_w = ui.available_width() / RailTab::ALL.len() as f32;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for tab in RailTab::ALL {
            let active = chrome.rail_tab == tab;
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(tab_w, TOUCH_ROW_H), egui::Sense::click());
            if resp.clicked() {
                chrome.rail_tab = tab;
            }
            let painter = ui.painter_at(rect);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                tab.label(),
                egui::FontId::proportional(theme::BODY),
                if active { theme::TEXT } else { theme::TEXT_DIM },
            );
            let y = rect.bottom() - 1.0;
            let (x0, x1, stroke) = if active {
                (
                    rect.left() + 10.0,
                    rect.right() - 10.0,
                    egui::Stroke::new(2.0, theme::SELECTED),
                )
            } else {
                (
                    rect.left(),
                    rect.right(),
                    egui::Stroke::new(1.0, theme::LINE),
                )
            };
            painter.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
        }
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(title)
            .size(theme::CAPTION)
            .color(theme::TEXT_FAINT),
    );
}

/// The phone's About, the desktop ⋯ menu's About at the sheet's foot: the
/// build's version, the frame rate and request count the desktop transport
/// row shows beside the activity chip, and the changelog link.
fn about_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    version: &str,
    readout: &crate::core::activity::Readout,
) {
    section(ui, "About");
    ui.label(
        RichText::new(version)
            .size(theme::CAPTION)
            .monospace()
            .color(theme::TEXT_DIM),
    );
    ui.label(
        RichText::new(readout.text())
            .size(theme::CAPTION)
            .monospace()
            .color(theme::TEXT_FAINT),
    );
    if ui
        .button(format!("{}  Changelog", crate::ui::icons::ARROW_SQUARE_OUT))
        .clicked()
    {
        state.push_command(Intent::OpenExternalUrl(
            super::super::top_bar::CHANGELOG_URL,
        ));
    }
    ui.add_space(8.0);
}

/// What the transport row has no room for: the playhead's time field, Frames
/// or Rays, and the speed — all intents, read from the transport view-model.
fn playback_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    playback: &crate::subsystem::Playback,
) {
    let vm = crate::core::transport::transport_vm(&playback.state, state.viz_state.tilt_role());
    let now = state.frame_now.secs();

    section(ui, "Playhead");
    if let Some(ts) = super::super::time_field::time_field(
        ui,
        "mobile_playhead_clock",
        playback.state.playback_position(),
        now,
        state.use_local_time,
        &super::super::time_field::TimeFieldLook {
            color: theme::SEEN,
            size: theme::READOUT,
            hover: "The playhead. Tap to type or paste a time.",
        },
    ) {
        state.push_command(Intent::Seek {
            ts,
            gesture: SeekGesture::Typed,
        });
    }

    section(ui, "Playback");
    ui.horizontal(|ui| {
        for choice in [PlaybackMode::Macro, PlaybackMode::Micro] {
            let on = vm.mode == choice;
            let disabled = (choice == PlaybackMode::Micro)
                .then_some(vm.rays_disabled)
                .flatten();
            if ui
                .add_enabled(
                    disabled.is_none(),
                    egui::Button::selectable(on, choice.label())
                        .min_size(Vec2::new(72.0, TOUCH_ROW_H)),
                )
                .on_disabled_hover_text(disabled.unwrap_or_default())
                .clicked()
                && !on
            {
                state.push_command(Intent::SetPlayMode(choice));
            }
        }
    });

    section(ui, "Speed");
    match &vm.speed {
        SpeedControl::Locked { label } => {
            ui.label(RichText::new(label).color(theme::TEXT_DIM))
                .on_hover_text("Real time while pinned to now in Rays");
        }
        SpeedControl::Menu { choices, .. } => {
            ui.horizontal_wrapped(|ui| {
                for (speed, label) in choices {
                    let on = *speed == playback.state.speed;
                    if ui
                        .add(
                            egui::Button::selectable(on, label.as_str())
                                .min_size(Vec2::new(48.0, TOUCH_ROW_H)),
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
