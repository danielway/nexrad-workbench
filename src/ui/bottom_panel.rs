//! Bottom panel UI: playback controls.

use crate::state::{AppState, PlaybackMode, PlaybackSpeed};
use eframe::egui::{self, RichText};

pub fn render_bottom_panel(ctx: &egui::Context, state: &mut AppState) {
    egui::TopBottomPanel::bottom("bottom_panel")
        .exact_height(50.0)
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                render_playback_controls(ui, state);
            });
        });
}

fn render_playback_controls(ui: &mut egui::Ui, state: &mut AppState) {
    // Play/Pause button
    let play_text = if state.playback_state.playing {
        "⏸ Pause"
    } else {
        "▶ Play"
    };

    if ui.button(play_text).clicked() {
        state.playback_state.toggle_playback();
    }

    ui.separator();

    // Timeline slider
    let max_frame = state.playback_state.total_frames.saturating_sub(1);
    ui.add(
        egui::Slider::new(&mut state.playback_state.current_frame, 0..=max_frame)
            .show_value(false)
            .clamping(egui::SliderClamping::Always),
    );

    // Frame label
    ui.label(
        RichText::new(state.playback_state.frame_label())
            .monospace()
            .size(12.0),
    );

    ui.separator();

    // Speed selector
    ui.label(RichText::new("Speed:").size(12.0));
    egui::ComboBox::from_id_salt("speed_selector")
        .selected_text(state.playback_state.speed.label())
        .width(60.0)
        .show_ui(ui, |ui| {
            for speed in PlaybackSpeed::all() {
                ui.selectable_value(&mut state.playback_state.speed, *speed, speed.label());
            }
        });

    ui.separator();

    // Mode selector
    ui.label(RichText::new("Mode:").size(12.0));
    egui::ComboBox::from_id_salt("mode_selector")
        .selected_text(state.playback_state.mode.label())
        .width(110.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut state.playback_state.mode,
                PlaybackMode::RadialAccurate,
                PlaybackMode::RadialAccurate.label(),
            );
            ui.selectable_value(
                &mut state.playback_state.mode,
                PlaybackMode::FrameStep,
                PlaybackMode::FrameStep.label(),
            );
        });
}
