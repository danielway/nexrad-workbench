//! Bottom panel UI: playback controls, timeline, and session statistics.

use super::colors::{live, timeline as tl_colors, ui as ui_colors};
use crate::state::radar_data::RadarTimeline;
use crate::state::{AppState, LiveExitReason, LivePhase, PlaybackSpeed};
use chrono::{Datelike, TimeZone, Timelike, Utc};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

/// Get current Unix timestamp in seconds (works on both native and WASM)
#[allow(dead_code)] // Utility function for UI timing
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

    // Color function based on elevation angle (0-20 degrees typical range)
    // Lower elevations are darker/more blue, higher elevations are lighter/more cyan
    let elevation_to_color = |elevation: f32| -> Color32 {
        // Normalize elevation to 0-1 range (clamp to 0-20 degrees)
        let t = (elevation / 20.0).clamp(0.0, 1.0);
        // Interpolate from dark blue-green to bright cyan-green
        let r = (40.0 + t * 40.0) as u8; // 40-80
        let g = (80.0 + t * 80.0) as u8; // 80-160
        let b = (60.0 + t * 60.0) as u8; // 60-120
        Color32::from_rgb(r, g, b)
    };

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
            // Draw scan blocks as background, with sweep blocks inside
            for scan in timeline.scans_in_range(view_start, view_end) {
                let scan_x_start = ts_to_x(scan.start_time).max(rect.left());
                let scan_x_end = ts_to_x(scan.end_time).min(rect.right());

                if scan_x_end > scan_x_start && (scan_x_end - scan_x_start) > 1.0 {
                    // Always draw the scan block as background
                    let scan_rect = Rect::from_min_max(
                        Pos2::new(scan_x_start, rect.top() + 3.0),
                        Pos2::new(scan_x_end, rect.bottom() - 3.0),
                    );

                    painter.rect_filled(scan_rect, 2.0, scan_color);
                    painter.rect_stroke(
                        scan_rect,
                        2.0,
                        Stroke::new(1.0, scan_border),
                        StrokeKind::Inside,
                    );

                    // Draw individual sweep blocks inside the scan (if loaded)
                    if !scan.sweeps.is_empty() {
                        for sweep in scan.sweeps.iter() {
                            let x_start = ts_to_x(sweep.start_time).max(rect.left());
                            let x_end = ts_to_x(sweep.end_time).min(rect.right());

                            if x_end > x_start && (x_end - x_start) > 0.5 {
                                // Color based on elevation for visual distinction
                                let color = elevation_to_color(sweep.elevation);

                                // Sweeps are narrower (more inset) than the scan block
                                let sweep_rect = Rect::from_min_max(
                                    Pos2::new(x_start, rect.top() + 6.0),
                                    Pos2::new(x_end, rect.bottom() - 6.0),
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
}

pub fn render_bottom_panel(ctx: &egui::Context, state: &mut AppState) {
    let dt = ctx.input(|i| i.stable_dt);

    // Update live mode pulse animation
    state.live_mode_state.update_pulse(dt);

    // Get current "now" time for live mode (use selected_timestamp as base, advancing in real-time)
    let _now = state
        .playback_state
        .selected_timestamp
        .unwrap_or(1714564800.0);

    // When live mode is active, playback position tracks real time exactly (1:1)
    if state.live_mode_state.is_active() {
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

    // Draw selection range (if user has selected a range via shift+drag)
    if let Some((range_start, range_end)) = state.playback_state.selection_range() {
        let start_x = ts_to_x(range_start);
        let end_x = ts_to_x(range_end);

        // Only draw if visible
        if end_x >= rect.left() && start_x <= rect.right() {
            let visible_start = start_x.max(rect.left());
            let visible_end = end_x.min(rect.right());

            // Selection range highlight
            let range_rect = Rect::from_min_max(
                Pos2::new(visible_start, rect.top()),
                Pos2::new(visible_end, rect.bottom()),
            );
            painter.rect_filled(
                range_rect,
                0.0,
                Color32::from_rgba_unmultiplied(100, 150, 255, 60),
            );

            // Range boundaries
            if start_x >= rect.left() && start_x <= rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(start_x, rect.top()),
                        Pos2::new(start_x, rect.bottom()),
                    ],
                    Stroke::new(1.5, Color32::from_rgb(100, 150, 255)),
                );
            }
            if end_x >= rect.left() && end_x <= rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(end_x, rect.top()),
                        Pos2::new(end_x, rect.bottom()),
                    ],
                    Stroke::new(1.5, Color32::from_rgb(100, 150, 255)),
                );
            }
        }
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

    // Check if shift is held
    let shift_held = ui.input(|i| i.modifiers.shift);

    // Handle shift+drag to select a range
    if shift_held && response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let drag_start_ts = view_start + (pos.x - rect.left()) as f64 / zoom;
            state.playback_state.selection_start = Some(drag_start_ts);
            state.playback_state.selection_end = Some(drag_start_ts);
            state.playback_state.selection_in_progress = true;
        }
    }

    if shift_held && response.dragged() && state.playback_state.selection_in_progress {
        if let Some(pos) = response.interact_pointer_pos() {
            let current_ts = view_start + (pos.x - rect.left()) as f64 / zoom;
            state.playback_state.selection_end = Some(current_ts);
        }
    }

    if response.drag_stopped() && state.playback_state.selection_in_progress {
        state.playback_state.selection_in_progress = false;
        // Log the selection range
        if let Some((start, end)) = state.playback_state.selection_range() {
            let duration_mins = (end - start) / 60.0;
            log::info!("Selected time range: {:.0} minutes", duration_mins);
        }
    }

    // Handle click to select time (when not shift-dragging)
    if response.clicked() && !shift_held {
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

            // Clear any selection range on regular click
            state.playback_state.selection_start = None;
            state.playback_state.selection_end = None;

            // If clicked within loaded data range, also seek to that frame
            if let Some(frame) = state.playback_state.timestamp_to_frame(clicked_ts as i64) {
                state.playback_state.current_frame = frame;
            }
        }
    }

    // Handle drag to pan - NO CLAMPING, free scrolling anywhere in time
    // Only pan when not shift-dragging (selection mode)
    if response.dragged() && !shift_held && !state.playback_state.selection_in_progress {
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

/// Render the datetime picker popup for jumping to a specific time.
fn render_datetime_picker_popup(ui: &mut egui::Ui, state: &mut AppState) {
    if !state.datetime_picker.open {
        return;
    }

    let popup_id = ui.make_persistent_id("datetime_picker_popup");

    egui::Area::new(popup_id)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(280.0);

                ui.vertical(|ui| {
                    ui.heading("Jump to Date/Time (UTC)");
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
                        ui.label("UTC");
                    });

                    ui.add_space(12.0);

                    // Validation feedback
                    let valid_ts = state.datetime_picker.to_timestamp();
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
                                    state.playback_state.selected_timestamp = Some(ts);

                                    // Center timeline view on new position
                                    let view_width_secs = ui.available_width() as f64
                                        / state.playback_state.timeline_zoom;
                                    state.playback_state.timeline_view_start =
                                        ts - view_width_secs / 2.0;

                                    // Exit live mode if active
                                    if state.live_mode_state.is_active() {
                                        state.live_mode_state.stop(LiveExitReason::UserSeeked);
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

fn render_playback_controls(ui: &mut egui::Ui, state: &mut AppState) {
    // Current position timestamp display (clickable to open datetime picker)
    if let Some(selected_ts) = state.playback_state.selected_timestamp {
        let timestamp_btn = ui.add(
            egui::Button::new(
                RichText::new(format_timestamp_full(selected_ts))
                    .monospace()
                    .size(13.0)
                    .color(tl_colors::SELECTION),
            )
            .frame(false),
        );

        if timestamp_btn.clicked() {
            state.datetime_picker.init_from_timestamp(selected_ts);
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
                RichText::new("\u{2022} Live")
                    .size(12.0)
                    .color(Color32::from_rgb(150, 150, 150)),
            )
            .on_hover_text("Start live streaming")
            .clicked()
        {
            // Signal main loop to start live mode
            state.start_live_requested = true;
            state.playback_state.speed = PlaybackSpeed::Realtime;
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

    ui.separator();

    // Download selection button (enabled when a range is selected)
    let has_selection = state.playback_state.selection_range().is_some();
    let download_in_progress = state.download_selection_in_progress;

    if download_in_progress {
        ui.add_enabled(
            false,
            egui::Button::new(RichText::new("Downloading...").size(11.0)),
        );
    } else if has_selection {
        if ui
            .button(RichText::new("Download Selection").size(11.0))
            .on_hover_text("Download all scans in the selected time range")
            .clicked()
        {
            state.download_selection_requested = true;
        }
    } else {
        ui.add_enabled(
            false,
            egui::Button::new(
                RichText::new("Download Selection")
                    .size(11.0)
                    .color(Color32::GRAY),
            ),
        )
        .on_hover_text("Drag on timeline to select a range to download");
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
fn render_session_stats(ui: &mut egui::Ui, state: &mut AppState) {
    let stats = &state.session_stats;

    // Latency stats (rightmost)
    ui.label(
        RichText::new(stats.format_latency_stats())
            .size(11.0)
            .color(ui_colors::VALUE),
    );
    ui.label(RichText::new("median:").size(11.0).color(ui_colors::LABEL));

    ui.separator();

    // Cache size with clear button
    if ui.small_button("x").on_hover_text("Clear cache").clicked() {
        state.clear_cache_requested = true;
    }
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
