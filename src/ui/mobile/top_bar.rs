//! Compact mobile top bar.
//!
//! Replaces the desktop top bar on mobile: drops the sidebar toggles, help
//! button, and view-mode switcher (all irrelevant on mobile), and trims to
//! the essentials: mode badge, site chip, alerts, worker error banner, and
//! a small version stamp in the top-right. The app-mode accent is painted
//! as a 2px colored line at the bottom edge of the bar (the border between
//! the bar and the canvas) rather than as a separate stripe at the very
//! top — the top edge sits under the iOS status bar / notch on real
//! devices, where a thin stripe is either obscured or crowded against OS
//! icons.

use super::super::layout::{Layer, LayerKind, LayoutCtx};
use crate::state::{AppMode, AppState};
use eframe::egui::{self, Align, Color32, Frame, Layout, Margin, RichText};

const TOP_BAR_CONTENT_HEIGHT: f32 = 44.0;
const ACCENT_THICKNESS: f32 = 2.0;

pub(in crate::ui) struct MobileTopBarLayer;

impl Layer for MobileTopBarLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        10
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        // Auto-hide in lockstep with the bottom chrome so the whole shell
        // reveals/hides together (spec §13). The decision is resolved once per
        // frame into `mobile_auto_hide.hidden` (see `main.rs`).
        !ctx.chrome.mobile_auto_hide.hidden
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_mobile_top_bar(
            ctx.ctx,
            ctx.state,
            ctx.live,
            ctx.diagnostics,
            ctx.derived,
            ctx.diagnostics_vm,
            ctx.activity_vm,
            ctx.chrome,
        );
    }
}

/// Compact displayed-frame time + tilt for the mobile bar. Bare 12-hour clock
/// (no seconds/zone — the bar is narrow); tap toggles local/UTC. Shows a muted
/// placeholder until a frame is on screen.
fn render_mobile_time_readout(ui: &mut egui::Ui, state: &mut AppState) {
    use super::super::time_format::{format_clock_12h, Compaction};

    ui.add_space(6.0);
    match state.viz_state.displayed_midpoint_secs() {
        Some(mid) => {
            let t = format_clock_12h(mid, state.use_local_time, Compaction::Bare);
            let label = format!("{t} · {}", state.viz_state.elevation);
            if ui
                .add(egui::Button::new(RichText::new(label).size(13.0).strong()).frame(false))
                .clicked()
            {
                state.use_local_time = !state.use_local_time;
            }
        }
        None => {
            ui.label(
                RichText::new("--:--")
                    .size(13.0)
                    .color(Color32::from_rgb(120, 120, 120)),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_mobile_top_bar(
    ctx: &egui::Context,
    state: &mut AppState,
    live: &crate::subsystem::Live,
    diagnostics: &crate::subsystem::Diagnostics,
    derived: &crate::subsystem::Derived,
    diagnostics_vm: &crate::core::diagnostics::DiagnosticsVm,
    activity_vm: &crate::core::activity::ActivityVm,
    chrome: &mut crate::subsystem::Chrome,
) {
    // iOS safe area: when installed as a home-screen PWA, the canvas extends
    // under the translucent status bar / notch. Pad the top so OS icons
    // don't overlap our content.
    let (inset_top, _inset_right, _inset_bottom, _inset_left) = super::safe_area_insets();

    let panel_fill = ctx.style().visuals.panel_fill;
    let accent_color = crate::ui::colors::mode::color(live.app_mode);

    // Zero inner-margin frame so the content sits as high as possible —
    // egui's default top-panel frame adds a couple of pixels of padding on
    // every side, which is noticeable below the iOS status bar.
    let frame = Frame::NONE.fill(panel_fill).inner_margin(Margin::ZERO);

    egui::TopBottomPanel::top("mobile_top_bar")
        .resizable(false)
        .exact_height(inset_top + TOP_BAR_CONTENT_HEIGHT)
        .frame(frame)
        .show(ctx, |ui| {
            if inset_top > 0.0 {
                ui.add_space(inset_top);
            }

            let panel_rect = ui.max_rect();

            ui.horizontal_centered(|ui| {
                ui.add_space(8.0);

                // Site chip — primary control, tapping opens the site modal.
                let site_label = format!(
                    "{} {}",
                    egui_phosphor::regular::MAP_PIN,
                    state.viz_state.site_id
                );
                if ui
                    .button(RichText::new(&site_label).size(14.0).strong())
                    .clicked()
                {
                    chrome.site_modal_open = true;
                }

                ui.separator();

                // Compact mode badge (Idle / Archive / Live).
                let mode = live.app_mode;
                let icon = match mode {
                    AppMode::Idle => egui_phosphor::regular::PAUSE_CIRCLE,
                    AppMode::Archive => egui_phosphor::regular::ARCHIVE_BOX,
                    AppMode::Live => egui_phosphor::regular::BROADCAST,
                };
                ui.label(RichText::new(icon).size(14.0).color(accent_color));

                // Primary displayed-frame time + tilt (spec §5/§11.1), compact
                // for the narrow bar. Formatted live from the frame's actual
                // collection time; tap toggles local/UTC (spec §11.4).
                render_mobile_time_readout(ui, state);

                // Alerts chip (reuses the desktop helper).
                super::super::top_bar::render_alerts_chip(
                    ui,
                    state,
                    diagnostics,
                    derived,
                    diagnostics_vm,
                    chrome,
                );

                // Worker error banner — critical, must be visible on mobile too.
                if let Some(ref error_msg) = state.worker_init_error {
                    let error_color = Color32::from_rgb(220, 60, 60);
                    ui.label(
                        RichText::new(egui_phosphor::regular::WARNING)
                            .size(14.0)
                            .color(error_color),
                    );
                    ui.label(RichText::new(error_msg).size(11.0).color(error_color));
                    if ui
                        .small_button(RichText::new("Retry").size(11.0).color(error_color))
                        .clicked()
                    {
                        state.push_command(crate::core::Intent::RetryWorker);
                    }
                }

                // Version stamp — right-aligned. Useful for cross-referencing
                // a deployed build against git history when reporting bugs.
                // Tap to toggle between truncated (fits the bar) and full
                // (shows the complete hash / version string).
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(8.0);

                    let expanded_id = egui::Id::new("mobile_top_bar_version_expanded");
                    let expanded: bool =
                        ui.ctx().data(|d| d.get_temp(expanded_id).unwrap_or(false));

                    const MAX_LEN: usize = 18;
                    let version = env!("NEXRAD_VERSION");
                    let display = if !expanded && version.len() > MAX_LEN {
                        let mut truncated = String::with_capacity(MAX_LEN + 3);
                        for (i, ch) in version.char_indices() {
                            if i >= MAX_LEN {
                                break;
                            }
                            truncated.push(ch);
                        }
                        truncated.push('\u{2026}');
                        truncated
                    } else {
                        version.to_string()
                    };

                    let response = ui.add(
                        egui::Button::new(
                            RichText::new(display)
                                .size(10.0)
                                .color(Color32::from_rgb(120, 120, 120)),
                        )
                        .frame(false),
                    );
                    if response.clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(expanded_id, !expanded));
                    }

                    // Acquiring indicator (spec §5 status chip, mobile home):
                    // a compact "↓ N" / failure tick that opens the queue
                    // sheet. Hidden when idle. Left of the version stamp.
                    ui.add_space(8.0);
                    super::super::activity_chip::render_activity_chip(ui, activity_vm, state, 12.0);
                });
            });

            // Paint the app-mode accent as a thin border along the bottom
            // edge of the panel, separating it from the canvas below.
            let y = panel_rect.bottom() - ACCENT_THICKNESS * 0.5;
            ui.painter().line_segment(
                [
                    egui::pos2(panel_rect.left(), y),
                    egui::pos2(panel_rect.right(), y),
                ],
                egui::Stroke::new(ACCENT_THICKNESS, accent_color),
            );
        });
}
