//! Playback controls: play/pause, speed, datetime picker, live indicator, and session stats.

use super::colors::{live, timeline as tl_colors, ui as ui_colors};
use super::timeline::format_timestamp_full;
use crate::state::{AppState, LiveExitReason, LivePhase, LoopMode, PlaybackSpeed};
use eframe::egui::{self, Color32, RichText, Vec2};

/// Render the datetime picker popup for jumping to a specific time.
fn render_datetime_picker_popup(ui: &mut egui::Ui, state: &mut AppState) {
    if !state.datetime_picker.open {
        return;
    }

    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        state.datetime_picker.close();
        return;
    }

    let use_local = state.use_local_time;
    let tz_label = if use_local { "Local" } else { "UTC" };
    let popup_id = ui.make_persistent_id("datetime_picker_popup");

    egui::Area::new(popup_id)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(280.0);

                ui.vertical(|ui| {
                    ui.heading(format!("Jump to Date/Time ({tz_label})"));
                    ui.add_space(8.0);

                    // Date row
                    ui.horizontal(|ui| {
                        ui.label("Date:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.datetime_picker.year)
                                .desired_width(45.0)
                                .hint_text("YYYY"),
                        );
                        ui.label("-");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.datetime_picker.month)
                                .desired_width(25.0)
                                .hint_text("MM"),
                        );
                        ui.label("-");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.datetime_picker.day)
                                .desired_width(25.0)
                                .hint_text("DD"),
                        );
                    });

                    ui.add_space(4.0);

                    // Time row
                    ui.horizontal(|ui| {
                        ui.label("Time:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.datetime_picker.hour)
                                .desired_width(25.0)
                                .hint_text("HH"),
                        );
                        ui.label(":");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.datetime_picker.minute)
                                .desired_width(25.0)
                                .hint_text("MM"),
                        );
                        ui.label(":");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.datetime_picker.second)
                                .desired_width(25.0)
                                .hint_text("SS"),
                        );
                        ui.label(tz_label);
                    });

                    ui.add_space(12.0);

                    // Validation feedback
                    let valid_ts = state.datetime_picker.to_timestamp(use_local);
                    if valid_ts.is_none() {
                        ui.colored_label(Color32::from_rgb(255, 100, 100), "Invalid date/time");
                    }

                    ui.add_space(8.0);

                    // Buttons
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            state.datetime_picker.close();
                        }

                        ui.add_enabled_ui(valid_ts.is_some(), |ui| {
                            if ui.button("Jump").clicked() {
                                if let Some(ts) = valid_ts {
                                    // Update playback position
                                    state.playback_state.set_playback_position(ts);

                                    // Left-align timeline view on new position
                                    // Place the jumped-to position at ~5% from the left edge
                                    let view_width_secs = state.playback_state.view_width_secs();
                                    state.playback_state.timeline_view_start =
                                        ts - view_width_secs * 0.05;

                                    // Exit live mode if active
                                    if state.live_mode_state.is_active() {
                                        state.live_mode_state.stop(LiveExitReason::UserSeeked);
                                        state.playback_state.time_model.disable_realtime_lock();
                                    }

                                    state.datetime_picker.close();
                                    log::info!("Jumped to timestamp: {}", ts);
                                }
                            }
                        });
                    });
                });
            });
        });

    // Close on click outside (check if clicked but not on the popup)
    if ui.input(|i| i.pointer.any_click()) {
        // We'll let the popup stay open as long as user is interacting with it
        // Close only via Cancel button or Jump button for now
    }
}

pub(super) fn render_playback_controls(ui: &mut egui::Ui, state: &mut AppState) {
    let use_local = state.use_local_time;

    // Current position timestamp display (clickable to open datetime picker)
    {
        let selected_ts = state.playback_state.playback_position();
        let tz_suffix = if use_local { "" } else { " Z" };
        let timestamp_btn = ui.add(
            egui::Button::new(
                RichText::new(format!(
                    "{}{}",
                    format_timestamp_full(selected_ts, use_local),
                    tz_suffix
                ))
                .monospace()
                .size(13.0)
                .color(tl_colors::SELECTION),
            )
            .frame(false),
        );

        if timestamp_btn.clicked() {
            state
                .datetime_picker
                .init_from_timestamp(selected_ts, use_local);
        }

        if timestamp_btn.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        timestamp_btn.on_hover_text("Click to jump to a specific date/time");

        ui.separator();
    }

    // Datetime picker popup
    render_datetime_picker_popup(ui, state);

    // Live mode indicator badge (when active)
    if state.live_mode_state.is_active() {
        render_live_indicator(ui, state);
        ui.separator();
    }

    // Live button (only shown when not in live mode)
    #[allow(clippy::collapsible_if)]
    if !state.live_mode_state.is_active() {
        if ui
            .button(
                RichText::new(format!("{} Live", egui_phosphor::regular::BROADCAST))
                    .size(12.0)
                    .color(Color32::from_rgb(150, 150, 150)),
            )
            .on_hover_text("Start live streaming")
            .clicked()
        {
            // Signal main loop to start live mode
            state.push_command(crate::state::AppCommand::StartLive);
            state.playback_state.speed = PlaybackSpeed::Realtime;
        }
    }

    // Play/Stop button
    let play_text = if state.playback_state.playing {
        egui_phosphor::regular::STOP
    } else {
        egui_phosphor::regular::PLAY
    };

    if ui.button(RichText::new(play_text).size(14.0)).clicked() {
        if state.playback_state.playing {
            // Stop - also exits live mode if active
            if state.live_mode_state.is_active() {
                state.live_mode_state.stop(LiveExitReason::UserStopped);
                state.playback_state.time_model.disable_realtime_lock();
                state.status_message = state
                    .live_mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }
            state.playback_state.playing = false;
        } else {
            // Play
            state.playback_state.playing = true;
        }
    }

    // Jog: jump to end of next/previous matching sweep for current elevation
    let current_pos = state.playback_state.playback_position();
    let target_elev = state.viz_state.target_elevation;
    const ELEV_TOLERANCE: f32 = 0.3;

    // Step backward
    if ui
        .button(RichText::new(egui_phosphor::regular::SKIP_BACK).size(14.0))
        .clicked()
    {
        // Exit live mode when jogging
        if state.live_mode_state.is_active() {
            state.live_mode_state.stop(LiveExitReason::UserJogged);
            state.playback_state.time_model.disable_realtime_lock();
            state.status_message = state
                .live_mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        }
        let new_pos = state
            .radar_timeline
            .prev_matching_sweep_end(current_pos, target_elev, ELEV_TOLERANCE)
            .unwrap_or(
                current_pos
                    - state
                        .playback_state
                        .speed
                        .timeline_seconds_per_real_second(),
            );
        state.playback_state.set_playback_position(new_pos);
    }

    // Step forward
    if ui
        .button(RichText::new(egui_phosphor::regular::SKIP_FORWARD).size(14.0))
        .clicked()
    {
        // Exit live mode when jogging
        if state.live_mode_state.is_active() {
            state.live_mode_state.stop(LiveExitReason::UserJogged);
            state.playback_state.time_model.disable_realtime_lock();
            state.status_message = state
                .live_mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        }
        let new_pos = state
            .radar_timeline
            .next_matching_sweep_end(current_pos, target_elev, ELEV_TOLERANCE)
            .unwrap_or(
                current_pos
                    + state
                        .playback_state
                        .speed
                        .timeline_seconds_per_real_second(),
            );
        state.playback_state.set_playback_position(new_pos);
    }

    ui.separator();

    // Speed selector
    ui.label(RichText::new("Speed:").size(11.0));
    egui::ComboBox::from_id_salt("speed_selector")
        .selected_text(state.playback_state.speed.label())
        .width(75.0)
        .show_ui(ui, |ui| {
            for speed in PlaybackSpeed::all() {
                ui.selectable_value(&mut state.playback_state.speed, *speed, speed.label());
            }
        });

    // Loop mode selector (only show when playback bounds are set)
    if state.playback_state.time_model.playback_bounds.is_some() {
        ui.separator();
        ui.label(RichText::new("Loop:").size(11.0));
        egui::ComboBox::from_id_salt("loop_mode_selector")
            .selected_text(state.playback_state.time_model.loop_mode.label())
            .width(70.0)
            .show_ui(ui, |ui| {
                for mode in LoopMode::all() {
                    ui.selectable_value(
                        &mut state.playback_state.time_model.loop_mode,
                        *mode,
                        mode.label(),
                    );
                }
            });

        // Clear selection button
        if ui
            .small_button(egui_phosphor::regular::X)
            .on_hover_text("Clear selection and playback bounds")
            .clicked()
        {
            state.playback_state.clear_selection();
        }
    }

    ui.separator();

    // Download button
    let has_selection = state.playback_state.selection_range().is_some();
    let download_in_progress = state.download_selection_in_progress;

    if download_in_progress {
        let label = if state.download_progress.is_batch() {
            format!(
                "Downloading {}/{}...",
                (state.download_progress.batch_completed + 1)
                    .min(state.download_progress.batch_total),
                state.download_progress.batch_total
            )
        } else {
            "Downloading...".to_string()
        };
        ui.add_enabled(false, egui::Button::new(RichText::new(label).size(11.0)));
    } else if has_selection {
        if ui
            .button(
                RichText::new(format!(
                    "{} Download Selection",
                    egui_phosphor::regular::DOWNLOAD_SIMPLE
                ))
                .size(11.0),
            )
            .on_hover_text("Download all scans in the selected time range")
            .clicked()
        {
            state.push_command(crate::state::AppCommand::DownloadSelection);
        }
    } else if ui
        .button(
            RichText::new(format!(
                "{} Download",
                egui_phosphor::regular::DOWNLOAD_SIMPLE
            ))
            .size(11.0),
        )
        .on_hover_text("Download the scan at the current playback position")
        .clicked()
    {
        state.push_command(crate::state::AppCommand::DownloadAtPosition);
    }

    ui.separator();

    // UTC/Local toggle
    {
        let label = if state.use_local_time { "Local" } else { "UTC" };
        if ui
            .button(RichText::new(label).size(10.0).monospace())
            .on_hover_text("Toggle between UTC and local time")
            .clicked()
        {
            state.use_local_time = !state.use_local_time;
        }
    }

    // Push session stats to the right
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        render_session_stats(ui, state);
    });
}

/// Render live mode indicator badge with pulsing dot.
fn render_live_indicator(ui: &mut egui::Ui, state: &AppState) {
    let phase = state.live_mode_state.phase;
    let pulse_alpha = state.live_mode_state.pulse_alpha();

    // Get current time for status text
    let now = state.playback_state.playback_position();

    match phase {
        LivePhase::AcquiringLock => {
            // Show "CONNECTING" with orange pulsing
            let pulsed_color = Color32::from_rgba_unmultiplied(
                live::ACQUIRING.r(),
                live::ACQUIRING.g(),
                live::ACQUIRING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(
                RichText::new(egui_phosphor::regular::BROADCAST)
                    .size(16.0)
                    .color(pulsed_color),
            );

            let elapsed = state.live_mode_state.phase_elapsed_secs(now) as i32;
            ui.label(
                RichText::new(format!("CONNECTING {}s", elapsed))
                    .size(11.0)
                    .strong()
                    .color(live::ACQUIRING),
            );
        }
        LivePhase::Streaming | LivePhase::WaitingForChunk => {
            // Show red "LIVE" indicator (always visible once streaming)
            let pulsed_color = Color32::from_rgba_unmultiplied(
                live::STREAMING.r(),
                live::STREAMING.g(),
                live::STREAMING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(
                RichText::new(egui_phosphor::regular::BROADCAST)
                    .size(16.0)
                    .color(pulsed_color),
            );
            ui.label(
                RichText::new("LIVE")
                    .size(11.0)
                    .strong()
                    .color(live::STREAMING),
            );

            // Show chunk count
            if state.live_mode_state.chunks_received > 0 {
                ui.label(
                    RichText::new(format!("({})", state.live_mode_state.chunks_received))
                        .size(10.0)
                        .color(ui_colors::value(state.is_dark)),
                );
            }

            // Show status: downloading or waiting
            if phase == LivePhase::Streaming {
                ui.label(
                    RichText::new("receiving...")
                        .size(10.0)
                        .italics()
                        .color(ui_colors::SUCCESS),
                );
            } else if let Some(remaining) = state.live_mode_state.countdown_remaining_secs(now) {
                ui.label(
                    RichText::new(format!("next in {}s", remaining.ceil() as i32))
                        .size(10.0)
                        .color(live::WAITING),
                );
            }
        }
        _ => {}
    }
}

/// Render session statistics (right-aligned in the bottom bar).
///
/// Layout (right-to-left): FPS | pipeline (clickable) | download | cache
fn render_session_stats(ui: &mut egui::Ui, state: &mut AppState) {
    let dark = state.is_dark;

    // FPS (rightmost) — read value before mutable borrow
    let fps = state.session_stats.avg_fps;
    let active_count = state.session_stats.active_request_count;
    let request_count = state.session_stats.session_request_count;
    let transferred = state.session_stats.format_transferred();
    let cache_size = state.session_stats.format_cache_size();

    if let Some(fps) = fps {
        ui.label(
            RichText::new(format!("{:.0} fps", fps))
                .size(11.0)
                .color(ui_colors::value(dark)),
        );
        ui.separator();
    }

    // Pipeline status — clickable phase boxes open detail modal
    render_pipeline_indicator(ui, state);

    // Download group: requests + transferred
    // Use service worker aggregate if available, otherwise fall back to channel stats
    let sw_total = state.network_aggregate.total_requests;
    let (display_count, display_transferred) = if sw_total > 0 {
        (
            sw_total,
            crate::state::format_bytes(state.network_aggregate.total_bytes),
        )
    } else {
        (request_count, transferred)
    };

    if active_count > 0 {
        ui.label(
            RichText::new(format!("({} active)", active_count))
                .size(10.0)
                .italics()
                .color(ui_colors::ACTIVE),
        );
    }
    if display_count > 0 {
        // Clickable to open network log
        let req_text = format!("{} req / {}", display_count, display_transferred);
        if ui
            .add(
                egui::Label::new(
                    RichText::new(req_text)
                        .size(10.0)
                        .color(ui_colors::value(dark)),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("Click to view network log")
            .clicked()
        {
            state.network_log_open = true;
        }
        ui.separator();
    }

    // Cross-origin isolation indicator
    if state.cross_origin_isolated {
        ui.label(RichText::new("COI").size(9.0).color(ui_colors::SUCCESS))
            .on_hover_text("Cross-Origin Isolated: SharedArrayBuffer available");
        ui.separator();
    }

    // Cache group: size with clear button
    if ui.small_button("x").on_hover_text("Clear cache").clicked() {
        state.push_command(crate::state::AppCommand::ClearCache);
    }
    ui.label(
        RichText::new(cache_size)
            .size(10.0)
            .color(ui_colors::value(dark)),
    );
}

/// Render pipeline phase indicator boxes (3 high-level groups).
///
/// Shows a row of small clickable phase labels (DL, PROC, GPU). Active or
/// recently-completed phases are highlighted; idle ones are dimmed.
/// Clicking any phase opens the detailed stats modal.
/// The indicator stays visible for 1.5 s after the last phase completes
/// so the user can see which stages ran.
fn render_pipeline_indicator(ui: &mut egui::Ui, state: &mut AppState) {
    let pipeline = &state.session_stats.pipeline;
    let progress = &state.download_progress;
    let dark = state.is_dark;

    // Each entry: (label, is_lit)
    // "lit" means actively running OR recently completed (within linger window)
    let dl_lit = pipeline.phase_visible(pipeline.downloading > 0, pipeline.last_download_done_ms);
    let proc_lit = pipeline.phase_visible(pipeline.processing, pipeline.last_processing_done_ms);
    let gpu_lit = pipeline.phase_visible(pipeline.rendering, pipeline.last_render_done_ms);

    // Show batch count on DL when doing a multi-file download
    let dl_label: String = if progress.is_batch() {
        format!(
            "DL {}/{}",
            (progress.batch_completed + 1).min(progress.batch_total),
            progress.batch_total
        )
    } else if pipeline.downloading > 1 {
        "DL+".to_string()
    } else {
        "DL".to_string()
    };

    let phases: &[(&str, bool)] = &[(&dl_label, dl_lit), ("PROC", proc_lit), ("GPU", gpu_lit)];

    // Also show compact latency summary after the indicator
    let has_any_timing = state.session_stats.median_chunk_latency_ms.is_some()
        || state.session_stats.median_processing_time_ms.is_some()
        || state.session_stats.avg_render_time_ms.is_some();

    let summary_text = if has_any_timing {
        Some(state.session_stats.format_latency_stats())
    } else {
        None
    };

    // Wider when showing batch count
    let base_width = if progress.is_batch() { 140.0 } else { 110.0 };
    let summary_width = summary_text
        .as_ref()
        .map(|s| s.len() as f32 * 6.0 + 16.0)
        .unwrap_or(0.0);
    let indicator_width = base_width + summary_width;

    // Use a fixed-width left-to-right sub-layout so phases read correctly
    // and don't consume all remaining horizontal space in the parent R-to-L layout.
    let mut clicked = false;
    ui.allocate_ui_with_layout(
        Vec2::new(indicator_width, ui.available_height()),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            let anim_time = ui.ctx().input(|i| i.time);
            let pulse = (0.5 + 0.5 * (anim_time * 3.0).sin()) as f32;

            for (i, (label, lit)) in phases.iter().enumerate() {
                if i > 0 {
                    ui.label(
                        RichText::new("\u{203A}")
                            .size(9.0)
                            .color(Color32::from_rgb(70, 70, 80)),
                    );
                }
                let color = if *lit {
                    // Pulse the active phase for visual emphasis
                    let base = ui_colors::ACTIVE;
                    let alpha = (180.0 + 75.0 * pulse) as u8;
                    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha)
                } else if dark {
                    Color32::from_rgb(55, 55, 65)
                } else {
                    Color32::from_rgb(180, 180, 190)
                };
                let btn = ui.add(
                    egui::Button::new(RichText::new(*label).size(9.0).monospace().color(color))
                        .frame(false),
                );
                if btn.clicked() {
                    clicked = true;
                }
                btn.on_hover_text("Click for detailed timing breakdown");
            }

            // Compact latency summary inline after the indicator
            if let Some(ref summary) = summary_text {
                ui.add_space(4.0);
                let btn = ui.add(
                    egui::Button::new(
                        RichText::new(summary)
                            .size(10.0)
                            .color(ui_colors::value(dark)),
                    )
                    .frame(false),
                );
                if btn.clicked() {
                    clicked = true;
                }
                btn.on_hover_text("Click for detailed timing breakdown");
            }
        },
    );

    if clicked {
        state.stats_detail_open = !state.stats_detail_open;
    }

    ui.separator();

    // Request repaint while lingering so phases fade out smoothly
    if pipeline.should_show() && !pipeline.is_active() {
        ui.ctx().request_repaint();
    }
    // Also repaint during batch downloads for pulse animation
    if progress.is_active() {
        ui.ctx().request_repaint();
    }
}
