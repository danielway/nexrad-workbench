//! Playback controls: play/pause, speed, datetime picker, live indicator, and session stats.

use super::colors::{live, timeline as tl_colors, ui as ui_colors};
use super::modal_states::PickerField;
use super::overflow_menu::overflow_menu;
use super::timeline::format_timestamp_compact;
use crate::core::{LoopMode, PlaybackMode, PlaybackSpeed};
use crate::state::{AppMode, AppState, WidthTier};
use eframe::egui::{self, Color32, RichText};

/// Render the datetime picker popup for jumping to a specific time.
pub(super) fn render_datetime_picker_popup(
    ui: &mut egui::Ui,
    state: &mut AppState,
    picker: &mut super::DateTimePickerState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
) {
    if !picker.open {
        return;
    }

    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        picker.close();
        return;
    }

    let use_local = state.use_local_time;
    let tz_label = if use_local { "Local" } else { "UTC" };
    let popup_id = ui.make_persistent_id("datetime_picker_popup");

    // Intercept a whole-timestamp paste before the individual field editors
    // consume it, so pasting "2026-07-31T14:30:00Z" fills the whole form
    // instead of dumping the string into whichever box happens to have focus.
    // Single-field pastes ("07" into the month) fall through untouched.
    let pasted = ui.input_mut(|i| {
        let mut found: Option<String> = None;
        i.events.retain(|e| match e {
            egui::Event::Paste(s)
                if found.is_none() && super::modal_states::looks_like_timestamp(s) =>
            {
                found = Some(s.clone());
                false
            }
            _ => true,
        });
        found
    });
    if let Some(s) = pasted {
        if !picker.apply_paste(&s, use_local) {
            state.status_message = format!("Couldn't read \"{s}\" as a date/time");
        }
    }

    egui::Area::new(popup_id)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(280.0);

                ui.vertical(|ui| {
                    ui.heading(format!("Jump to Date/Time ({tz_label})"));
                    ui.add_space(8.0);

                    // Date row. Every field in this form is a
                    // two-way binding: the egui widget owns this value while
                    // the user edits it.
                    ui.horizontal(|ui| {
                        ui.label("Date:");
                        nudgeable_field(ui, &mut picker.year, PickerField::Year, 45.0, "YYYY");
                        ui.label("-");
                        nudgeable_field(ui, &mut picker.month, PickerField::Month, 25.0, "MM");
                        ui.label("-");
                        nudgeable_field(ui, &mut picker.day, PickerField::Day, 25.0, "DD");
                    });

                    ui.add_space(4.0);

                    // Time row
                    ui.horizontal(|ui| {
                        ui.label("Time:");
                        nudgeable_field(ui, &mut picker.hour, PickerField::Hour, 25.0, "HH");
                        ui.label(":");
                        nudgeable_field(ui, &mut picker.minute, PickerField::Minute, 25.0, "MM");
                        ui.label(":");
                        nudgeable_field(ui, &mut picker.second, PickerField::Second, 25.0, "SS");
                        ui.label(tz_label);
                    });

                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("↑/↓ adjusts a field · paste a full timestamp anywhere")
                            .size(9.0)
                            .weak(),
                    );

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
                    let valid_ts = picker.to_timestamp(use_local);
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
                            picker.close();
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

                        picker.close();
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

/// One zero-padded numeric field of the datetime picker, with arrow-key nudge.
///
/// Borrows the buffer directly (rather than taking `&mut DateTimePickerState`)
/// so the six calls can sit inside one `ui.horizontal` without re-borrowing the
/// picker; the clamping itself lives in `DateTimePickerState::nudge`.
fn nudgeable_field(
    ui: &mut egui::Ui,
    buf: &mut String,
    field: PickerField,
    width: f32,
    hint: &str,
) {
    let response = ui.add(
        egui::TextEdit::singleline(buf)
            .desired_width(width)
            .hint_text(hint),
    );
    if !response.has_focus() {
        return;
    }
    let delta = ui.input(|i| {
        i.key_pressed(egui::Key::ArrowUp) as i64 - i.key_pressed(egui::Key::ArrowDown) as i64
    });
    if delta != 0 {
        super::modal_states::nudge_buf(buf, field, delta);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_playback_controls(
    ui: &mut egui::Ui,
    state: &mut AppState,
    picker: &mut super::DateTimePickerState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    activity_vm: &crate::core::activity::ActivityVm,
) {
    let use_local = state.use_local_time;
    let advanced = state.show_advanced();
    // Idle = nothing under the playback cursor. Disable transport controls
    // so they don't visibly do nothing; layout stays stable for when data
    // arrives.
    let interactive = live.session.app_mode != AppMode::Idle;

    // Current position timestamp display. In Advanced it's a button that
    // opens the datetime picker; in Basic it's a plain label so a casual
    // viewer can't accidentally jump weeks into the past.
    {
        let selected_ts = playback.state.playback_position();
        let tz_suffix = if use_local { "" } else { " Z" };
        let mut text = RichText::new(format!(
            "{}{}",
            format_timestamp_compact(selected_ts, use_local, state.width_tier),
            tz_suffix
        ))
        .size(13.0)
        .color(tl_colors::selection(state.is_dark));
        // Monospace reads as a precise instrument in Advanced; Basic (Level 0,
        // spec §14 "no jargon") gets a plain, friendly clock readout instead.
        if advanced {
            text = text.monospace();
        }

        if advanced {
            let timestamp_btn = ui.add(egui::Button::new(text).frame(false));
            if timestamp_btn.clicked() {
                picker.init_from_timestamp(selected_ts, use_local);
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
    render_datetime_picker_popup(ui, state, picker, live, playback);

    // The selection chip: names what plays (LIVE, a point, a range, or a
    // range ending now) and opens the menu that sets it, including going
    // live and stopping the stream. `L` re-tethers / starts a stream;
    // `Shift+L` stops it; the timeline now-line cap mirrors the tether.
    let view_start = playback.state.timeline_view_start;
    super::selection_chip::render_selection_chip(
        ui,
        state,
        live,
        playback,
        (view_start, view_start + playback.state.view_width_secs()),
        &super::selection_chip::ChipStyle {
            text_size: 11.0,
            min_height: 0.0,
            stop_placement: crate::core::transport::LiveStopPlacement::LiveEdge,
            compact: false,
        },
    );
    // The tether's companion: whether data is actually still arriving. Silent
    // when there is no stream.
    render_stream_activity(ui, state, live);
    ui.separator();

    // Play/Pause button — disabled in Idle (no data to play). While tethered
    // (LIVE-NOW) the live feed is conceptually playing, so the button reads
    // PAUSE and pressing it FREEZES (drops to archive at the live edge,
    // detached). In archive it's ordinary play/pause; resuming after a freeze
    // plays from the pause point. All branching lives in
    // `core::transport::reduce_toggle_play_pause`, reached via
    // `Intent::TogglePlayPause`.
    let tethered = playback.state.is_pinned();
    // Show PAUSE when the live feed is running (tethered) or archive playback
    // is active; PLAY otherwise.
    let play_text = if tethered || playback.state.playing {
        egui_phosphor::regular::PAUSE
    } else {
        egui_phosphor::regular::PLAY
    };
    let play_hover = if tethered {
        "Freeze (pause the live feed)"
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
        state.push_command(crate::core::Intent::TogglePlayPause);
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

    // Frames / Rays: the user's explicit playback mode, never changed by zoom.
    let mode = playback.state.playback_mode();
    ui.add_enabled_ui(interactive, |ui| {
        for (choice, hint) in [
            (
                PlaybackMode::Macro,
                "Frames: step between complete sweeps at a set frame rate",
            ),
            (
                PlaybackMode::Micro,
                "Rays: play continuously at a multiple of real time, showing \
                 data as the radar collected it",
            ),
        ] {
            let resp = ui
                .add(egui::Button::selectable(mode == choice, choice.label()))
                .on_hover_text(hint);
            if resp.clicked() && mode != choice {
                state.push_command(crate::core::Intent::SetPlayMode(choice));
            }
        }
    });

    // Speed selector (mode-aware: Frames shows fps labels, Rays shows a
    // realtime multiple). Disabled in Idle alongside the rest of the transport
    // controls. Uses the effective mode so a lookback replay shows fps options
    // for its frame rate.
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
    let has_bounds = playback.state.playback_bounds().is_some();

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
            state.push_command(crate::core::Intent::ClearLoop);
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
        render_session_stats(ui, state, activity_vm);
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
                crate::core::ElevationSelection::Fixed {
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
                crate::core::ElevationSelection::Latest => {
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

/// Stream-activity chip: **is data arriving right now?** — the other half of
/// the fact the LIVE button used to carry alone.
///
/// Renders nothing when there is no stream, so it costs no chrome in the common
/// archive case. When there is one it projects the session's
/// `activity_chip` line — the activity word plus whatever visibly moves (the
/// chunk count ticking up, the "next in ~m:ss" countdown, the stall growing):
/// a changing number is unambiguous evidence of ingestion in a way a static
/// fill never was. The glyph pulses only while data is genuinely moving, so
/// motion means exactly one thing. Neutral/amber tones only — the accent
/// budget's red stays with the live edge and the tether.
fn render_stream_activity(ui: &mut egui::Ui, state: &AppState, live: &crate::subsystem::Live) {
    use crate::core::session::Stream;

    let session = &live.session;
    let Some(detail) = session.activity_chip.as_deref() else {
        return; // No stream — the chip costs no chrome in the archive case.
    };

    let dark = state.is_dark;
    let color = match session.stream {
        // Amber, not blue: a stall is degraded, and blue is the routine
        // "waiting" tone elsewhere in the live palette.
        Stream::Stalled | Stream::Failed => live::ACQUIRING,
        Stream::Receiving => ui_colors::value(dark),
        _ => ui_colors::label(dark),
    };
    // The pulse is applied to alpha so the chip breathes without changing hue.
    let color = if session.stream.is_animated() {
        let pulse = live.mode_state.pulse_alpha();
        let alpha = (150.0 + 105.0 * pulse) as u8;
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
    } else {
        color
    };

    let resp = ui.label(
        RichText::new(egui_phosphor::regular::BROADCAST)
            .size(11.0)
            .color(color),
    );
    ui.label(RichText::new(detail).size(11.0).monospace().color(color));
    if let Some(hover) = session.activity_hover {
        resp.on_hover_text(hover);
    }

    // The countdown must keep ticking even at the detached 1 s repaint
    // cadence (same pattern as the timeline's in-flight cells).
    if live.frame_status.countdown_secs.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// Render the right-hand cluster of the transport row.
///
/// Everything that used to live here — FPS, the DL/PROC/GPU pipeline lamps,
/// the request/bytes readout, the COI badge — moved into the activity sheet's
/// Details disclosure, where it is available to every user rather than only
/// behind `?dev=true`. What remains is the ambient activity chip plus the
/// Advanced-only cache control.
fn render_session_stats(
    ui: &mut egui::Ui,
    state: &mut AppState,
    activity_vm: &crate::core::activity::ActivityVm,
) {
    let dark = state.is_dark;
    let cache_size = state.session_stats.format_cache_size();

    // Ambient activity chip (spec §5) — for ALL users, always visible
    // including idle. Opens the activity sheet. The dev-only metrics above
    // are separate.
    super::activity_chip::render_activity_chip(ui, activity_vm, state, 11.0);
    ui.separator();

    // Cache group (size + clear button) — Advanced only. A Level-0 viewer
    // (spec §14) sees a clean transport, not a cache readout or a destructive
    // clear control; the right-panel storage section is the home for cache
    // management when Advanced is on.
    if state.show_advanced() {
        if ui.small_button("x").on_hover_text("Clear cache").clicked() {
            state.push_command(crate::core::Intent::ClearCache);
        }
        ui.label(
            RichText::new(cache_size)
                .size(10.0)
                .color(ui_colors::value(dark)),
        );
    }
}
