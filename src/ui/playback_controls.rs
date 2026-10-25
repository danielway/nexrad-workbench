//! The transport row (§1 E): the playhead clock, the selection chip, play and
//! step, Frames or Rays, speed, and the activity chip. A thin projection of
//! [`crate::core::transport::transport_vm`] and the session view-model; every
//! control is always enabled and only emits an intent, and the core decides
//! what it means in the current state.

use crate::core::transport::{PlayButton, SeekGesture, SpeedControl, StepDir, StepUnit};
use crate::core::{Intent, PlaybackMode};
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, RichText};

/// The desktop transport row.
pub(super) fn render_playback_controls(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
    activity_vm: &crate::core::activity::ActivityVm,
) {
    let vm = crate::core::transport::transport_vm(&playback.state, state.viz_state.tilt_role());
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
    // The calendar: pick a day by its storm activity, or type a time.
    let calendar = ui
        .button(RichText::new(crate::ui::icons::CALENDAR_BLANK).size(14.0))
        .on_hover_text("Go to a day (G)");
    ui.ctx()
        .data_mut(|d| d.insert_temp(super::go_to_day::anchor_id(), calendar.rect));
    if calendar.clicked() {
        state.push_command(Intent::OpenGoToDay);
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
            phone: None,
            max_width: None,
        },
    );
    ui.separator();

    let glyph = match vm.play {
        PlayButton::Play => crate::ui::icons::PLAY,
        PlayButton::Pause => crate::ui::icons::PAUSE,
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
            crate::ui::icons::SKIP_BACK,
            "Previous frame (←; Shift+← for a volume)",
        ),
        (
            StepDir::Forward,
            crate::ui::icons::SKIP_FORWARD,
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
        let disabled = (choice == PlaybackMode::Micro)
            .then_some(vm.rays_disabled)
            .flatten();
        let resp = ui
            .add_enabled(
                disabled.is_none(),
                egui::Button::selectable(vm.mode == choice, choice.label()),
            )
            .on_hover_text(hint)
            .on_disabled_hover_text(disabled.unwrap_or_default());
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
        super::activity_chip::render_activity_chip(ui, activity_vm, state, 11.0, false);
        readout(ui, &activity_vm.readout);
    });
}

/// The frame rate and request count, faint and small to the left of the
/// activity chip.
fn readout(ui: &mut egui::Ui, readout: &crate::core::activity::Readout) {
    ui.add_space(6.0);
    ui.label(
        RichText::new(readout.text())
            .monospace()
            .size(theme::CAPTION)
            .color(theme::TEXT_FAINT),
    )
    .on_hover_text(
        "Frames per second while the map is moving (the app redraws only \
         when something changes), and network requests this session: \
         S3 listings and scans, the activity index and mosaic images.",
    );
}
