//! Top bar UI: app title, status, and site context.

use super::colors::live;
use crate::state::{AppState, CameraMode, LivePhase, ViewMode};
use eframe::egui::{self, Color32, RichText};

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

                // Show live status or regular status message
                if state.live_mode_state.is_active() {
                    render_live_status(ui, state);
                } else if !state.status_message.is_empty() {
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

                // Right-aligned: right panel toggle + view/camera mode
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new(egui_phosphor::regular::SIDEBAR_SIMPLE).size(14.0))
                        .on_hover_text("Toggle right panel")
                        .clicked()
                    {
                        state.right_sidebar_visible = !state.right_sidebar_visible;
                    }

                    ui.separator();

                    // View mode indicator (clickable to toggle)
                    let view_label = match state.viz_state.view_mode {
                        ViewMode::Globe3D => "3D",
                        ViewMode::Flat2D => "2D",
                    };
                    if ui
                        .button(
                            RichText::new(view_label)
                                .size(13.0)
                                .strong()
                                .color(Color32::from_rgb(100, 180, 255)),
                        )
                        .on_hover_text("Toggle view mode (G)")
                        .clicked()
                    {
                        state.viz_state.view_mode = match state.viz_state.view_mode {
                            ViewMode::Flat2D => ViewMode::Globe3D,
                            ViewMode::Globe3D => ViewMode::Flat2D,
                        };
                    }

                    // Camera mode indicator (only in 3D mode)
                    if state.viz_state.view_mode == ViewMode::Globe3D {
                        let mode = state.viz_state.camera.mode;
                        let mode_color = match mode {
                            CameraMode::PlanetOrbit => Color32::from_rgb(120, 200, 120),
                            CameraMode::SiteOrbit => Color32::from_rgb(255, 200, 80),
                            CameraMode::FreeLook => Color32::from_rgb(200, 140, 255),
                        };
                        if ui
                            .button(
                                RichText::new(mode.label())
                                    .size(12.0)
                                    .color(mode_color),
                            )
                            .on_hover_text("Cycle camera mode (C)")
                            .clicked()
                        {
                            state.viz_state.camera.mode = mode.next();
                        }
                    }
                });
            });
        });
}

/// Render live mode status in the top bar.
fn render_live_status(ui: &mut egui::Ui, state: &AppState) {
    let phase = state.live_mode_state.phase;
    let pulse_alpha = state.live_mode_state.pulse_alpha();

    let now = state.playback_state.playback_position();

    match phase {
        LivePhase::AcquiringLock => {
            let pulsed_color = Color32::from_rgba_unmultiplied(
                live::ACQUIRING.r(),
                live::ACQUIRING.g(),
                live::ACQUIRING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(RichText::new(egui_phosphor::regular::BROADCAST).size(16.0).color(pulsed_color));

            let elapsed = state.live_mode_state.phase_elapsed_secs(now) as i32;
            ui.label(
                RichText::new(format!("Acquiring lock... {}s", elapsed))
                    .size(13.0)
                    .color(live::ACQUIRING),
            );
        }
        LivePhase::Streaming | LivePhase::WaitingForChunk => {
            let pulsed_color = Color32::from_rgba_unmultiplied(
                live::STREAMING.r(),
                live::STREAMING.g(),
                live::STREAMING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(RichText::new(egui_phosphor::regular::BROADCAST).size(16.0).color(pulsed_color));
            ui.label(
                RichText::new("LIVE")
                    .size(13.0)
                    .strong()
                    .color(live::STREAMING),
            );

            let status = if phase == LivePhase::Streaming {
                format!(
                    "({} chunks) receiving...",
                    state.live_mode_state.chunks_received
                )
            } else if let Some(remaining) = state.live_mode_state.countdown_remaining_secs(now) {
                format!(
                    "({} chunks) next in {}s",
                    state.live_mode_state.chunks_received,
                    remaining.ceil() as i32
                )
            } else {
                format!("({} chunks)", state.live_mode_state.chunks_received)
            };

            ui.label(
                RichText::new(status)
                    .size(12.0)
                    .color(Color32::from_rgb(180, 180, 180)),
            );
        }
        _ => {}
    }
}
