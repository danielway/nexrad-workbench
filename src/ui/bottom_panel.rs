//! Bottom panel UI: playback controls and timeline.

use crate::state::{AppState, PlaybackMode, PlaybackSpeed};
use eframe::egui::{self, Color32, Pos2, RichText, Sense, Stroke, StrokeKind, Vec2};

pub fn render_bottom_panel(ctx: &egui::Context, state: &mut AppState) {
    egui::TopBottomPanel::bottom("bottom_panel")
        .exact_height(70.0)
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                // Timeline row
                ui.add_space(4.0);
                render_timeline(ui, state);

                ui.add_space(4.0);

                // Playback controls row
                ui.horizontal(|ui| {
                    render_playback_controls(ui, state);
                });
            });
        });
}

fn render_timeline(ui: &mut egui::Ui, state: &mut AppState) {
    let available_width = ui.available_width();
    let timeline_height = 28.0;

    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_width, timeline_height),
        Sense::click_and_drag(),
    );
    let rect = response.rect;

    // Background
    painter.rect_filled(rect, 2.0, Color32::from_rgb(30, 30, 40));
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, Color32::from_rgb(60, 60, 80)),
        StrokeKind::Outside,
    );

    let total_frames = state.playback_state.total_frames;
    if total_frames == 0 {
        return;
    }

    let zoom = state.playback_state.timeline_zoom;
    let pan = state.playback_state.timeline_pan;

    // Calculate visible frame range
    let visible_frames = available_width / zoom;
    let start_frame = pan;
    let end_frame = (pan + visible_frames).min(total_frames as f32);

    // Determine tick interval based on zoom
    let tick_interval = determine_tick_interval(zoom);
    let tick_interval_f = tick_interval as f32;
    let minor_tick_interval = tick_interval / 5;

    // Draw tick marks
    let first_tick = ((start_frame / tick_interval_f).floor() * tick_interval_f) as i32;
    let last_tick = ((end_frame / tick_interval_f).ceil() * tick_interval_f) as i32;

    for tick in (first_tick..=last_tick).step_by(minor_tick_interval.max(1) as usize) {
        if tick < 0 || tick >= total_frames as i32 {
            continue;
        }

        let x = rect.left() + (tick as f32 - pan) * zoom;
        if x < rect.left() || x > rect.right() {
            continue;
        }

        let is_major = tick % tick_interval == 0;
        let tick_height = if is_major { 12.0 } else { 6.0 };
        let tick_color = if is_major {
            Color32::from_rgb(120, 120, 140)
        } else {
            Color32::from_rgb(60, 60, 80)
        };

        painter.line_segment(
            [
                Pos2::new(x, rect.bottom() - tick_height),
                Pos2::new(x, rect.bottom()),
            ],
            Stroke::new(1.0, tick_color),
        );

        // Draw label for major ticks
        if is_major {
            let label = format_frame_label(tick as usize, zoom);
            painter.text(
                Pos2::new(x, rect.top() + 10.0),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::monospace(9.0),
                Color32::from_rgb(140, 140, 160),
            );
        }
    }

    // Draw playhead
    let playhead_x = rect.left() + (state.playback_state.current_frame as f32 - pan) * zoom;
    if playhead_x >= rect.left() && playhead_x <= rect.right() {
        // Playhead line
        painter.line_segment(
            [
                Pos2::new(playhead_x, rect.top()),
                Pos2::new(playhead_x, rect.bottom()),
            ],
            Stroke::new(2.0, Color32::from_rgb(255, 100, 100)),
        );

        // Playhead triangle
        let triangle = vec![
            Pos2::new(playhead_x - 5.0, rect.top()),
            Pos2::new(playhead_x + 5.0, rect.top()),
            Pos2::new(playhead_x, rect.top() + 8.0),
        ];
        painter.add(egui::Shape::convex_polygon(
            triangle,
            Color32::from_rgb(255, 100, 100),
            Stroke::NONE,
        ));
    }

    // Handle interactions
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let clicked_frame = pan + (pos.x - rect.left()) / zoom;
            state.playback_state.current_frame =
                (clicked_frame.round() as usize).clamp(0, total_frames.saturating_sub(1));
        }
    }

    if response.dragged_by(egui::PointerButton::Middle)
        || (response.dragged() && ui.input(|i| i.modifiers.shift))
    {
        let delta_frames = -response.drag_delta().x / zoom;
        state.playback_state.timeline_pan =
            (pan + delta_frames).clamp(0.0, (total_frames as f32 - visible_frames).max(0.0));
    }

    // Handle zoom with scroll
    if response.hovered() {
        let scroll_delta = ui.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = 1.0 + scroll_delta.y * 0.002;
            let old_zoom = state.playback_state.timeline_zoom;
            let new_zoom = (old_zoom * zoom_factor).clamp(0.5, 50.0);

            // Zoom centered on cursor position
            if let Some(cursor_pos) = response.hover_pos() {
                let cursor_frame = pan + (cursor_pos.x - rect.left()) / old_zoom;
                let new_pan = cursor_frame - (cursor_pos.x - rect.left()) / new_zoom;
                state.playback_state.timeline_pan = new_pan.clamp(
                    0.0,
                    (total_frames as f32 - available_width / new_zoom).max(0.0),
                );
            }

            state.playback_state.timeline_zoom = new_zoom;
        }
    }
}

fn determine_tick_interval(zoom: f32) -> i32 {
    // Choose tick interval based on zoom level
    // At higher zoom (more pixels per frame), show finer intervals
    if zoom > 20.0 {
        5 // Every 5 frames
    } else if zoom > 10.0 {
        10 // Every 10 frames
    } else if zoom > 5.0 {
        20 // Every 20 frames
    } else if zoom > 2.0 {
        50 // Every 50 frames
    } else if zoom > 1.0 {
        100 // Every 100 frames
    } else {
        200 // Every 200 frames
    }
}

fn format_frame_label(frame: usize, _zoom: f32) -> String {
    format!("{}", frame)
}

fn render_playback_controls(ui: &mut egui::Ui, state: &mut AppState) {
    // Play/Pause button
    let play_text = if state.playback_state.playing {
        "\u{23F8}" // Pause symbol
    } else {
        "\u{25B6}" // Play symbol
    };

    if ui.button(RichText::new(play_text).size(14.0)).clicked() {
        state.playback_state.toggle_playback();
    }

    // Step backward
    if ui.button(RichText::new("\u{23EE}").size(14.0)).clicked() {
        if state.playback_state.current_frame > 0 {
            state.playback_state.current_frame -= 1;
        }
    }

    // Step forward
    if ui.button(RichText::new("\u{23ED}").size(14.0)).clicked() {
        if state.playback_state.current_frame < state.playback_state.total_frames.saturating_sub(1)
        {
            state.playback_state.current_frame += 1;
        }
    }

    ui.separator();

    // Frame label
    ui.label(
        RichText::new(state.playback_state.frame_label())
            .monospace()
            .size(12.0),
    );

    ui.separator();

    // Speed selector
    ui.label(RichText::new("Speed:").size(11.0));
    egui::ComboBox::from_id_salt("speed_selector")
        .selected_text(state.playback_state.speed.label())
        .width(55.0)
        .show_ui(ui, |ui| {
            for speed in PlaybackSpeed::all() {
                ui.selectable_value(&mut state.playback_state.speed, *speed, speed.label());
            }
        });

    ui.separator();

    // Mode selector
    ui.label(RichText::new("Mode:").size(11.0));
    egui::ComboBox::from_id_salt("mode_selector")
        .selected_text(state.playback_state.mode.label())
        .width(100.0)
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
