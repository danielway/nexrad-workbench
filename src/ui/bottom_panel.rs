//! Bottom panel UI: playback controls and timeline.

use crate::state::{AppState, PlaybackMode, PlaybackSpeed};
use chrono::{Datelike, TimeZone, Timelike, Utc};
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

/// Time intervals for tick marks, from coarsest to finest
#[derive(Clone, Copy)]
struct TickConfig {
    /// Interval in seconds for major ticks
    major_interval: i64,
    /// Number of minor ticks between major ticks
    minor_divisions: i32,
    /// Minimum pixels per major tick to use this config
    min_pixels_per_major: f64,
}

const TICK_CONFIGS: &[TickConfig] = &[
    // Years (approximate - 365 days)
    TickConfig { major_interval: 365 * 24 * 3600, minor_divisions: 4, min_pixels_per_major: 60.0 },
    // Quarters (approximate - 91 days)
    TickConfig { major_interval: 91 * 24 * 3600, minor_divisions: 3, min_pixels_per_major: 60.0 },
    // Months (approximate - 30 days)
    TickConfig { major_interval: 30 * 24 * 3600, minor_divisions: 4, min_pixels_per_major: 60.0 },
    // Weeks
    TickConfig { major_interval: 7 * 24 * 3600, minor_divisions: 7, min_pixels_per_major: 60.0 },
    // Days
    TickConfig { major_interval: 24 * 3600, minor_divisions: 4, min_pixels_per_major: 60.0 },
    // 6 hours
    TickConfig { major_interval: 6 * 3600, minor_divisions: 6, min_pixels_per_major: 60.0 },
    // Hours
    TickConfig { major_interval: 3600, minor_divisions: 4, min_pixels_per_major: 60.0 },
    // 15 minutes
    TickConfig { major_interval: 15 * 60, minor_divisions: 3, min_pixels_per_major: 60.0 },
    // 5 minutes
    TickConfig { major_interval: 5 * 60, minor_divisions: 5, min_pixels_per_major: 60.0 },
    // 1 minute
    TickConfig { major_interval: 60, minor_divisions: 4, min_pixels_per_major: 60.0 },
    // 15 seconds
    TickConfig { major_interval: 15, minor_divisions: 3, min_pixels_per_major: 60.0 },
    // 5 seconds
    TickConfig { major_interval: 5, minor_divisions: 5, min_pixels_per_major: 60.0 },
    // 1 second
    TickConfig { major_interval: 1, minor_divisions: 4, min_pixels_per_major: 60.0 },
];

fn select_tick_config(zoom: f64) -> &'static TickConfig {
    // zoom is pixels per second
    // We want at least min_pixels_per_major pixels between major ticks
    // Iterate from finest (seconds) to coarsest (years), return the finest that fits
    for config in TICK_CONFIGS.iter().rev() {
        let pixels_per_major = zoom * config.major_interval as f64;
        if pixels_per_major >= config.min_pixels_per_major {
            return config;
        }
    }
    // Fallback to coarsest if nothing fits
    &TICK_CONFIGS[0]
}

fn format_timestamp(timestamp: i64, tick_config: &TickConfig) -> String {
    let dt = Utc.timestamp_opt(timestamp, 0).unwrap();
    let interval = tick_config.major_interval;

    if interval >= 30 * 24 * 3600 {
        // Months or longer: show "Jan 2024" or "2024"
        if interval >= 365 * 24 * 3600 {
            format!("{}", dt.year())
        } else {
            format!("{}", dt.format("%b %Y"))
        }
    } else if interval >= 24 * 3600 {
        // Days to weeks: show "May 15" or "Mon 15"
        format!("{}", dt.format("%b %d"))
    } else if interval >= 3600 {
        // Hours: show "14:00"
        format!("{:02}:{:02}", dt.hour(), dt.minute())
    } else if interval >= 60 {
        // Minutes: show "14:30"
        format!("{:02}:{:02}", dt.hour(), dt.minute())
    } else {
        // Seconds: show "14:30:45"
        format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second())
    }
}

fn render_timeline(ui: &mut egui::Ui, state: &mut AppState) {
    let available_width = ui.available_width() as f64;
    let timeline_height = 28.0;

    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_width as f32, timeline_height),
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

    let zoom = state.playback_state.timeline_zoom; // pixels per second
    if zoom <= 0.0 {
        return;
    }

    let view_start = state.playback_state.timeline_view_start; // absolute timestamp (f64)

    // Calculate visible time range
    let visible_secs = available_width / zoom;
    let view_end = view_start + visible_secs;

    // Helper to convert timestamp to x position
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    // Draw loaded data range highlight (if any data is loaded)
    if let (Some(data_start), Some(data_end)) = (
        state.playback_state.data_start_timestamp,
        state.playback_state.data_end_timestamp,
    ) {
        let data_x_start = ts_to_x(data_start as f64).max(rect.left());
        let data_x_end = ts_to_x(data_end as f64).min(rect.right());

        if data_x_end > data_x_start {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    Pos2::new(data_x_start, rect.top()),
                    Pos2::new(data_x_end, rect.bottom()),
                ),
                0.0,
                Color32::from_rgba_unmultiplied(60, 100, 60, 40),
            );
        }
    }

    // Select appropriate tick configuration
    let tick_config = select_tick_config(zoom);
    let major_interval = tick_config.major_interval;
    let minor_interval = (major_interval / tick_config.minor_divisions as i64).max(1);

    // Align to tick boundaries (use integer timestamps for ticks)
    let first_tick = ((view_start as i64) / minor_interval) * minor_interval;
    let last_tick = (((view_end as i64) / minor_interval) + 1) * minor_interval;

    // Draw tick marks
    let mut tick = first_tick;
    while tick <= last_tick {
        let x = ts_to_x(tick as f64);

        if x >= rect.left() && x <= rect.right() {
            let is_major = tick % major_interval == 0;
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
                let label = format_timestamp(tick, tick_config);
                painter.text(
                    Pos2::new(x, rect.top() + 10.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::monospace(9.0),
                    Color32::from_rgb(140, 140, 160),
                );
            }
        }

        tick += minor_interval;
    }

    // Draw playhead at current frame position (if data is loaded)
    if let Some(current_ts) = state.playback_state.current_timestamp() {
        let playhead_x = ts_to_x(current_ts as f64);

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
    }

    // Draw selection marker (if user has selected a time)
    if let Some(selected_ts) = state.playback_state.selected_timestamp {
        let sel_x = ts_to_x(selected_ts);

        if sel_x >= rect.left() && sel_x <= rect.right() {
            // Selection line (different color from playhead)
            painter.line_segment(
                [
                    Pos2::new(sel_x, rect.top()),
                    Pos2::new(sel_x, rect.bottom()),
                ],
                Stroke::new(2.0, Color32::from_rgb(100, 150, 255)),
            );

            // Selection diamond
            let diamond = vec![
                Pos2::new(sel_x, rect.top()),
                Pos2::new(sel_x + 5.0, rect.top() + 5.0),
                Pos2::new(sel_x, rect.top() + 10.0),
                Pos2::new(sel_x - 5.0, rect.top() + 5.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                diamond,
                Color32::from_rgb(100, 150, 255),
                Stroke::NONE,
            ));
        }
    }

    // Handle click to select time
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let clicked_ts = view_start + (pos.x - rect.left()) as f64 / zoom;
            state.playback_state.selected_timestamp = Some(clicked_ts);

            // If clicked within loaded data range, also seek to that frame
            if let Some(frame) = state.playback_state.timestamp_to_frame(clicked_ts as i64) {
                state.playback_state.current_frame = frame;
            }
        }
    }

    // Handle drag to pan - NO CLAMPING, free scrolling anywhere in time
    if response.dragged() {
        let delta_secs = -response.drag_delta().x as f64 / zoom;
        state.playback_state.timeline_view_start += delta_secs;
    }

    // Handle zoom with scroll wheel
    if response.hovered() {
        let scroll_delta = ui.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = 1.0 + scroll_delta.y as f64 * 0.002;
            let old_zoom = state.playback_state.timeline_zoom;
            // Allow zooming from ~10 years visible to ~1 second visible
            let new_zoom = (old_zoom * zoom_factor).clamp(0.000001, 1000.0);

            // Zoom centered on cursor position
            if let Some(cursor_pos) = response.hover_pos() {
                let cursor_ts = view_start + (cursor_pos.x - rect.left()) as f64 / old_zoom;
                let new_view_start = cursor_ts - (cursor_pos.x - rect.left()) as f64 / new_zoom;
                state.playback_state.timeline_view_start = new_view_start;
            }

            state.playback_state.timeline_zoom = new_zoom;
        }
    }
}

/// Format a timestamp (f64 unix seconds) for display with sub-second precision
fn format_timestamp_full(ts: f64) -> String {
    let secs = ts.floor() as i64;
    let millis = ((ts.fract()) * 1000.0).round() as u32;
    let dt = Utc.timestamp_opt(secs, 0).unwrap();
    format!(
        "{}.{:03}",
        dt.format("%Y-%m-%d %H:%M:%S"),
        millis
    )
}

fn render_playback_controls(ui: &mut egui::Ui, state: &mut AppState) {
    // Selected timestamp display (prominent)
    if let Some(selected_ts) = state.playback_state.selected_timestamp {
        ui.label(
            RichText::new(format_timestamp_full(selected_ts))
                .monospace()
                .size(13.0)
                .color(Color32::from_rgb(100, 150, 255)),
        );
        ui.separator();
    }

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
