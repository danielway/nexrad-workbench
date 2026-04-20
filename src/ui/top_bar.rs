//! Top bar UI: app title, status, and site context.

use crate::alerts::AlertSeverity;
use crate::state::{AppCommand, AppMode, AppState, CameraMode, ViewMode};
use eframe::egui::{self, Color32, Frame, RichText};

pub fn render_top_bar(ctx: &egui::Context, state: &mut AppState) {
    // Detect status message changes: if the message content differs from when we
    // last recorded the timestamp, update the timestamp now. This works even when
    // callers assign directly to `status_message` without using `set_status()`.
    let status_id = egui::Id::new("__last_status_msg");
    let prev_msg: Option<String> = ctx.data(|d| d.get_temp(status_id));
    if prev_msg.as_deref() != Some(&state.status_message) {
        state.status_message_set_ms = js_sys::Date::now();
        ctx.data_mut(|d| d.insert_temp(status_id, state.status_message.clone()));
    }

    // Thin mode-colored accent bar along the very top edge of the window.
    egui::TopBottomPanel::top("mode_accent")
        .resizable(false)
        .exact_height(3.0)
        .frame(Frame::NONE.fill(state.app_mode.color()))
        .show(ctx, |ui| {
            ui.allocate_space(ui.available_size());
        });

    egui::TopBottomPanel::top("top_bar")
        .exact_height(36.0)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                // Left panel toggle
                if ui
                    .button(RichText::new(egui_phosphor::regular::SIDEBAR_SIMPLE).size(14.0))
                    .on_hover_text("Toggle left panel")
                    .clicked()
                {
                    state.left_sidebar_visible = !state.left_sidebar_visible;
                }

                // App title
                ui.label(
                    RichText::new("NEXRAD Workbench")
                        .strong()
                        .size(16.0)
                        .color(ui.visuals().strong_text_color()),
                );

                ui.separator();

                // Site context button — opens site selection modal
                let site_label = format!("Site: {}", state.viz_state.site_id);
                if ui
                    .button(RichText::new(&site_label).size(14.0).strong())
                    .on_hover_text("Click to change radar site")
                    .clicked()
                {
                    state.site_modal_open = true;
                }

                ui.separator();

                // NWS alerts chip — shown only in 2D when one or more alerts
                // intersect the visible map bounds.
                render_alerts_chip(ui, state);

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

                render_mode_badge(ui, state);

                // Status message (Idle/Archive only — Live has its own trailing
                // text with chunk counts/countdown).
                if state.app_mode != AppMode::Live && !state.status_message.is_empty() {
                    // Auto-dismiss: fade out after 8 seconds, clear after 10
                    let now = js_sys::Date::now();
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

                // Right-aligned: right panel toggle + help + view/camera mode
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new(egui_phosphor::regular::SIDEBAR_SIMPLE).size(14.0))
                        .on_hover_text("Toggle right panel")
                        .clicked()
                    {
                        state.right_sidebar_visible = !state.right_sidebar_visible;
                    }

                    // Help button — toggles keyboard shortcut overlay
                    if ui
                        .button(RichText::new(egui_phosphor::regular::QUESTION).size(14.0))
                        .on_hover_text("Keyboard shortcuts (?)")
                        .clicked()
                    {
                        state.shortcuts_help_visible = !state.shortcuts_help_visible;
                    }

                    // Version stamp — clickable link to GitHub releases
                    {
                        const MAX_LEN: usize = 24;
                        let version = env!("NEXRAD_VERSION");
                        let full_version = env!("NEXRAD_VERSION_FULL");
                        let display = if version.len() > MAX_LEN {
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

                        response
                            .on_hover_text(format!("{} — click to view changelog", full_version));

                        if clicked {
                            if let Some(window) = web_sys::window() {
                                let _ = window.open_with_url_and_target(
                                    "https://github.com/danielway/nexrad-workbench/releases",
                                    "_blank",
                                );
                            }
                        }
                    }

                    ui.separator();

                    // View mode selector — all options always visible
                    let modes: &[(&str, ViewMode, Option<CameraMode>, Color32, &str)] = &[
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
                    ];

                    let dim = Color32::from_rgb(100, 100, 100);

                    for &(label, view, cam, color, key) in modes {
                        let is_active = match (view, cam) {
                            (ViewMode::Flat2D, _) => state.viz_state.view_mode == ViewMode::Flat2D,
                            (ViewMode::Globe3D, Some(cm)) => {
                                state.viz_state.view_mode == ViewMode::Globe3D
                                    && state.viz_state.camera.mode == cm
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
                });
            });
        });
}

/// Render a compact alerts indicator for the top bar. Shows nothing when
/// no active NWS alerts intersect the current viewing area (or when the
/// viewing area is undefined, e.g. in 3D globe mode).
fn render_alerts_chip(ui: &mut egui::Ui, state: &mut AppState) {
    // Show a subtle loading/error hint on the first fetch so the user knows
    // the feed is being contacted. After the first success, stay quiet unless
    // there are alerts to surface.
    let has_ever_loaded = state.alerts.last_success_ms > 0.0;
    let has_error = state.alerts.last_error.is_some();
    if !has_ever_loaded && !has_error {
        let icon = RichText::new(egui_phosphor::regular::BELL_SIMPLE)
            .size(14.0)
            .color(Color32::from_rgb(130, 130, 130));
        ui.add(egui::Label::new(icon))
            .on_hover_text("Loading NWS alerts\u{2026}");
        ui.separator();
        return;
    }

    let Some(bounds) = state.viz_state.last_visible_bounds else {
        // 3D globe view or canvas hasn't rendered yet.
        return;
    };

    let visible: Vec<(String, String, AlertSeverity)> = state
        .alerts
        .visible_in(bounds)
        .into_iter()
        .map(|a| (a.id.clone(), a.event.clone(), a.severity))
        .collect();

    if visible.is_empty() {
        // Render a quiet dimmed icon so users know the feed is live when hovered.
        let tooltip = if has_error {
            format!(
                "NWS alerts: {}",
                state.alerts.last_error.as_deref().unwrap_or("error")
            )
        } else {
            format!(
                "No active alerts in view ({} active nationwide)",
                state.alerts.alerts.len()
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

    // Use the highest-severity alert for the chip color (list is already
    // sorted by severity descending by `visible_in`).
    let top_severity = visible[0].2;
    let (r, g, b) = top_severity.color();
    let chip_color = Color32::from_rgb(r, g, b);

    let label = if visible.len() == 1 {
        let event = &visible[0].1;
        format!("{} {}", egui_phosphor::regular::WARNING, event)
    } else {
        format!(
            "{} {} alerts",
            egui_phosphor::regular::WARNING,
            visible.len()
        )
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
        for (_, event, sev) in visible.iter().take(6) {
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
            state.alerts.list_modal_open = true;
        }
    }

    ui.separator();
}

/// Render the unified mode badge (Idle / Archive / Live) in the top bar.
/// The Live branch preserves the previous pulse animation and streaming
/// detail text (chunk counter, acquire-lock elapsed, next-chunk countdown).
fn render_mode_badge(ui: &mut egui::Ui, state: &AppState) {
    let mode = state.app_mode;
    let color = mode.color();

    let icon_str = match mode {
        AppMode::Idle => egui_phosphor::regular::PAUSE_CIRCLE,
        AppMode::Archive => egui_phosphor::regular::ARCHIVE_BOX,
        AppMode::Live => egui_phosphor::regular::BROADCAST,
    };

    // For Live, pulse the icon's alpha channel; other modes render solid.
    let icon_color = if mode == AppMode::Live {
        let pulse = state.live_mode_state.pulse_alpha();
        Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            (128.0 + 127.0 * pulse) as u8,
        )
    } else {
        color
    };

    ui.label(RichText::new(icon_str).size(16.0).color(icon_color));
    ui.label(RichText::new(mode.label()).size(13.0).strong().color(color));

    // Live-only trailing detail: chunk count, countdown, or elapsed acquire time.
    if mode == AppMode::Live {
        use crate::state::LivePhase;
        let now = state.playback_state.playback_position();
        let phase = state.live_mode_state.phase;
        let detail = match phase {
            LivePhase::AcquiringLock => {
                let elapsed = state.live_mode_state.phase_elapsed_secs(now) as i32;
                format!("acquiring lock... {}s", elapsed)
            }
            LivePhase::Streaming => format!(
                "({} chunks) receiving...",
                state.live_mode_state.chunks_received
            ),
            LivePhase::WaitingForChunk => {
                if let Some(remaining) = state.live_mode_state.countdown_remaining_secs(now) {
                    format!(
                        "({} chunks) next in {}s",
                        state.live_mode_state.chunks_received,
                        remaining.ceil() as i32
                    )
                } else {
                    format!("({} chunks)", state.live_mode_state.chunks_received)
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
