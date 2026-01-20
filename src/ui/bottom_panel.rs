//! Bottom panel UI: playback controls, timeline, and session statistics.

use super::colors::{live, timeline as tl_colors, ui as ui_colors};
use crate::state::radar_data::RadarTimeline;
use crate::state::{AppState, LiveExitReason, LivePhase, PlaybackSpeed, SessionStats};
use chrono::{Datelike, TimeZone, Timelike, Utc};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

/// Get current Unix timestamp in seconds (works on both native and WASM)
fn current_timestamp_secs() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() / 1000.0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}

/// Level of detail for radar data rendering
#[derive(Clone, Copy, PartialEq)]
enum DetailLevel {
    /// Just show solid color where data exists
    Solid,
    /// Show individual scan blocks
    Scans,
    /// Show sweep blocks within scans
    Sweeps,
}

/// Render radar data on the timeline at the appropriate detail level
fn render_radar_data(
    painter: &Painter,
    rect: &Rect,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
) {
    // Helper to convert timestamp to x position
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    let scan_color = tl_colors::SCAN_FILL;
    let scan_border = tl_colors::SCAN_BORDER;
    let sweep_colors: [Color32; 4] = [
        Color32::from_rgb(50, 100, 70),
        Color32::from_rgb(60, 120, 80),
        Color32::from_rgb(70, 140, 90),
        Color32::from_rgb(55, 110, 75),
    ];

    match detail_level {
        DetailLevel::Solid => {
            // Draw solid regions for each contiguous time range
            for range in timeline.time_ranges() {
                let x_start = ts_to_x(range.start).max(rect.left());
                let x_end = ts_to_x(range.end).min(rect.right());

                if x_end > x_start {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(x_start, rect.top() + 2.0),
                            Pos2::new(x_end, rect.bottom() - 2.0),
                        ),
                        2.0,
                        Color32::from_rgba_unmultiplied(60, 120, 80, 180),
                    );
                }
            }
        }
        DetailLevel::Scans => {
            // Draw individual scan blocks
            for scan in timeline.scans_in_range(view_start, view_end) {
                let x_start = ts_to_x(scan.start_time).max(rect.left());
                let x_end = ts_to_x(scan.end_time).min(rect.right());

                if x_end > x_start && (x_end - x_start) > 1.0 {
                    let scan_rect = Rect::from_min_max(
                        Pos2::new(x_start, rect.top() + 3.0),
                        Pos2::new(x_end, rect.bottom() - 3.0),
                    );

                    painter.rect_filled(scan_rect, 2.0, scan_color);
                    painter.rect_stroke(
                        scan_rect,
                        2.0,
                        Stroke::new(1.0, scan_border),
                        StrokeKind::Inside,
                    );
                }
            }
        }
        DetailLevel::Sweeps => {
            // Draw sweep blocks within scans (or fall back to scan blocks if sweeps not loaded)
            for scan in timeline.scans_in_range(view_start, view_end) {
                if scan.sweeps.is_empty() {
                    // Sweeps not populated (metadata-only scan) - draw scan block instead
                    let x_start = ts_to_x(scan.start_time).max(rect.left());
                    let x_end = ts_to_x(scan.end_time).min(rect.right());

                    if x_end > x_start && (x_end - x_start) > 1.0 {
                        let scan_rect = Rect::from_min_max(
                            Pos2::new(x_start, rect.top() + 3.0),
                            Pos2::new(x_end, rect.bottom() - 3.0),
                        );

                        painter.rect_filled(scan_rect, 2.0, scan_color);
                        painter.rect_stroke(
                            scan_rect,
                            2.0,
                            Stroke::new(1.0, scan_border),
                            StrokeKind::Inside,
                        );
                    }
                } else {
                    // Draw individual sweep blocks
                    for (i, sweep) in scan.sweeps.iter().enumerate() {
                        let x_start = ts_to_x(sweep.start_time).max(rect.left());
                        let x_end = ts_to_x(sweep.end_time).min(rect.right());

                        if x_end > x_start && (x_end - x_start) > 0.5 {
                            // Alternate colors for visual distinction
                            let color = sweep_colors[i % sweep_colors.len()];

                            let sweep_rect = Rect::from_min_max(
                                Pos2::new(x_start, rect.top() + 3.0),
                                Pos2::new(x_end, rect.bottom() - 3.0),
                            );

                            painter.rect_filled(sweep_rect, 1.0, color);

                            // Draw border between sweeps if there's enough space
                            if (x_end - x_start) > 3.0 {
                                painter.rect_stroke(
                                    sweep_rect,
                                    1.0,
                                    Stroke::new(0.5, Color32::from_rgb(40, 80, 55)),
                                    StrokeKind::Inside,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn render_bottom_panel(ctx: &egui::Context, state: &mut AppState) {
    let dt = ctx.input(|i| i.stable_dt);

    // Update live mode pulse animation
    state.live_mode_state.update_pulse(dt);

    // Get current "now" time for live mode (use selected_timestamp as base, advancing in real-time)
    let now = state
        .playback_state
        .selected_timestamp
        .unwrap_or(1714564800.0);

    // Live mode state machine - automatic transitions for testing/demo
    if state.live_mode_state.is_active() {
        let elapsed = state.live_mode_state.phase_elapsed_secs(now);

        match state.live_mode_state.phase {
            LivePhase::AcquiringLock => {
                // After 5 seconds, transition to Streaming
                if elapsed >= 5.0 {
                    state.live_mode_state.start_streaming(now);
                }
            }
            LivePhase::Streaming => {
                // After 3 seconds of streaming, transition to WaitingForChunk
                if elapsed >= 3.0 {
                    state.live_mode_state.wait_for_next_chunk(now);
                }
            }
            LivePhase::WaitingForChunk => {
                // When countdown expires, transition back to Streaming
                if let Some(remaining) = state.live_mode_state.countdown_remaining_secs(now) {
                    if remaining <= 0.0 {
                        state.live_mode_state.start_streaming(now);
                    }
                }
            }
            _ => {}
        }

        // When live, playback position tracks real time exactly (1:1)
        // Advance by real dt, not playback speed
        if let Some(ts) = state.playback_state.selected_timestamp.as_mut() {
            *ts += dt as f64;
        }

        // Request continuous repaint for live mode
        ctx.request_repaint();
    }

    // Handle spacebar to toggle playback (only when no text input is focused)
    let space_pressed = ctx.input(|i| i.key_pressed(egui::Key::Space) && !i.modifiers.any());
    let has_focus = ctx.memory(|m| m.focused().is_some());
    if space_pressed && !has_focus {
        if state.playback_state.playing {
            // Stop - also exits live mode if active
            if state.live_mode_state.is_active() {
                state.live_mode_state.stop(LiveExitReason::UserStopped);
                state.status_message = state
                    .live_mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }
            state.playback_state.playing = false;
        } else {
            state.playback_state.playing = true;
        }
    }

    // Advance playback position when playing (but not in live mode - handled above)
    if state.playback_state.playing && !state.live_mode_state.is_active() {
        let advance = dt as f64
            * state
                .playback_state
                .speed
                .timeline_seconds_per_real_second();

        if let Some(ts) = state.playback_state.selected_timestamp.as_mut() {
            *ts += advance;
        }

        // Request continuous repaint while playing
        ctx.request_repaint();
    }

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
    TickConfig {
        major_interval: 365 * 24 * 3600,
        minor_divisions: 4,
        min_pixels_per_major: 60.0,
    },
    // Quarters (approximate - 91 days)
    TickConfig {
        major_interval: 91 * 24 * 3600,
        minor_divisions: 3,
        min_pixels_per_major: 60.0,
    },
    // Months (approximate - 30 days)
    TickConfig {
        major_interval: 30 * 24 * 3600,
        minor_divisions: 4,
        min_pixels_per_major: 60.0,
    },
    // Weeks
    TickConfig {
        major_interval: 7 * 24 * 3600,
        minor_divisions: 7,
        min_pixels_per_major: 60.0,
    },
    // Days
    TickConfig {
        major_interval: 24 * 3600,
        minor_divisions: 4,
        min_pixels_per_major: 60.0,
    },
    // 6 hours
    TickConfig {
        major_interval: 6 * 3600,
        minor_divisions: 6,
        min_pixels_per_major: 60.0,
    },
    // Hours
    TickConfig {
        major_interval: 3600,
        minor_divisions: 4,
        min_pixels_per_major: 60.0,
    },
    // 15 minutes
    TickConfig {
        major_interval: 15 * 60,
        minor_divisions: 3,
        min_pixels_per_major: 60.0,
    },
    // 5 minutes
    TickConfig {
        major_interval: 5 * 60,
        minor_divisions: 5,
        min_pixels_per_major: 60.0,
    },
    // 1 minute
    TickConfig {
        major_interval: 60,
        minor_divisions: 4,
        min_pixels_per_major: 60.0,
    },
    // 15 seconds
    TickConfig {
        major_interval: 15,
        minor_divisions: 3,
        min_pixels_per_major: 60.0,
    },
    // 5 seconds
    TickConfig {
        major_interval: 5,
        minor_divisions: 5,
        min_pixels_per_major: 60.0,
    },
    // 1 second
    TickConfig {
        major_interval: 1,
        minor_divisions: 4,
        min_pixels_per_major: 60.0,
    },
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
    painter.rect_filled(rect, 2.0, tl_colors::BACKGROUND);
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, tl_colors::BORDER),
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

    // Draw radar data based on zoom level
    // - Zoomed out (< 0.2 px/sec): solid fill where we have data
    // - Medium zoom (0.2 - 1.0 px/sec): show individual scan blocks
    // - Zoomed in (> 1.0 px/sec): show sweep blocks within scans
    let detail_level = if zoom < 0.2 {
        DetailLevel::Solid
    } else if zoom < 1.0 {
        DetailLevel::Scans
    } else {
        DetailLevel::Sweeps
    };

    render_radar_data(
        &painter,
        &rect,
        &state.radar_timeline,
        view_start,
        view_end,
        zoom,
        detail_level,
    );

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
                tl_colors::TICK_MAJOR
            } else {
                tl_colors::TICK_MINOR
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
                    tl_colors::TICK_LABEL,
                );
            }
        }

        tick += minor_interval;
    }

    // Draw selection marker (if user has selected a time)
    if let Some(selected_ts) = state.playback_state.selected_timestamp {
        let sel_x = ts_to_x(selected_ts);

        if sel_x >= rect.left() && sel_x <= rect.right() {
            let marker_color = tl_colors::SELECTION;

            // Selection line
            painter.line_segment(
                [
                    Pos2::new(sel_x, rect.top()),
                    Pos2::new(sel_x, rect.bottom()),
                ],
                Stroke::new(2.0, marker_color),
            );

            // Selection triangle
            let triangle = vec![
                Pos2::new(sel_x - 5.0, rect.top()),
                Pos2::new(sel_x + 5.0, rect.top()),
                Pos2::new(sel_x, rect.top() + 8.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                triangle,
                marker_color,
                Stroke::NONE,
            ));
        }
    }

    // Handle click to select time
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            // Exit live mode when user clicks timeline
            if state.live_mode_state.is_active() {
                state.live_mode_state.stop(LiveExitReason::UserSeeked);
                state.status_message = state
                    .live_mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }

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
    format!("{}.{:03}", dt.format("%Y-%m-%d %H:%M:%S"), millis)
}

fn render_playback_controls(ui: &mut egui::Ui, state: &mut AppState) {
    // Current position timestamp display
    if let Some(selected_ts) = state.playback_state.selected_timestamp {
        ui.label(
            RichText::new(format_timestamp_full(selected_ts))
                .monospace()
                .size(13.0)
                .color(tl_colors::SELECTION),
        );
        ui.separator();
    }

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
                RichText::new("\u{2022} Live")
                    .size(12.0)
                    .color(Color32::from_rgb(150, 150, 150)),
            )
            .on_hover_text("Start live streaming")
            .clicked()
        {
            // Snap to actual current time when entering live mode
            let now = current_timestamp_secs();
            state.playback_state.selected_timestamp = Some(now);
            state.live_mode_state.start(now);
            state.playback_state.playing = true;
            state.playback_state.speed = PlaybackSpeed::Realtime;
            state.status_message = "Live mode started".to_string();
        }
    }

    // Play/Stop button
    let play_text = if state.playback_state.playing {
        "\u{25A0}" // ■ Stop
    } else {
        "\u{25B6}" // ▶ Play
    };

    if ui.button(RichText::new(play_text).size(14.0)).clicked() {
        if state.playback_state.playing {
            // Stop - also exits live mode if active
            if state.live_mode_state.is_active() {
                state.live_mode_state.stop(LiveExitReason::UserStopped);
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

    // Jog amount: skip by the playback speed amount (1 second worth of playback)
    let jog_amount = state
        .playback_state
        .speed
        .timeline_seconds_per_real_second();

    // Step backward
    if ui.button(RichText::new("\u{25C0}").size(14.0)).clicked() {
        // ◀
        // Exit live mode when jogging
        if state.live_mode_state.is_active() {
            state.live_mode_state.stop(LiveExitReason::UserJogged);
            state.status_message = state
                .live_mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        }
        if let Some(ts) = state.playback_state.selected_timestamp.as_mut() {
            *ts -= jog_amount;
        }
    }

    // Step forward
    if ui.button(RichText::new("\u{25B6}").size(14.0)).clicked() {
        // ▶
        // Exit live mode when jogging
        if state.live_mode_state.is_active() {
            state.live_mode_state.stop(LiveExitReason::UserJogged);
            state.status_message = state
                .live_mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        }
        if let Some(ts) = state.playback_state.selected_timestamp.as_mut() {
            *ts += jog_amount;
        }
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

    // Push session stats to the right
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        render_session_stats(ui, &state.session_stats);
    });
}

/// Render live mode indicator badge with pulsing dot.
fn render_live_indicator(ui: &mut egui::Ui, state: &AppState) {
    let phase = state.live_mode_state.phase;
    let pulse_alpha = state.live_mode_state.pulse_alpha();

    // Get current time for status text
    let now = state
        .playback_state
        .selected_timestamp
        .unwrap_or(1714564800.0);

    match phase {
        LivePhase::AcquiringLock => {
            // Show "CONNECTING" with orange pulsing
            let pulsed_color = Color32::from_rgba_unmultiplied(
                live::ACQUIRING.r(),
                live::ACQUIRING.g(),
                live::ACQUIRING.b(),
                (128.0 + 127.0 * pulse_alpha) as u8,
            );
            ui.label(RichText::new("\u{2022}").size(16.0).color(pulsed_color));

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
            ui.label(RichText::new("\u{2022}").size(16.0).color(pulsed_color));
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
                        .color(ui_colors::VALUE),
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
fn render_session_stats(ui: &mut egui::Ui, stats: &SessionStats) {
    // Latency stats (rightmost)
    ui.label(
        RichText::new(stats.format_latency_stats())
            .size(11.0)
            .color(ui_colors::VALUE),
    );
    ui.label(RichText::new("median:").size(11.0).color(ui_colors::LABEL));

    ui.separator();

    // Cache size
    ui.label(
        RichText::new(stats.format_cache_size())
            .size(11.0)
            .color(ui_colors::VALUE),
    );
    ui.label(RichText::new("cache:").size(11.0).color(ui_colors::LABEL));

    ui.separator();

    // Transferred data
    ui.label(
        RichText::new(stats.format_transferred())
            .size(11.0)
            .color(ui_colors::VALUE),
    );
    ui.label(
        RichText::new("transferred:")
            .size(11.0)
            .color(ui_colors::LABEL),
    );

    ui.separator();

    // Request count with active indicator
    if stats.active_request_count > 0 {
        ui.label(
            RichText::new(format!("({} active)", stats.active_request_count))
                .size(11.0)
                .italics()
                .color(ui_colors::ACTIVE),
        );
    }
    ui.label(
        RichText::new(format!("{}", stats.session_request_count))
            .size(11.0)
            .color(ui_colors::VALUE),
    );
    ui.label(
        RichText::new("requests:")
            .size(11.0)
            .color(ui_colors::LABEL),
    );
}
