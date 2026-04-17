//! Timeline rendering: time ruler, scan/sweep tracks, tooltip, and overlays.

mod interaction;
mod overlays;
mod ruler;
mod scan_track;
mod strokes;
mod sweep_track;
mod tooltips;

use super::colors::timeline as tl_colors;
use crate::state::AppState;
use chrono::{Datelike, TimeZone, Timelike, Utc};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use interaction::handle_timeline_interaction;
use overlays::{render_download_ghosts, render_realtime_progress, render_saved_events};
use ruler::{render_playback_cursor, render_tick_marks};
use scan_track::{render_scan_track, render_shadow_boundaries};
use sweep_track::{render_connector_lines, render_sweep_track};
use tooltips::render_timeline_tooltip;

/// Get current Unix timestamp in seconds.
pub(super) fn current_timestamp_secs() -> f64 {
    js_sys::Date::now() / 1000.0
}

/// Level of detail for radar data rendering
#[derive(Clone, Copy, PartialEq)]
pub(super) enum DetailLevel {
    /// Just show solid color where data exists
    Solid,
    /// Show individual scan blocks
    Scans,
    /// Show sweep blocks within scans
    Sweeps,
}

/// Time intervals for tick marks, from coarsest to finest
#[derive(Clone, Copy)]
pub(super) struct TickConfig {
    /// Interval in seconds for major ticks
    pub(super) major_interval: i64,
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

pub(super) fn select_tick_config(zoom: f64) -> &'static TickConfig {
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

/// Date/time components extracted from a Unix timestamp.
pub(super) struct DateTimeComponents {
    pub(super) year: i32,
    pub(super) month: u32,
    pub(super) day: u32,
    pub(super) hour: u32,
    pub(super) minute: u32,
    pub(super) second: u32,
}

impl DateTimeComponents {
    pub(super) fn from_timestamp(timestamp: i64, use_local: bool) -> Self {
        if use_local {
            let d = js_sys::Date::new_0();
            d.set_time((timestamp as f64) * 1000.0);
            Self {
                year: d.get_full_year() as i32,
                month: d.get_month() + 1, // JS months are 0-based
                day: d.get_date(),
                hour: d.get_hours(),
                minute: d.get_minutes(),
                second: d.get_seconds(),
            }
        } else {
            let dt = Utc.timestamp_opt(timestamp, 0).unwrap();
            Self {
                year: dt.year(),
                month: dt.month(),
                day: dt.day(),
                hour: dt.hour(),
                minute: dt.minute(),
                second: dt.second(),
            }
        }
    }

    pub(super) fn month_abbrev(&self) -> &'static str {
        match self.month {
            1 => "Jan",
            2 => "Feb",
            3 => "Mar",
            4 => "Apr",
            5 => "May",
            6 => "Jun",
            7 => "Jul",
            8 => "Aug",
            9 => "Sep",
            10 => "Oct",
            11 => "Nov",
            12 => "Dec",
            _ => "???",
        }
    }
}

pub(super) fn format_timestamp(
    timestamp: i64,
    tick_config: &TickConfig,
    use_local: bool,
) -> String {
    let dt = DateTimeComponents::from_timestamp(timestamp, use_local);
    let interval = tick_config.major_interval;

    if interval >= 30 * 24 * 3600 {
        if interval >= 365 * 24 * 3600 {
            format!("{}", dt.year)
        } else {
            format!("{} {}", dt.month_abbrev(), dt.year)
        }
    } else if interval >= 24 * 3600 {
        format!("{} {:02}", dt.month_abbrev(), dt.day)
    } else if interval >= 60 {
        format!("{:02}:{:02}", dt.hour, dt.minute)
    } else {
        format!("{:02}:{:02}:{:02}", dt.hour, dt.minute, dt.second)
    }
}

/// Format a timestamp (f64 unix seconds) for display with sub-second precision
pub(super) fn format_timestamp_full(ts: f64, use_local: bool) -> String {
    let mut secs = ts.floor() as i64;
    let mut millis = ((ts.fract()) * 1000.0).round() as u32;
    if millis >= 1000 {
        millis -= 1000;
        secs += 1;
    }
    let dt = DateTimeComponents::from_timestamp(secs, use_local);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, millis
    )
}

pub(super) fn render_timeline(ui: &mut egui::Ui, state: &mut AppState) {
    let use_local = state.use_local_time;
    let available_width = ui.available_width() as f64;
    state.playback_state.timeline_width_px = available_width;

    let zoom = state.playback_state.timeline_zoom;
    let detail_level = if zoom < 0.2 {
        DetailLevel::Solid
    } else if zoom < 1.0 {
        DetailLevel::Scans
    } else {
        DetailLevel::Sweeps
    };

    // Track heights — timestamp lane sits above the scan track so labels
    // never overlap scan block content.  Sweep track only at Sweeps detail.
    let tick_lane_h: f32 = 12.0; // dedicated lane for time tick labels
    let scan_track_h: f32 = if detail_level == DetailLevel::Sweeps {
        20.0
    } else {
        24.0
    };
    let sweep_track_h: f32 = if detail_level == DetailLevel::Sweeps {
        20.0
    } else {
        0.0
    };
    let separator_h: f32 = if detail_level == DetailLevel::Sweeps {
        1.0
    } else {
        0.0
    };
    let timeline_height = tick_lane_h + scan_track_h + separator_h + sweep_track_h;

    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_width as f32, timeline_height),
        Sense::click_and_drag(),
    );
    let full_rect = response.rect;

    // Sub-rects for each track: tick lane -> scan track -> sweep track
    let tick_rect = Rect::from_min_max(
        full_rect.min,
        Pos2::new(full_rect.max.x, full_rect.min.y + tick_lane_h),
    );
    let scan_rect = Rect::from_min_max(
        Pos2::new(full_rect.min.x, tick_rect.max.y),
        Pos2::new(full_rect.max.x, tick_rect.max.y + scan_track_h),
    );
    let sweep_rect = if detail_level == DetailLevel::Sweeps {
        Rect::from_min_max(
            Pos2::new(full_rect.min.x, scan_rect.max.y + separator_h),
            Pos2::new(
                full_rect.max.x,
                scan_rect.max.y + separator_h + sweep_track_h,
            ),
        )
    } else {
        Rect::NOTHING // not used
    };

    let dark = state.is_dark;

    // Background for scan track
    painter.rect_filled(scan_rect, 2.0, tl_colors::background(dark));
    painter.rect_stroke(
        scan_rect,
        2.0,
        Stroke::new(1.0, tl_colors::border(dark)),
        StrokeKind::Outside,
    );

    // Background for sweep track (when visible)
    if detail_level == DetailLevel::Sweeps {
        painter.rect_filled(sweep_rect, 0.0, tl_colors::background(dark));
        painter.rect_stroke(
            sweep_rect,
            0.0,
            Stroke::new(0.5, tl_colors::border(dark)),
            StrokeKind::Outside,
        );
        // Separator line
        painter.line_segment(
            [
                Pos2::new(full_rect.left(), scan_rect.bottom()),
                Pos2::new(full_rect.right(), scan_rect.bottom()),
            ],
            Stroke::new(0.5, tl_colors::track_separator()),
        );
    }

    if zoom <= 0.0 {
        return;
    }

    let view_start = state.playback_state.timeline_view_start;
    let visible_secs = available_width / zoom;
    let view_end = view_start + visible_secs;

    // Use scan_rect for ts_to_x since it spans the full width
    let ts_to_x = |ts: f64| -> f32 { scan_rect.left() + ((ts - view_start) * zoom) as f32 };

    let active_sweep = if detail_level == DetailLevel::Sweeps {
        match (
            state.viz_state.displayed_scan_timestamp,
            state.viz_state.displayed_sweep_elevation_number,
        ) {
            (Some(ts), Some(en)) => Some((ts, en)),
            _ => None,
        }
    } else {
        None
    };

    // -- Render shadow scan boundaries from archive index --
    if !state.shadow_scan_boundaries.is_empty() {
        render_shadow_boundaries(
            &painter,
            &scan_rect,
            &state.shadow_scan_boundaries,
            &state.radar_timeline,
            view_start,
            view_end,
            zoom,
            detail_level,
        );
    }

    // -- Render scan track --
    // Extract the scan key timestamp (seconds) for the active real-time volume
    // so we can skip it in normal timeline rendering.
    let active_scan_key_ts: Option<f64> = state.live_radar_model.volume.as_ref().and_then(|v| {
        v.scan_key.as_ref().and_then(|key| {
            // Scan key format: "SITE|TIMESTAMP_MS"
            key.split('|')
                .nth(1)?
                .parse::<i64>()
                .ok()
                .map(|ms| ms as f64 / 1000.0)
        })
    });
    render_scan_track(
        &painter,
        &scan_rect,
        &state.radar_timeline,
        view_start,
        view_end,
        zoom,
        detail_level,
        active_scan_key_ts,
    );

    // -- Render sweep track (only at Sweeps detail) --
    let prev_active_sweep = if state.effective_sweep_animation() {
        match (
            state.viz_state.prev_sweep_scan_timestamp,
            state.viz_state.prev_sweep_elevation_number,
        ) {
            (Some(ts), Some(en)) => Some((ts, en)),
            _ => None,
        }
    } else {
        None
    };
    if detail_level == DetailLevel::Sweeps {
        render_sweep_track(
            &painter,
            &sweep_rect,
            &state.radar_timeline,
            view_start,
            view_end,
            zoom,
            active_sweep,
            state.viz_state.elevation_selection.elevation_number(),
            active_scan_key_ts,
            prev_active_sweep,
        );
        render_connector_lines(
            &painter,
            &scan_rect,
            &sweep_rect,
            &state.radar_timeline,
            view_start,
            view_end,
            zoom,
        );
    }

    // -- Render ghost markers for pending downloads --
    if state.download_progress.is_active() {
        let anim_time = ui.ctx().input(|i| i.time);
        render_download_ghosts(
            &painter,
            &scan_rect,
            &state.download_progress,
            &state.radar_timeline,
            view_start,
            view_end,
            zoom,
            detail_level,
            anim_time,
        );
        ui.ctx().request_repaint();
    }

    // -- Render real-time partial scan progress --
    // Compute `now` once per frame so render + tooltip use a consistent boundary.
    let frame_now_secs = js_sys::Date::now() / 1000.0;
    if let Some(ref position) = state.live_radar_model.position {
        let anim_time = ui.ctx().input(|i| i.time);
        let overlay_ctx = overlays::LiveOverlayContext {
            countdown_secs: state
                .live_mode_state
                .countdown_remaining_secs(frame_now_secs),
            chunk_interval_secs: state.live_mode_state.chunk_interval_secs,
            in_progress_radials: state
                .live_mode_state
                .current_in_progress_radials
                .unwrap_or(0),
            elevations_received: state.live_mode_state.elevations_received.clone(),
            in_progress_elevation: state.live_mode_state.current_in_progress_elevation,
        };
        render_realtime_progress(
            &painter,
            &scan_rect,
            if detail_level == DetailLevel::Sweeps {
                Some(&sweep_rect)
            } else {
                None
            },
            position,
            &overlay_ctx,
            view_start,
            view_end,
            zoom,
            anim_time,
            frame_now_secs,
            state.viz_state.elevation_selection.elevation_number(),
            active_sweep,
            prev_active_sweep,
        );
        ui.ctx().request_repaint();
    }

    // VCP track removed — the scan lane already represents VCP via color.

    // Select appropriate tick configuration
    let tick_config = select_tick_config(zoom);
    let major_interval = tick_config.major_interval;
    let minor_interval = (major_interval / tick_config.minor_divisions as i64).max(1);

    // When displaying local time, align ticks to local boundaries instead of
    // UTC boundaries.  For example, day ticks should land on local midnight,
    // not UTC midnight.  We obtain the browser's timezone offset and shift
    // into local seconds for alignment, then shift back to UTC for plotting.
    let tz_offset_secs: i64 = if use_local {
        let d = js_sys::Date::new_0();
        d.set_time(view_start * 1000.0);
        // getTimezoneOffset() returns minutes positive-west; convert to
        // seconds-east so that local = utc + tz_offset_secs.
        -(d.get_timezone_offset() as i64) * 60
    } else {
        0
    };

    let local_start = view_start as i64 + tz_offset_secs;
    let local_end = view_end as i64 + tz_offset_secs;
    let first_tick = (local_start / minor_interval) * minor_interval - tz_offset_secs;
    let last_tick = ((local_end / minor_interval) + 1) * minor_interval - tz_offset_secs;

    // Draw tick marks and labels in the dedicated tick lane above the scan track
    render_tick_marks(
        &painter,
        &tick_rect,
        first_tick,
        last_tick,
        minor_interval,
        major_interval,
        tz_offset_secs,
        tick_config,
        dark,
        use_local,
        view_start,
        zoom,
    );

    // The overlay_rect spans all data tracks (scan + sweep, not VCP)
    let overlay_rect = Rect::from_min_max(
        scan_rect.min,
        Pos2::new(
            scan_rect.max.x,
            if detail_level == DetailLevel::Sweeps {
                sweep_rect.max.y
            } else {
                scan_rect.max.y
            },
        ),
    );

    // Draw saved event overlays (behind the selection range)
    render_saved_events(
        &painter,
        &overlay_rect,
        &state.saved_events,
        &state.viz_state.site_id,
        view_start,
        zoom,
    );

    // Draw selection range (if user has selected a range via shift+drag)
    if let Some((range_start, range_end)) = state.playback_state.selection_range() {
        let start_x = ts_to_x(range_start);
        let end_x = ts_to_x(range_end);

        if end_x >= overlay_rect.left() && start_x <= overlay_rect.right() {
            let visible_start = start_x.max(overlay_rect.left());
            let visible_end = end_x.min(overlay_rect.right());

            let range_rect = Rect::from_min_max(
                Pos2::new(visible_start, overlay_rect.top()),
                Pos2::new(visible_end, overlay_rect.bottom()),
            );
            painter.rect_filled(
                range_rect,
                0.0,
                Color32::from_rgba_unmultiplied(100, 150, 255, 40),
            );

            if start_x >= overlay_rect.left() && start_x <= overlay_rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(start_x, overlay_rect.top()),
                        Pos2::new(start_x, overlay_rect.bottom()),
                    ],
                    Stroke::new(1.5, Color32::from_rgb(100, 150, 255)),
                );
            }
            if end_x >= overlay_rect.left() && end_x <= overlay_rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(end_x, overlay_rect.top()),
                        Pos2::new(end_x, overlay_rect.bottom()),
                    ],
                    Stroke::new(1.5, Color32::from_rgb(100, 150, 255)),
                );
            }
        }
    }

    // Draw selection marker (playback cursor) and "now" marker
    render_playback_cursor(
        &painter,
        &overlay_rect,
        state.playback_state.playback_position(),
        view_start,
        zoom,
    );

    // Draw selection range labels (boundaries and duration)
    if let Some((range_start, range_end)) = state.playback_state.selection_range() {
        let start_x = ts_to_x(range_start);
        let end_x = ts_to_x(range_end);

        if end_x >= scan_rect.left() && start_x <= scan_rect.right() {
            let label_color = tl_colors::SELECTION_LABEL;
            let duration_secs = range_end - range_start;
            let duration_text = if duration_secs < 60.0 {
                format!("{:.0}s", duration_secs)
            } else if duration_secs < 3600.0 {
                format!("{:.1}m", duration_secs / 60.0)
            } else {
                format!("{:.1}h", duration_secs / 3600.0)
            };

            let center_x =
                ((start_x + end_x) / 2.0).clamp(scan_rect.left() + 20.0, scan_rect.right() - 20.0);
            painter.text(
                Pos2::new(center_x, scan_rect.top() + 3.0),
                egui::Align2::CENTER_TOP,
                &duration_text,
                egui::FontId::monospace(8.0),
                label_color,
            );

            let tick_config_sel = select_tick_config(zoom);
            if (end_x - start_x) > 100.0 {
                let start_label = format_timestamp(range_start as i64, tick_config_sel, use_local);
                let end_label = format_timestamp(range_end as i64, tick_config_sel, use_local);
                if start_x >= scan_rect.left() && start_x <= scan_rect.right() {
                    painter.text(
                        Pos2::new(start_x + 2.0, scan_rect.bottom() - 2.0),
                        egui::Align2::LEFT_BOTTOM,
                        &start_label,
                        egui::FontId::monospace(7.0),
                        label_color,
                    );
                }
                if end_x >= scan_rect.left() && end_x <= scan_rect.right() {
                    painter.text(
                        Pos2::new(end_x - 2.0, scan_rect.bottom() - 2.0),
                        egui::Align2::RIGHT_BOTTOM,
                        &end_label,
                        egui::FontId::monospace(7.0),
                        label_color,
                    );
                }
            }
        }
    }

    // -- Hover tooltips --
    if response.hovered() {
        if let Some(hover_pos) = response.hover_pos() {
            let hover_ts = view_start + (hover_pos.x - full_rect.left()) as f64 / zoom;
            render_timeline_tooltip(
                ui,
                &state.radar_timeline,
                state,
                hover_ts,
                hover_pos,
                &scan_rect,
                &sweep_rect,
                detail_level,
                use_local,
                frame_now_secs,
            );
        }
    }

    // -- Interaction handling --
    handle_timeline_interaction(ui, state, &response, &full_rect, view_start, zoom);
}
