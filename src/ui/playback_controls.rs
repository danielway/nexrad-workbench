//! The transport row (§1 E): the playhead clock, the selection chip, play and
//! step, Frames or Rays, speed, and the activity chip. A thin projection of
//! [`crate::core::transport::transport_vm`] and the session view-model; every
//! control is always enabled and only emits an intent, and the core decides
//! what it means in the current state.
//!
//! Also the phone's datetime picker popup, until the phone gets the time field.

use super::modal_states::PickerField;
use crate::core::transport::{PlayButton, SeekGesture, SpeedControl, StepDir, StepUnit};
use crate::core::{Intent, PlaybackMode};
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, RichText};

/// Render the datetime picker popup for jumping to a specific time.
pub(super) fn render_datetime_picker_popup(
    ui: &mut egui::Ui,
    state: &mut AppState,
    picker: &mut super::DateTimePickerState,
) {
    if !picker.open {
        return;
    }

    // Claim Escape so the dialog stack does not also close the sheet under it.
    if ui
        .ctx()
        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
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
                        ui.colored_label(theme::legacy::ERROR, "Invalid date/time");
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
                        state.push_command(Intent::Seek {
                            ts,
                            gesture: SeekGesture::Typed,
                        });
                        picker.close();
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

/// The desktop transport row.
pub(super) fn render_playback_controls(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
    activity_vm: &crate::core::activity::ActivityVm,
) {
    let vm = crate::core::transport::transport_vm(&playback.state);
    let now = state.frame_now.secs();

    // The playhead clock, in the playhead's white: click it, type or paste a
    // time, Enter.
    if let Some(ts) = super::time_field::time_field(
        ui,
        "playhead_clock",
        playback.state.playback_position(),
        now,
        state.use_local_time,
        &super::time_field::TimeFieldLook {
            color: theme::SEEN,
            size: theme::READOUT,
            hover: "The playhead. Click to type or paste a time.",
        },
    ) {
        state.push_command(Intent::Seek {
            ts,
            gesture: SeekGesture::Typed,
        });
    }
    ui.separator();

    // What plays, and the menu that sets it.
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
    ui.separator();

    let glyph = match vm.play {
        PlayButton::Play => egui_phosphor::regular::PLAY,
        PlayButton::Pause => egui_phosphor::regular::PAUSE,
    };
    if ui
        .button(RichText::new(glyph).size(14.0))
        .on_hover_text(vm.play_hover)
        .clicked()
    {
        state.push_command(Intent::TogglePlayPause);
    }
    for (dir, glyph, hover) in [
        (
            StepDir::Back,
            egui_phosphor::regular::SKIP_BACK,
            "Previous frame (←; Shift+← for a volume)",
        ),
        (
            StepDir::Forward,
            egui_phosphor::regular::SKIP_FORWARD,
            "Next frame (→; Shift+→ for a volume)",
        ),
    ] {
        if ui
            .button(RichText::new(glyph).size(14.0))
            .on_hover_text(hover)
            .clicked()
        {
            let unit = if ui.input(|i| i.modifiers.shift) {
                StepUnit::Volume
            } else {
                StepUnit::Frame
            };
            state.push_command(Intent::Step { dir, unit });
        }
    }
    ui.separator();

    // Frames / Rays: the user's explicit playback mode, never changed by zoom.
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
            .add(egui::Button::selectable(vm.mode == choice, choice.label()))
            .on_hover_text(hint);
        if resp.clicked() && vm.mode != choice {
            state.push_command(Intent::SetPlayMode(choice));
        }
    }

    match &vm.speed {
        SpeedControl::Locked { label } => {
            ui.label(RichText::new(label).size(theme::LABEL))
                .on_hover_text("Real time while pinned to now in Rays");
        }
        SpeedControl::Menu { selected, choices } => {
            egui::ComboBox::from_id_salt("speed_selector")
                .selected_text(selected.as_str())
                .width(55.0)
                .show_ui(ui, |ui| {
                    for (speed, label) in choices {
                        let on = *speed == playback.state.speed;
                        if ui.selectable_label(on, label).clicked() && !on {
                            state.push_command(Intent::SetSpeed(*speed));
                        }
                    }
                });
        }
    }

    // What the app is doing to get data, at the right edge.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        super::activity_chip::render_activity_chip(ui, activity_vm, state, 11.0);
    });
}
