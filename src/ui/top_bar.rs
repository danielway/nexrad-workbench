//! Top bar UI: app title, status, and site context.

use super::layout::{Layer, LayerKind, LayoutCtx};
use super::overflow_menu::overflow_menu;
use crate::alerts::{event_color, AlertSeverity};
use crate::state::{AppCommand, AppMode, AppState, CameraMode, ErrorContext, ViewMode, WidthTier};
use eframe::egui::{self, Color32, Frame, RichText};

pub(super) struct TopBarLayer;

impl Layer for TopBarLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        10
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_top_bar(
            ctx.ctx,
            ctx.state,
            ctx.live,
            ctx.playback,
            ctx.diagnostics,
            ctx.derived,
            ctx.chrome,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_top_bar(
    ctx: &egui::Context,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    diagnostics: &mut crate::subsystem::Diagnostics,
    derived: &crate::subsystem::Derived,
    chrome: &mut crate::subsystem::Chrome,
) {
    // Detect status message changes: if the message content differs from when we
    // last recorded the timestamp, update the timestamp now. This works even when
    // callers assign directly to `status_message` without using `set_status()`.
    let status_id = egui::Id::new("__last_status_msg");
    let prev_msg: Option<String> = ctx.data(|d| d.get_temp(status_id));
    if prev_msg.as_deref() != Some(&state.status_message) {
        state.status_message_set_ms = state.frame_now.millis();
        ctx.data_mut(|d| d.insert_temp(status_id, state.status_message.clone()));
    }

    // Thin mode-colored accent bar along the very top edge of the window.
    egui::TopBottomPanel::top("mode_accent")
        .resizable(false)
        .exact_height(3.0)
        .frame(Frame::NONE.fill(live.app_mode.color()))
        .show(ctx, |ui| {
            ui.allocate_space(ui.available_size());
        });

    egui::TopBottomPanel::top("top_bar")
        .exact_height(36.0)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                // Left panel toggle — hidden in Basic since the left panel
                // (radar diagnostics) is itself force-hidden in Basic.
                if state.show_advanced()
                    && ui
                        .button(RichText::new(egui_phosphor::regular::SIDEBAR_SIMPLE).size(14.0))
                        .on_hover_text("Toggle left panel")
                        .clicked()
                {
                    chrome.left_sidebar_visible = !chrome.left_sidebar_visible;
                }

                // App title — non-interactive, so it's the first thing to go:
                // shown only at full width, dropped once space tightens.
                if state.width_tier >= WidthTier::Full {
                    ui.label(
                        RichText::new("NEXRAD Workbench")
                            .strong()
                            .size(16.0)
                            .color(ui.visuals().strong_text_color()),
                    );
                    ui.separator();
                }

                // Site context button — opens site selection modal
                let site_label = format!("Site: {}", state.viz_state.site_id);
                if ui
                    .button(RichText::new(&site_label).size(14.0).strong())
                    .on_hover_text("Click to change radar site")
                    .clicked()
                {
                    chrome.site_modal_open = true;
                }

                ui.separator();

                // NWS alerts chip — shown only in 2D when one or more alerts
                // intersect the visible map bounds.
                render_alerts_chip(ui, state, diagnostics, derived, chrome);

                // Recent-errors chip — surfaces failures from the unified
                // ErrorContext aggregator. Quiet when no errors have been
                // pushed; a click pops a small log with the latest entries.
                render_errors_chip(ui, &mut state.errors, state.frame_now.millis());

                // Persistent worker initialization error banner
                if let Some(ref error_msg) = state.worker_init_error {
                    let error_color = Color32::from_rgb(220, 60, 60);
                    ui.label(
                        RichText::new(egui_phosphor::regular::WARNING)
                            .size(14.0)
                            .color(error_color),
                    );
                    ui.label(
                        RichText::new(error_msg.as_str())
                            .size(13.0)
                            .color(error_color),
                    );
                    if ui
                        .button(RichText::new("Retry").size(12.0).color(error_color))
                        .on_hover_text("Retry worker initialization")
                        .clicked()
                    {
                        state.push_command(crate::state::AppCommand::RetryWorker);
                    }
                }

                render_mode_badge(ui, live, playback);

                // Status message (Idle/Archive only — Live has its own trailing
                // text with chunk counts/countdown). Suppressed below the full
                // tier: it's transient and would crowd the narrow bar.
                if state.width_tier >= WidthTier::Full
                    && live.app_mode != AppMode::Live
                    && !state.status_message.is_empty()
                {
                    // Auto-dismiss: fade out after 8 seconds, clear after 10
                    let now = state.frame_now.millis();
                    let age_ms = now - state.status_message_set_ms;
                    const FADE_START_MS: f64 = 8000.0;
                    const DISMISS_MS: f64 = 10000.0;

                    if state.status_message_set_ms > 0.0 && age_ms >= DISMISS_MS {
                        state.status_message.clear();
                    } else {
                        let alpha = if state.status_message_set_ms <= 0.0 || age_ms < FADE_START_MS
                        {
                            255u8
                        } else {
                            let t = 1.0 - (age_ms - FADE_START_MS) / (DISMISS_MS - FADE_START_MS);
                            (t.clamp(0.0, 1.0) * 255.0) as u8
                        };

                        ui.label(
                            RichText::new(&state.status_message)
                                .size(13.0)
                                .color(Color32::from_rgba_unmultiplied(128, 128, 128, alpha)),
                        );

                        // Request repaint during fade
                        if (FADE_START_MS..DISMISS_MS).contains(&age_ms) {
                            ui.ctx().request_repaint();
                        }
                    }
                }

                // Right-aligned cluster: right panel toggle, view/camera mode,
                // and a ⋯ overflow menu that absorbs lower-priority chrome when
                // it doesn't fit. The fit decision is driven by the *measured*
                // width remaining after the (variable-width) left content, so
                // items collapse exactly when there's no room — no fixed-width
                // band where the cluster overlaps the left content.
                let advanced = state.show_advanced();
                let fit = decide_right_cluster(ui, advanced, ui.available_width());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Right panel toggle — always the rightmost item.
                    if ui
                        .button(RichText::new(egui_phosphor::regular::SIDEBAR_SIMPLE).size(14.0))
                        .on_hover_text("Toggle right panel")
                        .clicked()
                    {
                        chrome.right_sidebar_visible = !chrome.right_sidebar_visible;
                    }

                    // Overflow menu — holds whatever didn't fit. Placed just
                    // left of the panel toggle so it's always reachable.
                    if fit.any_overflow() {
                        overflow_menu(ui, |ui| {
                            if !fit.views {
                                ui.label(RichText::new("View").size(11.0).weak());
                                ui.horizontal(|ui| render_view_mode_pills(ui, state));
                            }
                            if !fit.pill {
                                ui.label(RichText::new("Mode").size(11.0).weak());
                                ui.horizontal(|ui| render_ui_mode_pill(ui, state));
                            }
                            if !fit.help
                                && ui
                                    .button(format!(
                                        "{}  Keyboard shortcuts",
                                        egui_phosphor::regular::QUESTION
                                    ))
                                    .clicked()
                            {
                                chrome.shortcuts_help_visible = !chrome.shortcuts_help_visible;
                            }
                            if !fit.version {
                                render_version_link(ui);
                            }
                        });
                    }

                    // Inline survivors — right-to-left order matching the
                    // original full-width layout (help, mode pill, version,
                    // separator, view pills).
                    if fit.help
                        && ui
                            .button(RichText::new(egui_phosphor::regular::QUESTION).size(14.0))
                            .on_hover_text("Keyboard shortcuts (?)")
                            .clicked()
                    {
                        chrome.shortcuts_help_visible = !chrome.shortcuts_help_visible;
                    }

                    if fit.pill {
                        // Basic / Advanced pill — toggles UI complexity. Same
                        // segmented-pill idiom as the view-mode selector.
                        render_ui_mode_pill(ui, state);
                    }

                    if fit.version {
                        render_version_link(ui);
                    }

                    if fit.views {
                        ui.separator();
                        render_view_mode_pills(ui, state);
                    }
                });
            });
        });
}

/// Two-segment Basic/Advanced pill matching the view-mode selector idiom.
/// Active segment is bold + colored; inactive is dim. Click flips the
/// preference (also bound to Ctrl+Shift+A in `shortcuts.rs`).
fn render_ui_mode_pill(ui: &mut egui::Ui, state: &mut AppState) {
    let active_color = Color32::from_rgb(100, 180, 255);
    let dim = Color32::from_rgb(100, 100, 100);

    // Right-to-left layout: render the rightmost segment first (Advanced).
    for &(label, advanced_value) in &[("Advanced", true), ("Basic", false)] {
        let is_active = state.show_advanced() == advanced_value;
        let text = if is_active {
            RichText::new(label).size(13.0).strong().color(active_color)
        } else {
            RichText::new(label).size(13.0).color(dim)
        };
        let resp = ui.add(egui::Button::new(text).frame(is_active));
        if !is_active && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp
            .on_hover_text(format!(
                "Switch to {} controls (Ctrl+Shift+A)",
                label.to_lowercase()
            ))
            .clicked()
        {
            state.advanced_mode = advanced_value;
        }
    }
}

/// View-mode selector pills. In Basic, a single 2D / 3D toggle (3D picks
/// SiteOrbit as the sensible default for radar viewing). In Advanced, all
/// three 3D camera modes are exposed as separate pills. Rendered both inline
/// in the top bar and, when space is tight, inside the overflow menu.
fn render_view_mode_pills(ui: &mut egui::Ui, state: &mut AppState) {
    let dim = Color32::from_rgb(100, 100, 100);

    let modes: &[(&str, ViewMode, Option<CameraMode>, Color32, &str)] = if state.show_advanced() {
        &[
            (
                "2D",
                ViewMode::Flat2D,
                None,
                Color32::from_rgb(100, 180, 255),
                "1",
            ),
            (
                "3D Site",
                ViewMode::Globe3D,
                Some(CameraMode::SiteOrbit),
                Color32::from_rgb(255, 200, 80),
                "2",
            ),
            (
                "3D Planet",
                ViewMode::Globe3D,
                Some(CameraMode::PlanetOrbit),
                Color32::from_rgb(120, 200, 120),
                "3",
            ),
            (
                "3D Free",
                ViewMode::Globe3D,
                Some(CameraMode::FreeLook),
                Color32::from_rgb(200, 140, 255),
                "4",
            ),
        ]
    } else {
        &[
            (
                "2D",
                ViewMode::Flat2D,
                None,
                Color32::from_rgb(100, 180, 255),
                "1",
            ),
            (
                "3D",
                ViewMode::Globe3D,
                Some(CameraMode::SiteOrbit),
                Color32::from_rgb(255, 200, 80),
                "2",
            ),
        ]
    };

    for &(label, view, cam, color, key) in modes {
        let is_active = match (view, cam) {
            (ViewMode::Flat2D, _) => state.viz_state.view_mode == ViewMode::Flat2D,
            (ViewMode::Globe3D, Some(cm)) => {
                if state.show_advanced() {
                    state.viz_state.view_mode == ViewMode::Globe3D
                        && state.viz_state.camera.mode == cm
                } else {
                    // In Basic the single 3D pill is active for any 3D camera mode.
                    state.viz_state.view_mode == ViewMode::Globe3D
                }
            }
            _ => false,
        };

        let text = if is_active {
            RichText::new(label).size(13.0).strong().color(color)
        } else {
            RichText::new(label).size(13.0).color(dim)
        };

        let response = ui.add(egui::Button::new(text).frame(is_active));

        if !is_active && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        if response
            .on_hover_text(format!("Switch to {} ({})", label, key))
            .clicked()
        {
            state.viz_state.view_mode = view;
            if let Some(cm) = cam {
                state.viz_state.camera.switch_mode(cm);
            }
        }
    }
}

/// The (possibly truncated) version string shown by [`render_version_link`].
/// Factored out so the right-cluster fit logic can measure it.
fn version_display_text() -> String {
    const MAX_LEN: usize = 24;
    let version = env!("NEXRAD_VERSION");
    if version.len() > MAX_LEN {
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
    }
}

/// Which demotable items in the top bar's right cluster fit inline this frame.
/// Anything `false` is rendered inside the `⋯` overflow menu instead.
#[derive(Clone, Copy)]
struct RightClusterFit {
    help: bool,
    pill: bool,
    version: bool,
    views: bool,
}

impl RightClusterFit {
    fn any_overflow(&self) -> bool {
        !(self.help && self.pill && self.version && self.views)
    }
}

/// Width a text/icon button occupies for `text` at `size`. Frame on or off
/// doesn't change the footprint — the horizontal padding is the same — so this
/// works for both the framed icon buttons and the frameless version link.
fn button_width(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    let font = egui::FontId::proportional(size);
    let galley_w = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x;
    galley_w + 2.0 * ui.spacing().button_padding.x
}

/// Decide which right-cluster items fit inline given the measured width
/// remaining after the left content. Demotes lowest-priority items first
/// (version → help → view pills → Basic/Advanced pill) until the cluster fits,
/// accounting for the `⋯` button once anything is demoted. Slightly
/// conservative (a small margin plus rounded-up estimates) so the bias is
/// toward collapsing one frame early rather than ever overlapping.
fn decide_right_cluster(ui: &egui::Ui, advanced: bool, avail: f32) -> RightClusterFit {
    let sp = ui.spacing().item_spacing.x;
    let icon = |g: &str| button_width(ui, g, 14.0);
    let w_toggle = icon(egui_phosphor::regular::SIDEBAR_SIMPLE);
    let w_menu = icon(egui_phosphor::regular::DOTS_THREE);
    let w_help = icon(egui_phosphor::regular::QUESTION);
    let w_pill = button_width(ui, "Basic", 13.0) + sp + button_width(ui, "Advanced", 13.0);
    let w_version = button_width(ui, &version_display_text(), 11.0);

    let view_labels: &[&str] = if advanced {
        &["2D", "3D Site", "3D Planet", "3D Free"]
    } else {
        &["2D", "3D"]
    };
    // A leading separator (a Separator widget plus its spacing) precedes the pills.
    let mut w_views = 6.0 + sp;
    for (i, label) in view_labels.iter().enumerate() {
        if i > 0 {
            w_views += sp;
        }
        w_views += button_width(ui, label, 13.0);
    }

    let mut fit = RightClusterFit {
        help: true,
        pill: true,
        version: true,
        views: true,
    };
    const MARGIN: f32 = 6.0;
    loop {
        let mut used = w_toggle;
        if fit.any_overflow() {
            used += sp + w_menu;
        }
        if fit.help {
            used += sp + w_help;
        }
        if fit.pill {
            used += sp + w_pill;
        }
        if fit.version {
            used += sp + w_version;
        }
        if fit.views {
            used += sp + w_views;
        }
        if used <= avail - MARGIN {
            break;
        }
        // Demote the lowest-priority item still inline.
        if fit.version {
            fit.version = false;
        } else if fit.help {
            fit.help = false;
        } else if fit.views {
            fit.views = false;
        } else if fit.pill {
            fit.pill = false;
        } else {
            break; // nothing left to demote
        }
    }
    fit
}

/// Version stamp — a frameless, clickable link that opens the GitHub releases
/// page. Rendered inline in the top bar and, when space is tight, inside the
/// overflow menu.
fn render_version_link(ui: &mut egui::Ui) {
    let full_version = env!("NEXRAD_VERSION_FULL");
    let display = version_display_text();

    let response = ui.add(
        egui::Button::new(
            RichText::new(&display)
                .size(11.0)
                .color(Color32::from_rgb(80, 80, 80)),
        )
        .frame(false),
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let clicked = response.clicked();

    response.on_hover_text(format!("{} — click to view changelog", full_version));

    if clicked {
        if let Some(window) = web_sys::window() {
            let _ = window.open_with_url_and_target(
                "https://github.com/danielway/nexrad-workbench/releases",
                "_blank",
            );
        }
    }
}

/// Render a compact alerts indicator for the top bar. Shows nothing when
/// no active NWS alerts intersect the current viewing area (or when the
/// viewing area is undefined, e.g. in 3D globe mode).
pub(super) fn render_alerts_chip(
    ui: &mut egui::Ui,
    state: &mut AppState,
    diagnostics: &mut crate::subsystem::Diagnostics,
    derived: &crate::subsystem::Derived,
    _chrome: &mut crate::subsystem::Chrome,
) {
    // Show a subtle loading/error hint on the first fetch so the user knows
    // the feed is being contacted. After the first success, stay quiet unless
    // there are alerts to surface.
    let has_ever_loaded = diagnostics.alerts.last_success_ms > 0.0;
    let has_error = diagnostics.alerts.last_error.is_some();
    if !has_ever_loaded && !has_error {
        let icon = RichText::new(egui_phosphor::regular::BELL_SIMPLE)
            .size(14.0)
            .color(Color32::from_rgb(130, 130, 130));
        ui.add(egui::Label::new(icon))
            .on_hover_text("Loading NWS alerts\u{2026}");
        ui.separator();
        return;
    }

    let Some(bounds) = derived.visible_bounds else {
        // 3D globe view or canvas hasn't rendered yet.
        return;
    };

    let visible: Vec<(String, String, AlertSeverity, bool)> = diagnostics
        .alerts
        .visible_in(bounds)
        .into_iter()
        .map(|a| (a.id.clone(), a.event.clone(), a.severity, a.is_warning()))
        .collect();

    if visible.is_empty() {
        // Render a quiet dimmed icon so users know the feed is live when hovered.
        let tooltip = if has_error {
            format!(
                "NWS alerts: {}",
                diagnostics.alerts.last_error.as_deref().unwrap_or("error")
            )
        } else {
            format!(
                "No active alerts in view ({} active nationwide)",
                diagnostics.alerts.alerts.len()
            )
        };
        let color = if has_error {
            Color32::from_rgb(200, 120, 60)
        } else {
            Color32::from_rgb(110, 110, 110)
        };
        let icon = RichText::new(egui_phosphor::regular::BELL_SIMPLE)
            .size(14.0)
            .color(color);
        let response = ui.add(egui::Label::new(icon).sense(egui::Sense::click()));
        response.clone().on_hover_text(tooltip);
        if response.clicked() {
            state.push_command(AppCommand::RefreshAlerts);
        }
        ui.separator();
        return;
    }

    // Color the chip after the highest-severity alert's event type (the list is
    // already sorted by severity descending by `visible_in`).
    let (r, g, b) = event_color(&visible[0].1);
    let chip_color = Color32::from_rgb(r, g, b);

    let label = if visible.len() == 1 {
        let event = &visible[0].1;
        format!("{} {}", egui_phosphor::regular::WARNING, event)
    } else {
        // Split the count into warnings vs everything else (watches/advisories),
        // omitting a side when it's zero.
        let warnings = visible.iter().filter(|a| a.3).count();
        let watches = visible.len() - warnings;
        let mut parts = Vec::new();
        if warnings > 0 {
            parts.push(format!(
                "{} warning{}",
                warnings,
                if warnings == 1 { "" } else { "s" }
            ));
        }
        if watches > 0 {
            parts.push(format!(
                "{} watch{}",
                watches,
                if watches == 1 { "" } else { "es" }
            ));
        }
        format!("{} {}", egui_phosphor::regular::WARNING, parts.join(" · "))
    };

    let response = ui.add(egui::Button::new(
        RichText::new(label).size(13.0).strong().color(chip_color),
    ));

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let hover = if visible.len() == 1 {
        format!("{} — click for details", visible[0].1,)
    } else {
        let mut lines = String::from("Click to view alerts in this area:\n");
        for (_, event, sev, _) in visible.iter().take(6) {
            lines.push_str(&format!("\n  \u{2022} [{}] {}", sev.label(), event));
        }
        if visible.len() > 6 {
            lines.push_str(&format!("\n  \u{2026} and {} more", visible.len() - 6));
        }
        lines
    };

    if response.on_hover_text(hover).clicked() {
        if visible.len() == 1 {
            state.push_command(AppCommand::OpenAlert(visible[0].0.clone()));
        } else {
            diagnostics.alerts.list_modal_open = true;
        }
    }

    ui.separator();
}

/// Render a compact errors-log indicator for the top bar.
///
/// Renders nothing when no errors have been recorded — this is the
/// quiet, non-intrusive baseline. As soon as any reporter pushes into
/// [`ErrorContext`], a warning chip with a count appears; clicking it
/// pops a small log showing the most recent entries (newest first) and
/// a Clear button to dismiss the ring.
pub(super) fn render_errors_chip(ui: &mut egui::Ui, errors: &mut ErrorContext, now_ms: f64) {
    if errors.is_empty() {
        return;
    }

    let count = errors.len();
    let label = format!("{} {}", egui_phosphor::regular::WARNING, count);
    let chip_color = Color32::from_rgb(220, 120, 60);

    let response = ui.add(egui::Button::new(
        RichText::new(label).size(13.0).strong().color(chip_color),
    ));

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let hover = if count == 1 {
        "1 recent error — click for details".to_string()
    } else {
        format!("{} recent errors — click for details", count)
    };

    let response = response.on_hover_text(hover);

    egui::Popup::menu(&response).show(|ui| {
        ui.set_min_width(320.0);
        ui.set_max_width(480.0);
        ui.label(RichText::new(format!("Recent errors ({})", count)).strong());
        ui.separator();

        // Show newest first, capped at 10 entries to keep the popup
        // compact. The ring buffer retains 50 total — opening a full
        // log view is left to a future Modal if appetite arises.
        let entries: Vec<_> = errors.iter().rev().take(10).cloned().collect();
        for entry in &entries {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format_age(now_ms - entry.timestamp_ms))
                        .size(11.0)
                        .color(Color32::from_rgb(130, 130, 130)),
                );
                ui.label(
                    RichText::new(entry.error.source_label())
                        .size(11.0)
                        .color(Color32::from_rgb(180, 140, 80))
                        .strong(),
                );
                ui.label(
                    RichText::new(entry.error.message())
                        .size(12.0)
                        .color(Color32::from_rgb(220, 220, 220)),
                );
            });
        }

        ui.separator();
        if ui
            .button(format!("{} Clear", egui_phosphor::regular::TRASH))
            .clicked()
        {
            errors.clear();
            ui.close();
        }
    });

    ui.separator();
}

/// Compact relative-age string for the errors popup. Always returns a
/// 4–6 char string so columns stay aligned.
fn format_age(age_ms: f64) -> String {
    let secs = (age_ms.max(0.0) / 1000.0) as u64;
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Render the unified mode badge (Idle / Archive / Live) in the top bar.
/// Drawn as a colored pill (~20% alpha fill + colored border) so the
/// active mode is glanceable. Clicking the pill opens a small action
/// menu (Go Live / Stop streaming) — the canonical way to enter or
/// leave Live. The Live pulse animation and streaming detail text are
/// preserved.
pub(super) fn render_mode_badge(
    ui: &mut egui::Ui,
    live: &mut crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
) {
    let mode = live.app_mode;
    let color = mode.color();

    let icon_str = match mode {
        AppMode::Idle => egui_phosphor::regular::PAUSE_CIRCLE,
        AppMode::Archive => egui_phosphor::regular::ARCHIVE_BOX,
        AppMode::Live => egui_phosphor::regular::BROADCAST,
    };

    // For Live, pulse the icon's alpha channel; other modes render solid.
    let icon_color = if mode == AppMode::Live {
        let pulse = live.mode_state.pulse_alpha();
        Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            (128.0 + 127.0 * pulse) as u8,
        )
    } else {
        color
    };

    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40);
    let stroke = egui::Stroke::new(1.0, color);
    let inner = egui::Frame::default()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new(icon_str).size(15.0).color(icon_color));
                ui.label(RichText::new(mode.label()).size(13.0).strong().color(color));
            });
        });
    // Indicator-only: the badge reports the current mode. Entering/leaving Live
    // happens through the timeline (click the now-line, press play at the live
    // edge, or the Go-live button) — not through this pill.
    let hover_text = match mode {
        AppMode::Live => "Streaming live",
        AppMode::Archive => "Browsing archive — go to now to stream",
        AppMode::Idle => "No data loaded",
    };
    inner.response.on_hover_text(hover_text);

    // Live-only trailing detail: chunk count, countdown, or elapsed acquire time.
    if mode == AppMode::Live {
        use crate::state::LivePhase;
        let now = playback.state.playback_position();
        let phase = live.mode_state.phase;
        let detail = match phase {
            LivePhase::AcquiringLock => {
                let elapsed = live.mode_state.phase_elapsed_secs(now) as i32;
                format!("acquiring lock... {}s", elapsed)
            }
            LivePhase::Streaming => {
                format!("({} chunks) receiving...", live.mode_state.chunks_received)
            }
            LivePhase::WaitingForChunk => {
                if let Some(remaining) = live.countdown_remaining_secs(now) {
                    format!(
                        "({} chunks) next in {}s",
                        live.mode_state.chunks_received,
                        remaining.ceil() as i32
                    )
                } else {
                    format!("({} chunks)", live.mode_state.chunks_received)
                }
            }
            _ => String::new(),
        };
        if !detail.is_empty() {
            ui.label(
                RichText::new(detail)
                    .size(12.0)
                    .color(Color32::from_rgb(180, 180, 180)),
            );
        }
    }
}
