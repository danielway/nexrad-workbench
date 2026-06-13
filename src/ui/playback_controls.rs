//! Playback controls: play/pause, speed, datetime picker, live indicator, and session stats.

use super::colors::{live, timeline as tl_colors, ui as ui_colors};
use super::overflow_menu::overflow_menu;
use super::timeline::format_timestamp_compact;
use crate::state::{AppMode, AppState, LoopMode, PlaybackMode, PlaybackSpeed, WidthTier};
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

                    ui.add_space(8.0);

                    // Quick jumps relative to now — the common "what was
                    // happening recently" cases without typing a timestamp.
                    // Phase 2's anchor fast-path fetches the landing scan
                    // automatically, so these are true one-click jumps.
                    let mut jump_target: Option<f64> = None;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Quick:").size(10.0));
                        for (label, secs_ago) in [
                            ("1h ago", 3_600.0),
                            ("6h ago", 21_600.0),
                            ("24h ago", 86_400.0),
                        ] {
                            if ui.small_button(label).clicked() {
                                jump_target = Some(state.frame_now.secs() - secs_ago);
                            }
                        }
                    });

                    ui.add_space(8.0);

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
                                jump_target = valid_ts;
                            }
                        });
                    });

                    if let Some(ts) = jump_target {
                        // Detach the playhead first — a seek while pinned
                        // would be rejected. The stream (if any) keeps
                        // running in the background unless the data-saver
                        // policy stops it.
                        live.detach_playhead(
                            &mut playback.state,
                            state.frame_now.secs(),
                            state.pause_stream_while_reviewing,
                        );

                        playback.state.set_playback_position(ts);

                        // Left-align the view: place the jumped-to position
                        // at ~5% from the left edge.
                        let view_width_secs = playback.state.view_width_secs();
                        playback.state.timeline_view_start = ts - view_width_secs * 0.05;

                        state.datetime_picker.close();
                        log::debug!("Jumped to timestamp: {}", ts);
                    }
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
            format_timestamp_compact(selected_ts, use_local, state.width_tier),
            tz_suffix
        ))
        .monospace()
        .size(13.0)
        .color(tl_colors::selection(state.is_dark));

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

    // Persistent, stateful LIVE button (spec §7). Always present in the
    // transport row: solid "● LIVE" while tethered, hollow "● LIVE · m:ss
    // behind" while a background stream runs detached (click re-tethers), and
    // hollow "● GO LIVE" with no stream (click starts one). Stopping the stream
    // is still owned by the timeline now-line cap / Ctrl+L.
    render_live_button(ui, state, live, playback);
    ui.separator();

    // Play/Pause button — disabled in Idle (no data to play). While tethered
    // (LIVE-NOW) the live feed is conceptually playing, so the button reads
    // PAUSE and pressing it FREEZES (drops to archive at the live edge,
    // detached). In archive it's ordinary play/pause; resuming after a freeze
    // plays from the pause point. All branching lives in
    // `transport::toggle_play_pause`.
    let tethered = playback.state.time_model.is_pinned();
    // Show PAUSE when the live feed is running (tethered) or archive playback
    // is active; PLAY otherwise.
    let play_text = if tethered || playback.state.playing {
        egui_phosphor::regular::PAUSE
    } else {
        egui_phosphor::regular::PLAY
    };
    let play_hover = if tethered {
        "Freeze (pause the live feed)"
    } else if playback.state.time_model.is_lookback() {
        "Return to live"
    } else if playback.state.playing {
        "Pause"
    } else {
        "Play"
    };

    if ui
        .add_enabled(
            interactive,
            egui::Button::new(RichText::new(play_text).size(14.0)),
        )
        .on_hover_text(play_hover)
        .clicked()
    {
        super::transport::toggle_play_pause(state, timeline, live, playback);
    }

    // Jog buttons step between sweeps. Stepping is a seek gesture, so while
    // tethered/replaying it detaches first (handled by `step_jog`), then steps
    // — the buttons stay visible in Live (spec §7/§12).
    // Step backward
    if ui
        .add_enabled(
            interactive,
            egui::Button::new(RichText::new(egui_phosphor::regular::SKIP_BACK).size(14.0)),
        )
        .clicked()
    {
        step_jog(state, timeline, live, playback, -1);
    }

    // Step forward
    if ui
        .add_enabled(
            interactive,
            egui::Button::new(RichText::new(egui_phosphor::regular::SKIP_FORWARD).size(14.0)),
        )
        .clicked()
    {
        step_jog(state, timeline, live, playback, 1);
    }

    ui.separator();

    // Speed selector (mode-aware: macro shows fps labels, micro shows timeline speed).
    // Disabled in Idle alongside the rest of the transport controls. Uses the
    // effective mode so a lookback replay shows fps options for its frame rate.
    let mode = playback.state.effective_playback_mode();
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

    // Below the full width tier, the Advanced-only loop combo and UTC toggle
    // are demoted into a ⋯ overflow menu so they don't crowd the transport row.
    let compact = state.width_tier < WidthTier::Full;
    let has_bounds = playback.state.time_model.playback_bounds.is_some();

    // Loop mode + clear-selection. The loop combo is Advanced-only and only
    // inline at full width; the clear-selection X always stays inline when
    // bounds are set so a Basic user landing on a `?selection=…` URL can clear
    // it.
    if has_bounds {
        ui.separator();
        if advanced && !compact {
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

    // UTC/Local toggle — Advanced-only, inline only at full width.
    if advanced && !compact {
        ui.separator();
        render_utc_toggle(ui, state);
    }

    // Overflow menu for the demoted Advanced controls (UTC toggle, and the
    // loop mode when a selection is active). Nothing is demoted in Basic, so
    // the menu only appears in Advanced at narrow widths.
    if advanced && compact {
        ui.separator();
        overflow_menu(ui, |ui| {
            ui.label(RichText::new("Time zone").size(11.0).weak());
            render_utc_toggle(ui, state);
            if has_bounds {
                ui.separator();
                ui.label(RichText::new("Loop").size(11.0).weak());
                // Listed as rows rather than a nested combo: a ComboBox inside
                // a close-on-click menu would dismiss the menu on its first
                // click before its own popup could open.
                for mode in LoopMode::all() {
                    if ui
                        .selectable_label(
                            playback.state.time_model.loop_mode == *mode,
                            mode.label(),
                        )
                        .clicked()
                    {
                        playback.state.time_model.loop_mode = *mode;
                    }
                }
            }
        });
    }

    // Push session stats to the right
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        render_session_stats(ui, state, playback, acquisition, chrome);
    });
}

/// Step the playhead one frame in `direction` (-1 back, +1 forward). Stepping is
/// a seek, so it detaches the playhead first (the stream, if any, keeps ingesting
/// unless the data-saver policy stops it) and then steps — exactly like the
/// mobile jog (`mobile::tabs::step_frame`). Macro frame-steps the index; Micro
/// seeks to the prev/next matching sweep end, falling back to a speed-sized time
/// nudge when no neighbor exists.
fn step_jog(
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    direction: isize,
) {
    let current_pos = playback.state.playback_position();
    // Detach before any `set_playback_position` (which debug-asserts Free mode).
    live.detach_playhead(
        &mut playback.state,
        state.frame_now.secs(),
        state.pause_stream_while_reviewing,
    );
    match playback.state.playback_mode() {
        PlaybackMode::Macro => {
            playback.state.step_macro_frame(direction);
        }
        PlaybackMode::Micro => {
            let step = playback.state.speed.timeline_seconds_per_real_second();
            let fallback = current_pos + step * direction as f64;
            let new_pos = match &state.viz_state.elevation_selection {
                crate::state::ElevationSelection::Fixed {
                    elevation_number, ..
                } => {
                    if direction < 0 {
                        timeline
                            .scans
                            .prev_matching_sweep_end_by_number(current_pos, *elevation_number)
                            .unwrap_or(fallback)
                    } else {
                        timeline
                            .scans
                            .next_matching_sweep_end_by_number(current_pos, *elevation_number)
                            .unwrap_or(fallback)
                    }
                }
                crate::state::ElevationSelection::Latest => {
                    if direction < 0 {
                        timeline
                            .scans
                            .prev_any_sweep_end(current_pos)
                            .unwrap_or(fallback)
                    } else {
                        timeline
                            .scans
                            .next_any_sweep_end(current_pos)
                            .unwrap_or(fallback)
                    }
                }
            };
            playback.state.set_playback_position(new_pos);
        }
    }
}

/// UTC/Local time-zone toggle. Rendered inline in the transport row at full
/// width and inside the overflow menu when space is tight.
fn render_utc_toggle(ui: &mut egui::Ui, state: &mut AppState) {
    let label = if state.use_local_time { "Local" } else { "UTC" };
    if ui
        .button(RichText::new(label).size(10.0).monospace())
        .on_hover_text("Toggle between UTC and local time")
        .clicked()
    {
        state.use_local_time = !state.use_local_time;
    }
}

/// Persistent, stateful LIVE button (spec §7). Three states, always present so
/// the live edge is one glance/one tap away no matter where the user is:
///
/// - **Tethered** (playhead attached, `AppMode::Live`): solid red "● LIVE".
///   Stopping the stream stays with the now-line cap / Ctrl+L, so this state is
///   an indicator — clicking is a no-op.
/// - **Detached** (stream running, playhead browsing): hollow "● LIVE · m:ss
///   behind" where the lag is wall-now − playhead. Click re-tethers
///   (`ReturnToLive`).
/// - **No stream** (idle): hollow "● GO LIVE". Click starts a stream.
///
/// If the stream dies underneath a detached state, `mode_state.is_active()`
/// flips false and the button falls through to GO LIVE (risk 4).
fn render_live_button(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
    let dot = egui_phosphor::regular::BROADCAST;
    let tethered = live.app_mode == AppMode::Live;
    let streaming = live.mode_state.is_active();

    if tethered {
        // Solid red badge — tethered to the live edge. Pulse the fill subtly so
        // it reads as "live" without being an alarm.
        let pulse = live.mode_state.pulse_alpha();
        let alpha = (200.0 + 55.0 * pulse) as u8;
        let fill = Color32::from_rgba_unmultiplied(
            live::STREAMING.r(),
            live::STREAMING.g(),
            live::STREAMING.b(),
            alpha,
        );
        let label = RichText::new(format!("{dot} LIVE"))
            .size(11.0)
            .strong()
            .color(Color32::WHITE);
        ui.add(egui::Button::new(label).fill(fill))
            .on_hover_text("Tethered to the live edge");
        return;
    }

    if streaming {
        // Detached background stream: hollow outline + lag readout. One click
        // snaps back to the live edge.
        let lag = state.frame_now.secs() - playback.state.playback_position();
        let label = RichText::new(format!(
            "{dot} LIVE · {} behind",
            crate::state::format_lag(lag)
        ))
        .size(11.0)
        .strong()
        .color(live::STREAMING);
        if ui
            .add(egui::Button::new(label).fill(Color32::TRANSPARENT))
            .on_hover_text("Stream running in background — click to rejoin live")
            .clicked()
        {
            state.push_command(crate::state::AppCommand::ReturnToLive);
        }
        return;
    }

    // No stream: hollow GO LIVE invitation.
    let label = RichText::new(format!("{dot} GO LIVE"))
        .size(11.0)
        .strong()
        .color(tl_colors::NOW_IDLE);
    if ui
        .add(egui::Button::new(label).fill(Color32::TRANSPARENT))
        .on_hover_text("Stream live from now")
        .clicked()
    {
        playback.state.clear_selection();
        state.push_command(crate::state::AppCommand::StartLive);
        playback.state.speed = PlaybackSpeed::Realtime;
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
