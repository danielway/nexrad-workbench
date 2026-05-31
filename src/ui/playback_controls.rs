//! Playback controls: play/pause, speed, datetime picker, live indicator, and session stats.

use super::colors::{live, timeline as tl_colors, ui as ui_colors};
use super::timeline::format_timestamp_full;
use crate::state::{
    AppMode, AppState, LiveExitReason, LivePhase, LoopMode, PlaybackMode, PlaybackSpeed, TimeModel,
};
use crate::subsystem::Acquisition;
use eframe::egui::{self, Color32, RichText, Vec2};

/// Render the datetime picker popup for jumping to a specific time.
pub(super) fn render_datetime_picker_popup(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
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

                    // Enter key submits the form when valid
                    let enter_pressed =
                        ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid_ts.is_some();

                    ui.add_space(8.0);

                    // Buttons
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            state.datetime_picker.close();
                        }

                        ui.add_enabled_ui(valid_ts.is_some(), |ui| {
                            if ui.button("Jump").clicked() || enter_pressed {
                                if let Some(ts) = valid_ts {
                                    // Update playback position
                                    playback.state.set_playback_position(ts);

                                    // Left-align timeline view on new position
                                    // Place the jumped-to position at ~5% from the left edge
                                    let view_width_secs = playback.state.view_width_secs();
                                    playback.state.timeline_view_start =
                                        ts - view_width_secs * 0.05;

                                    // Exit live mode if active
                                    if live.mode_state.is_active() {
                                        live.mode_state.stop(LiveExitReason::UserSeeked);
                                        playback.state.time_model.disable_realtime_lock();
                                    }

                                    state.datetime_picker.close();
                                    log::debug!("Jumped to timestamp: {}", ts);
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

pub(super) fn render_playback_controls(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    acquisition: &mut Acquisition,
    chrome: &mut crate::subsystem::Chrome,
) {
    let use_local = state.use_local_time;
    let advanced = state.show_advanced();
    // Idle = nothing under the playback cursor. Disable transport controls
    // so they don't visibly do nothing; layout stays stable for when data
    // arrives.
    let interactive = live.app_mode != AppMode::Idle;

    // Current position timestamp display. In Advanced it's a button that
    // opens the datetime picker; in Basic it's a plain label so a casual
    // viewer can't accidentally jump weeks into the past.
    {
        let selected_ts = playback.state.playback_position();
        let tz_suffix = if use_local { "" } else { " Z" };
        let text = RichText::new(format!(
            "{}{}",
            format_timestamp_full(selected_ts, use_local),
            tz_suffix
        ))
        .monospace()
        .size(13.0)
        .color(tl_colors::SELECTION);

        if advanced {
            let timestamp_btn = ui.add(egui::Button::new(text).frame(false));
            if timestamp_btn.clicked() {
                state
                    .datetime_picker
                    .init_from_timestamp(selected_ts, use_local);
            }
            if timestamp_btn.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            timestamp_btn.on_hover_text("Click to jump to a specific date/time");
        } else {
            ui.label(text);
        }

        ui.separator();
    }

    // Datetime picker popup
    render_datetime_picker_popup(ui, state, live, playback);

    // Live mode indicator badge (when active)
    if live.mode_state.is_active() {
        render_live_indicator(ui, state, live, playback);
        ui.separator();
    }
    // Live entry emerges from the timeline: press play at the live edge, click
    // the now-line, or use the Go-live (crosshair) button below. The top-bar
    // mode pill is now an indicator only.

    // Play/Stop button — disabled in Idle (no data to play).
    let play_text = if playback.state.playing {
        egui_phosphor::regular::STOP
    } else {
        egui_phosphor::regular::PLAY
    };

    if ui
        .add_enabled(
            interactive,
            egui::Button::new(RichText::new(play_text).size(14.0)),
        )
        .clicked()
    {
        if playback.state.playing {
            // Pause. If live, exit and freeze the current frame (drop to
            // Archive): disabling the realtime lock stops `advance()` from
            // overwriting the position, and playing=false stops it advancing.
            if live.mode_state.is_active() {
                live.mode_state.stop(LiveExitReason::UserPaused);
                playback.state.time_model.disable_realtime_lock();
                state.status_message = live
                    .mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }
            playback.state.playing = false;
        } else if !live.mode_state.is_active() && playback.state.is_at_live_edge() {
            // Parked at the live edge — pressing play goes live rather than
            // replaying the last archive frames.
            state.push_command(crate::state::AppCommand::StartLive);
            playback.state.speed = PlaybackSpeed::Realtime;
        } else {
            // Ordinary playback from an archive position.
            playback.state.playing = true;
        }
    }

    // Jog: jump to end of next/previous matching sweep for current elevation
    let current_pos = playback.state.playback_position();

    // Step backward
    if ui
        .add_enabled(
            interactive,
            egui::Button::new(RichText::new(egui_phosphor::regular::SKIP_BACK).size(14.0)),
        )
        .clicked()
    {
        // Exit live mode when jogging
        if live.mode_state.is_active() {
            live.mode_state.stop(LiveExitReason::UserJogged);
            playback.state.time_model.disable_realtime_lock();
            state.status_message = live
                .mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        }
        match playback.state.playback_mode() {
            PlaybackMode::Macro => {
                playback.state.step_macro_frame(-1);
            }
            PlaybackMode::Micro => {
                let fallback =
                    current_pos - playback.state.speed.timeline_seconds_per_real_second();
                let new_pos = match &state.viz_state.elevation_selection {
                    crate::state::ElevationSelection::Fixed {
                        elevation_number, ..
                    } => timeline
                        .scans
                        .prev_matching_sweep_end_by_number(current_pos, *elevation_number)
                        .unwrap_or(fallback),
                    crate::state::ElevationSelection::Latest => timeline
                        .scans
                        .prev_any_sweep_end(current_pos)
                        .unwrap_or(fallback),
                };
                playback.state.set_playback_position(new_pos);
            }
        }
    }

    // Step-forward and "Now" are no-ops in Live (cursor is locked to wall
    // clock). Hide them in Basic+Live to declutter; Advanced always sees
    // them so power users keep their workflow.
    let show_forward_seek = advanced || live.app_mode != AppMode::Live;

    // Step forward
    if show_forward_seek
        && ui
            .add_enabled(
                interactive,
                egui::Button::new(RichText::new(egui_phosphor::regular::SKIP_FORWARD).size(14.0)),
            )
            .clicked()
    {
        // Exit live mode when jogging
        if live.mode_state.is_active() {
            live.mode_state.stop(LiveExitReason::UserJogged);
            playback.state.time_model.disable_realtime_lock();
            state.status_message = live
                .mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        }
        match playback.state.playback_mode() {
            PlaybackMode::Macro => {
                playback.state.step_macro_frame(1);
            }
            PlaybackMode::Micro => {
                let fallback =
                    current_pos + playback.state.speed.timeline_seconds_per_real_second();
                let new_pos = match &state.viz_state.elevation_selection {
                    crate::state::ElevationSelection::Fixed {
                        elevation_number, ..
                    } => timeline
                        .scans
                        .next_matching_sweep_end_by_number(current_pos, *elevation_number)
                        .unwrap_or(fallback),
                    crate::state::ElevationSelection::Latest => timeline
                        .scans
                        .next_any_sweep_end(current_pos)
                        .unwrap_or(fallback),
                };
                playback.state.set_playback_position(new_pos);
            }
        }
    }

    // "Go live" button — jump to now and start streaming. This is the
    // "I'm lost, take me to now" affordance for when the cursor is far in the
    // past, so it must re-center the view here (start_live_mode only re-centers
    // when it bumps zoom). It does NOT set position/lock/playing itself —
    // start_live_mode owns all of that and entry is async (AcquiringLock).
    if show_forward_seek
        && ui
            .add(egui::Button::new(
                RichText::new(egui_phosphor::regular::CROSSHAIR).size(14.0),
            ))
            .on_hover_text("Go live")
            .clicked()
    {
        let now = TimeModel::wall_clock_time();
        playback.state.center_view_on(now);
        state.push_command(crate::state::AppCommand::StartLive);
        playback.state.speed = PlaybackSpeed::Realtime;
    }

    ui.separator();

    // Speed selector (mode-aware: macro shows fps labels, micro shows timeline speed).
    // Disabled in Idle alongside the rest of the transport controls.
    let mode = playback.state.playback_mode();
    let selected_label = match mode {
        PlaybackMode::Macro => playback.state.speed.macro_label(),
        PlaybackMode::Micro => playback.state.speed.label(),
    };
    ui.add_enabled_ui(interactive, |ui| {
        egui::ComboBox::from_id_salt("speed_selector")
            .selected_text(selected_label)
            .width(55.0)
            .show_ui(ui, |ui| {
                let speeds: &[PlaybackSpeed] = match mode {
                    PlaybackMode::Macro => PlaybackSpeed::macro_speeds(),
                    PlaybackMode::Micro => PlaybackSpeed::all(),
                };
                for speed in speeds {
                    let label = match mode {
                        PlaybackMode::Macro => speed.macro_label(),
                        PlaybackMode::Micro => speed.label(),
                    };
                    ui.selectable_value(&mut playback.state.speed, *speed, label);
                }
            });
    });

    // Loop mode + clear-selection. Show the loop combo only in Advanced.
    // Always show the clear-selection X when bounds are set so a Basic
    // user landing on a `?selection=…` URL has a way to clear it.
    if playback.state.time_model.playback_bounds.is_some() {
        ui.separator();
        if advanced {
            egui::ComboBox::from_id_salt("loop_mode_selector")
                .selected_text(playback.state.time_model.loop_mode.label())
                .width(55.0)
                .show_ui(ui, |ui| {
                    for mode in LoopMode::all() {
                        ui.selectable_value(
                            &mut playback.state.time_model.loop_mode,
                            *mode,
                            mode.label(),
                        );
                    }
                });
        }

        if ui
            .small_button(egui_phosphor::regular::X)
            .on_hover_text("Clear selection and playback bounds")
            .clicked()
        {
            playback.state.clear_selection();
        }
    }

    if advanced {
        ui.separator();

        // UTC/Local toggle
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
        render_session_stats(ui, state, playback, acquisition, chrome);
    });
}

/// Render live mode indicator badge with pulsing dot.
fn render_live_indicator(
    ui: &mut egui::Ui,
    state: &AppState,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
) {
    let phase = live.mode_state.phase;
    let pulse_alpha = live.mode_state.pulse_alpha();

    // Get current time for status text
    let now = playback.state.playback_position();

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

            let elapsed = live.mode_state.phase_elapsed_secs(now) as i32;
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
            if live.mode_state.chunks_received > 0 {
                ui.label(
                    RichText::new(format!("({})", live.mode_state.chunks_received))
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
            } else if let Some(remaining) = live.mode_state.countdown_remaining_secs(now) {
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
fn render_session_stats(
    ui: &mut egui::Ui,
    state: &mut AppState,
    playback: &mut crate::subsystem::Playback,
    acquisition: &mut Acquisition,
    chrome: &mut crate::subsystem::Chrome,
) {
    let dark = state.is_dark;

    // FPS (rightmost) — read value before mutable borrow
    let fps = state.session_stats.avg_fps;
    let active_count = state.session_stats.active_request_count;
    let request_count = state.session_stats.session_request_count;
    let transferred = state.session_stats.format_transferred();
    let cache_size = state.session_stats.format_cache_size();

    // FPS, pipeline indicator, network metrics, and COI badge are all
    // dev-mode-only diagnostics — hidden by default.
    if state.dev_mode {
        if let Some(fps) = fps {
            ui.label(
                RichText::new(format!("{:.0} fps", fps))
                    .size(11.0)
                    .color(ui_colors::value(dark)),
            );
            ui.separator();
        }

        // Pipeline status — clickable phase boxes open detail modal
        render_pipeline_indicator(ui, state, playback, chrome);

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
            // Clickable to toggle acquisition drawer (subsumes network log modal)
            let queued = acquisition.state.queued_count();
            let req_text = if queued > 0 {
                format!("{}r / {} | {}q", display_count, display_transferred, queued)
            } else {
                format!("{}r / {}", display_count, display_transferred)
            };

            let drawer_icon = if acquisition.state.drawer_expanded {
                egui_phosphor::regular::CARET_DOWN
            } else {
                egui_phosphor::regular::CARET_UP
            };

            if ui
                .add(
                    egui::Label::new(
                        RichText::new(format!("{} {}", drawer_icon, req_text))
                            .size(10.0)
                            .color(ui_colors::value(dark)),
                    )
                    .sense(egui::Sense::click()),
                )
                .on_hover_text("Click to toggle acquisition drawer")
                .clicked()
            {
                acquisition.state.drawer_expanded = !acquisition.state.drawer_expanded;
            }
            ui.separator();
        }

        // Cross-origin isolation indicator
        if state.cross_origin_isolated {
            ui.label(RichText::new("COI").size(9.0).color(ui_colors::SUCCESS))
                .on_hover_text("Cross-Origin Isolated: SharedArrayBuffer available");
            ui.separator();
        }
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
fn render_pipeline_indicator(
    ui: &mut egui::Ui,
    state: &mut AppState,
    _playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
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
        chrome.stats_detail_open = !chrome.stats_detail_open;
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
