//! Timeline rendering: time ruler, scan/sweep tracks, tooltip, and overlays.

use super::colors::timeline as tl_colors;
use crate::data::ScanCompleteness;
use crate::state::radar_data::RadarTimeline;
use crate::state::{AppState, LiveExitReason, SavedEvents};
use chrono::{Datelike, TimeZone, Timelike, Utc};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

/// Get current Unix timestamp in seconds.
fn current_timestamp_secs() -> f64 {
    js_sys::Date::now() / 1000.0
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

/// Render scan blocks on the scan track (warm palette, VCP-based colors).
#[allow(clippy::too_many_arguments)]
fn render_scan_track(
    painter: &Painter,
    rect: &Rect,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
    active_scan_key_ts: Option<f64>,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    match detail_level {
        DetailLevel::Solid => {
            // Draw solid regions for each contiguous time range
            for range in timeline.time_ranges() {
                let x_start = ts_to_x(range.start).max(rect.left());
                let x_end = ts_to_x(range.end).min(rect.right());

                // Enforce minimum visual width for sub-pixel data regions
                let x_end = if (x_end - x_start) > 0.0 && (x_end - x_start) < 8.0 {
                    (x_start + 8.0).min(rect.right())
                } else {
                    x_end
                };

                if x_end > x_start {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(x_start, rect.top() + 2.0),
                            Pos2::new(x_end, rect.bottom() - 2.0),
                        ),
                        2.0,
                        tl_colors::scan_fill(0, None),
                    );
                }
            }
        }
        DetailLevel::Scans | DetailLevel::Sweeps => {
            for scan in timeline.scans_in_range(view_start, view_end) {
                // Skip the scan that corresponds to the active real-time volume —
                // render_realtime_progress draws it with received/projected styling.
                if let Some(key_ts) = active_scan_key_ts {
                    if (scan.key_timestamp - key_ts).abs() < 0.5 {
                        continue;
                    }
                }
                let x_start = ts_to_x(scan.start_time).max(rect.left());
                let x_end = ts_to_x(scan.end_time).min(rect.right());
                let width = x_end - x_start;

                if width < 1.0 {
                    continue;
                }

                let scan_rect = Rect::from_min_max(
                    Pos2::new(x_start, rect.top() + 2.0),
                    Pos2::new(x_end, rect.bottom() - 2.0),
                );

                let fill = tl_colors::scan_fill(scan.vcp, scan.completeness);
                let border = tl_colors::scan_border(scan.vcp, scan.completeness);

                // Missing: outline only with dashed effect
                if scan.completeness == Some(ScanCompleteness::Missing) {
                    painter.rect_stroke(
                        scan_rect,
                        2.0,
                        Stroke::new(1.0, border),
                        StrokeKind::Inside,
                    );
                } else {
                    painter.rect_filled(scan_rect, 2.0, fill);
                    painter.rect_stroke(
                        scan_rect,
                        2.0,
                        Stroke::new(1.0, border),
                        StrokeKind::Inside,
                    );

                    // Hatch pattern for PartialWithVcp
                    if scan.completeness == Some(ScanCompleteness::PartialWithVcp) {
                        let hatch_color = tl_colors::scan_hatch(scan.vcp);
                        let spacing = 6.0;
                        let h = scan_rect.height();
                        // Use global x-coordinate phase so hatch lines are parallel across all blocks
                        let phase = scan_rect.left() % spacing;
                        let mut offset = -phase;
                        while offset < width + h {
                            // Unclipped 45° diagonal: top to bottom, shifting left by h
                            let x0 = scan_rect.left() + offset;
                            let x1 = x0 - h;
                            // Clip to rect: adjust y when x is clamped to preserve angle
                            let (cx0, cy0) = if x0 > scan_rect.right() {
                                (
                                    scan_rect.right(),
                                    scan_rect.top() + (x0 - scan_rect.right()),
                                )
                            } else {
                                (x0, scan_rect.top())
                            };
                            let (cx1, cy1) = if x1 < scan_rect.left() {
                                (
                                    scan_rect.left(),
                                    scan_rect.bottom() - (scan_rect.left() - x1),
                                )
                            } else {
                                (x1, scan_rect.bottom())
                            };
                            if cy0 < cy1 {
                                painter.line_segment(
                                    [Pos2::new(cx0, cy0), Pos2::new(cx1, cy1)],
                                    Stroke::new(0.5, hatch_color),
                                );
                            }
                            offset += spacing;
                        }
                    }

                    // PartialNoVcp: draw dashed border on top of filled rect
                    if scan.completeness == Some(ScanCompleteness::PartialNoVcp) {
                        // Already drew solid border above; the reduced alpha handles visual distinction
                    }
                }

                // Single combined label: "VCP 212 15/17" — centered in block
                // Only show when the block is wide enough to avoid overlap with
                // neighboring blocks and time tick labels.
                if width > 60.0 && scan.vcp > 0 {
                    let is_partial = matches!(
                        (scan.present_records, scan.expected_records),
                        (Some(p), Some(e)) if e > 0 && p < e
                    );
                    let label = if is_partial {
                        let (p, e) = (
                            scan.present_records.unwrap(),
                            scan.expected_records.unwrap(),
                        );
                        if width > 120.0 {
                            format!("VCP {} {}/{}", scan.vcp, p, e)
                        } else {
                            format!("{} {}/{}", scan.vcp, p, e)
                        }
                    } else if width > 100.0 {
                        let elev_count = scan
                            .vcp_pattern
                            .as_ref()
                            .map(|v| v.elevations.len())
                            .unwrap_or(scan.sweeps.len());
                        if elev_count > 0 {
                            format!("VCP {} ({})", scan.vcp, elev_count)
                        } else {
                            format!("VCP {}", scan.vcp)
                        }
                    } else {
                        format!("{}", scan.vcp)
                    };
                    painter.text(
                        scan_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::monospace(7.0),
                        Color32::from_rgba_unmultiplied(220, 220, 240, 180),
                    );
                }
            }
        }
    }
}

/// Render sweep blocks on the sweep track (cool indigo-to-cyan palette).
#[allow(clippy::too_many_arguments)]
fn render_sweep_track(
    painter: &Painter,
    rect: &Rect,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    active_sweep: Option<(i64, u8)>,
    target_elevation: f32,
    active_scan_key_ts: Option<f64>,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    for scan in timeline.scans_in_range(view_start, view_end) {
        // Skip the scan that corresponds to the active real-time volume —
        // render_realtime_progress owns all sweeps for the in-progress volume.
        if let Some(key_ts) = active_scan_key_ts {
            if (scan.key_timestamp - key_ts).abs() < 0.5 {
                continue;
            }
        }
        if scan.sweeps.is_empty() {
            continue;
        }

        let vcp_elevations = scan.vcp_pattern.as_ref().map(|v| &v.elevations);

        for sweep in scan.sweeps.iter() {
            let x_start = ts_to_x(sweep.start_time).max(rect.left());
            let x_end = ts_to_x(sweep.end_time).min(rect.right());
            let width = x_end - x_start;

            if width < 0.5 {
                continue;
            }

            let matches_elevation = (sweep.elevation - target_elevation).abs() < 0.3;
            let is_active = active_sweep.is_some_and(|(scan_ts, elev_num)| {
                scan.key_timestamp as i64 == scan_ts && sweep.elevation_number == elev_num
            });

            let fill = tl_colors::sweep_fill(sweep.elevation, matches_elevation);
            let border = tl_colors::sweep_border(sweep.elevation, is_active);

            let sweep_rect = Rect::from_min_max(
                Pos2::new(x_start, rect.top() + 2.0),
                Pos2::new(x_end, rect.bottom() - 2.0),
            );

            painter.rect_filled(sweep_rect, 1.0, fill);

            if width > 3.0 {
                let stroke_width = if is_active { 2.0 } else { 0.5 };
                let stroke_kind = if is_active {
                    StrokeKind::Outside
                } else {
                    StrokeKind::Inside
                };
                painter.rect_stroke(
                    sweep_rect,
                    1.0,
                    Stroke::new(stroke_width, border),
                    stroke_kind,
                );
            }

            // Elevation + product labels
            if width > 25.0 {
                let mut label = if width > 60.0 {
                    format!("E{} {:.1}\u{00B0}", sweep.elevation_number, sweep.elevation)
                } else {
                    format!("{:.1}", sweep.elevation)
                };

                if width > 80.0 {
                    if let Some(elevs) = vcp_elevations {
                        if let Some(vcp_elev) =
                            elevs.get(sweep.elevation_number.saturating_sub(1) as usize)
                        {
                            let products = match vcp_elev.waveform.as_str() {
                                "CS" | "ContiguousSurveillance" => "R",
                                "CDW"
                                | "CDWO"
                                | "ContiguousDopplerWithGating"
                                | "ContiguousDopplerWithoutGating" => "V",
                                "B" | "Batch" => "R/V",
                                "SPP" | "StaggeredPulsePair" => "R/V/D",
                                _ => "",
                            };
                            if !products.is_empty() {
                                label.push_str(&format!(" {}", products));
                            }
                        }
                    }
                }

                painter.text(
                    sweep_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::monospace(8.0),
                    Color32::from_rgba_unmultiplied(220, 230, 255, 180),
                );
            }
        }
    }
}

/// Draw thin connector lines from scan boundaries into the sweep track.
fn render_connector_lines(
    painter: &Painter,
    scan_rect: &Rect,
    sweep_rect: &Rect,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
) {
    let ts_to_x = |ts: f64| -> f32 { scan_rect.left() + ((ts - view_start) * zoom) as f32 };

    for scan in timeline.scans_in_range(view_start, view_end) {
        if scan.sweeps.is_empty() {
            continue;
        }
        for ts in [scan.start_time, scan.end_time] {
            let x = ts_to_x(ts);
            if x >= scan_rect.left() && x <= scan_rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(x, scan_rect.bottom()),
                        Pos2::new(x, sweep_rect.top() + 2.0),
                    ],
                    Stroke::new(0.5, tl_colors::connector()),
                );
            }
        }
    }
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

/// Date/time components extracted from a Unix timestamp.
struct DateTimeComponents {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

impl DateTimeComponents {
    fn from_timestamp(timestamp: i64, use_local: bool) -> Self {
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

    fn month_abbrev(&self) -> &'static str {
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

fn format_timestamp(timestamp: i64, tick_config: &TickConfig, use_local: bool) -> String {
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

    // Sub-rects for each track: tick lane → scan track → sweep track
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
            state.displayed_scan_timestamp,
            state.displayed_sweep_elevation_number,
        ) {
            (Some(ts), Some(en)) => Some((ts, en)),
            _ => None,
        }
    } else {
        None
    };

    // ── Render shadow scan boundaries from archive index ───────────────
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

    // ── Render scan track ─────────────────────────────────────────────
    // Extract the scan key timestamp (seconds) for the active real-time volume
    // so we can skip it in normal timeline rendering.
    let active_scan_key_ts: Option<f64> = if state.live_mode_state.is_active() {
        state
            .live_mode_state
            .current_scan_key
            .as_ref()
            .and_then(|key| {
                // Scan key format: "SITE|TIMESTAMP_MS"
                key.split('|')
                    .nth(1)?
                    .parse::<i64>()
                    .ok()
                    .map(|ms| ms as f64 / 1000.0)
            })
    } else {
        None
    };
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

    // ── Render sweep track (only at Sweeps detail) ────────────────────
    if detail_level == DetailLevel::Sweeps {
        render_sweep_track(
            &painter,
            &sweep_rect,
            &state.radar_timeline,
            view_start,
            view_end,
            zoom,
            active_sweep,
            state.viz_state.target_elevation,
            active_scan_key_ts,
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

    // ── Render ghost markers for pending downloads ────────────────────
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

    // ── Render real-time partial scan progress ────────────────────────
    // Compute `now` once per frame so render + tooltip use a consistent boundary.
    let frame_now_secs = js_sys::Date::now() / 1000.0;
    if state.live_mode_state.is_active() {
        let anim_time = ui.ctx().input(|i| i.time);
        render_realtime_progress(
            &painter,
            &scan_rect,
            if detail_level == DetailLevel::Sweeps {
                Some(&sweep_rect)
            } else {
                None
            },
            &state.live_mode_state,
            view_start,
            view_end,
            zoom,
            anim_time,
            frame_now_secs,
            state.viz_state.target_elevation,
            active_sweep,
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

    // ── Hover tooltips ────────────────────────────────────────────────
    if response.hovered() {
        if let Some(hover_pos) = response.hover_pos() {
            let hover_ts = view_start + (hover_pos.x - full_rect.left()) as f64 / zoom;
            render_timeline_tooltip(
                ui,
                &state.radar_timeline,
                &state.live_mode_state,
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

    // ── Interaction handling ──────────────────────────────────────────
    handle_timeline_interaction(ui, state, &response, &full_rect, view_start, zoom);
}

/// Draw tick marks (major + minor) and labels in the tick lane.
#[allow(clippy::too_many_arguments)]
fn render_tick_marks(
    painter: &Painter,
    tick_rect: &Rect,
    first_tick: i64,
    last_tick: i64,
    minor_interval: i64,
    major_interval: i64,
    tz_offset_secs: i64,
    tick_config: &TickConfig,
    dark: bool,
    use_local: bool,
    view_start: f64,
    zoom: f64,
) {
    let ts_to_x = |ts: f64| -> f32 { tick_rect.left() + ((ts - view_start) * zoom) as f32 };

    let mut tick = first_tick;
    while tick <= last_tick {
        let x = ts_to_x(tick as f64);

        if x >= tick_rect.left() && x <= tick_rect.right() {
            let local_tick = tick + tz_offset_secs;
            let is_major = local_tick % major_interval == 0;
            let tick_height = if is_major { 4.0 } else { 2.0 };
            let tick_color = if is_major {
                tl_colors::tick_major(dark)
            } else {
                tl_colors::tick_minor(dark)
            };

            // Tick mark hangs down from the bottom of the tick lane
            painter.line_segment(
                [
                    Pos2::new(x, tick_rect.bottom() - tick_height),
                    Pos2::new(x, tick_rect.bottom()),
                ],
                Stroke::new(1.0, tick_color),
            );

            // Label for major ticks — above tick marks
            if is_major {
                let label = format_timestamp(tick, tick_config, use_local);
                painter.text(
                    Pos2::new(x, tick_rect.bottom() - tick_height),
                    egui::Align2::CENTER_BOTTOM,
                    label,
                    egui::FontId::monospace(8.0),
                    tl_colors::tick_label(dark),
                );
            }
        }

        tick += minor_interval;
    }
}

/// Draw the playback position cursor (selection marker) and "now" wall-clock marker.
fn render_playback_cursor(
    painter: &Painter,
    overlay_rect: &Rect,
    selected_ts: f64,
    view_start: f64,
    zoom: f64,
) {
    let ts_to_x = |ts: f64| -> f32 { overlay_rect.left() + ((ts - view_start) * zoom) as f32 };

    // Selection marker (playback position indicator)
    {
        let sel_x = ts_to_x(selected_ts);

        if sel_x >= overlay_rect.left() && sel_x <= overlay_rect.right() {
            let marker_color = tl_colors::SELECTION;

            painter.line_segment(
                [
                    Pos2::new(sel_x, overlay_rect.top()),
                    Pos2::new(sel_x, overlay_rect.bottom()),
                ],
                Stroke::new(2.0, marker_color),
            );

            let triangle = vec![
                Pos2::new(sel_x - 5.0, overlay_rect.top()),
                Pos2::new(sel_x + 5.0, overlay_rect.top()),
                Pos2::new(sel_x, overlay_rect.top() + 8.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                triangle,
                marker_color,
                Stroke::NONE,
            ));
        }
    }

    // "Now" marker (current wall-clock time)
    {
        let now_ts = current_timestamp_secs();
        let now_x = ts_to_x(now_ts);

        if now_x >= overlay_rect.left() && now_x <= overlay_rect.right() {
            let now_color = tl_colors::NOW_MARKER;

            painter.line_segment(
                [
                    Pos2::new(now_x, overlay_rect.top()),
                    Pos2::new(now_x, overlay_rect.top() + 4.0),
                ],
                Stroke::new(1.5, now_color),
            );
            painter.line_segment(
                [
                    Pos2::new(now_x, overlay_rect.bottom() - 4.0),
                    Pos2::new(now_x, overlay_rect.bottom()),
                ],
                Stroke::new(1.5, now_color),
            );
            painter.line_segment(
                [
                    Pos2::new(now_x, overlay_rect.top() + 4.0),
                    Pos2::new(now_x, overlay_rect.bottom() - 4.0),
                ],
                Stroke::new(
                    0.5,
                    Color32::from_rgba_unmultiplied(
                        now_color.r(),
                        now_color.g(),
                        now_color.b(),
                        100,
                    ),
                ),
            );
            let d = 3.0;
            let diamond = vec![
                Pos2::new(now_x, overlay_rect.bottom() - d),
                Pos2::new(now_x + d, overlay_rect.bottom()),
                Pos2::new(now_x, overlay_rect.bottom() + d),
                Pos2::new(now_x - d, overlay_rect.bottom()),
            ];
            painter.add(egui::Shape::convex_polygon(
                diamond,
                now_color,
                Stroke::NONE,
            ));
        }
    }
}

/// Handle mouse interaction on the timeline: click, shift+click, drag-to-pan, scroll-to-zoom.
fn handle_timeline_interaction(
    ui: &mut egui::Ui,
    state: &mut AppState,
    response: &egui::Response,
    full_rect: &Rect,
    view_start: f64,
    zoom: f64,
) {
    let shift_held = ui.input(|i| i.modifiers.shift);

    if shift_held && response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let clicked_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
            let current_pos = state.playback_state.playback_position();
            state.playback_state.selection_start = Some(current_pos);
            state.playback_state.selection_end = Some(clicked_ts);
            state.playback_state.apply_selection_as_bounds();
            let duration_mins = (clicked_ts - current_pos).abs() / 60.0;
            log::info!("Shift+click range: {:.0} minutes", duration_mins);
        }
    }

    if shift_held && response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let drag_start_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
            state.playback_state.selection_start = Some(drag_start_ts);
            state.playback_state.selection_end = Some(drag_start_ts);
            state.playback_state.selection_in_progress = true;
        }
    }

    if shift_held && response.dragged() && state.playback_state.selection_in_progress {
        if let Some(pos) = response.interact_pointer_pos() {
            let current_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;
            state.playback_state.selection_end = Some(current_ts);
        }
    }

    if response.drag_stopped() && state.playback_state.selection_in_progress {
        state.playback_state.selection_in_progress = false;
        if let Some((start, end)) = state.playback_state.selection_range() {
            let duration_mins = (end - start) / 60.0;
            log::info!("Selected time range: {:.0} minutes", duration_mins);
            state.playback_state.apply_selection_as_bounds();
        }
    }

    if response.clicked() && !shift_held {
        if let Some(pos) = response.interact_pointer_pos() {
            if state.live_mode_state.is_active() {
                state.live_mode_state.stop(LiveExitReason::UserSeeked);
                state.playback_state.time_model.disable_realtime_lock();
                state.status_message = state
                    .live_mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }

            let clicked_ts = view_start + (pos.x - full_rect.left()) as f64 / zoom;

            let snap_dist_secs = 10.0 / zoom;
            let snapped_ts = state
                .radar_timeline
                .snap_to_boundary(clicked_ts, snap_dist_secs)
                .unwrap_or(clicked_ts);

            state.playback_state.set_playback_position(snapped_ts);
            state.playback_state.clear_selection();

            if let Some(frame) = state.playback_state.timestamp_to_frame(snapped_ts as i64) {
                state.playback_state.current_frame = frame;
            }
        }
    }

    // Drag to pan
    if response.dragged() && !shift_held && !state.playback_state.selection_in_progress {
        let delta_secs = -response.drag_delta().x as f64 / zoom;
        state.playback_state.timeline_view_start += delta_secs;
    }

    // Scroll wheel zoom
    if response.hovered() {
        let scroll_delta = ui.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            let zoom_factor = 1.0 + scroll_delta.y as f64 * 0.002;
            let old_zoom = state.playback_state.timeline_zoom;
            let new_zoom = (old_zoom * zoom_factor).clamp(0.000001, 1000.0);

            if let Some(cursor_pos) = response.hover_pos() {
                let cursor_ts = view_start + (cursor_pos.x - full_rect.left()) as f64 / old_zoom;
                let new_view_start =
                    cursor_ts - (cursor_pos.x - full_rect.left()) as f64 / new_zoom;
                state.playback_state.timeline_view_start = new_view_start;
            }

            state.playback_state.timeline_zoom = new_zoom;
        }
    }
}

/// Render shadow scan boundaries from the archive index.
///
/// These are subtle markers showing where scans exist in the archive before
/// they are downloaded. Boundaries that overlap already-downloaded scans are
/// skipped so only un-downloaded positions are highlighted.
#[allow(clippy::too_many_arguments)]
fn render_shadow_boundaries(
    painter: &Painter,
    rect: &Rect,
    boundaries: &[crate::nexrad::ScanBoundary],
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    let view_start_i64 = view_start as i64;
    let view_end_i64 = view_end as i64;

    match detail_level {
        DetailLevel::Solid => {
            // At solid detail, merge all visible shadow boundaries into contiguous regions
            let visible: Vec<_> = boundaries
                .iter()
                .filter(|b| b.end > view_start_i64 && b.start < view_end_i64)
                .filter(|b| {
                    !timeline
                        .scans
                        .iter()
                        .any(|s| (s.key_timestamp as i64 - b.start).abs() < 60)
                })
                .collect();

            if visible.is_empty() {
                return;
            }

            // Merge into contiguous regions (gap < 15 min)
            let mut regions: Vec<(i64, i64)> = Vec::new();
            for b in &visible {
                if let Some(last) = regions.last_mut() {
                    if b.start - last.1 < 900 {
                        last.1 = b.end;
                        continue;
                    }
                }
                regions.push((b.start, b.end));
            }

            for (start, end) in regions {
                let x_start = ts_to_x(start as f64).max(rect.left());
                let x_end = ts_to_x(end as f64).min(rect.right());
                let x_end = if (x_end - x_start) > 0.0 && (x_end - x_start) < 8.0 {
                    (x_start + 8.0).min(rect.right())
                } else {
                    x_end
                };
                if x_end > x_start {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(x_start, rect.top() + 2.0),
                            Pos2::new(x_end, rect.bottom() - 2.0),
                        ),
                        2.0,
                        tl_colors::shadow_fill(),
                    );
                }
            }
        }
        DetailLevel::Scans | DetailLevel::Sweeps => {
            for b in boundaries {
                // Skip if outside visible range
                if b.end <= view_start_i64 || b.start >= view_end_i64 {
                    continue;
                }
                // Skip if this scan is already downloaded (within 60s tolerance)
                if timeline
                    .scans
                    .iter()
                    .any(|s| (s.key_timestamp as i64 - b.start).abs() < 60)
                {
                    continue;
                }

                let x_start = ts_to_x(b.start as f64).max(rect.left());
                let x_end = ts_to_x(b.end as f64).min(rect.right());
                let width = x_end - x_start;

                if width < 1.0 {
                    continue;
                }

                let shadow_rect = Rect::from_min_max(
                    Pos2::new(x_start, rect.top() + 2.0),
                    Pos2::new(x_end, rect.bottom() - 2.0),
                );

                painter.rect_filled(shadow_rect, 2.0, tl_colors::shadow_fill());
                painter.rect_stroke(
                    shadow_rect,
                    2.0,
                    Stroke::new(0.5, tl_colors::shadow_border()),
                    StrokeKind::Inside,
                );
            }
        }
    }
}

/// Render ghost blocks on the scan track for pending/active/processing downloads.
///
/// Distinct visual styles per state:
/// - Pending (queued): blue outline with diagonal stripe pattern
/// - Active (downloading): pulsing blue fill
/// - Processing (in_flight after download): amber tint
/// - Recently completed: brief green flash
#[allow(clippy::too_many_arguments)]
fn render_download_ghosts(
    painter: &Painter,
    rect: &Rect,
    progress: &crate::state::DownloadProgress,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
    anim_time: f64,
) {
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    if detail_level == DetailLevel::Solid {
        // At solid detail, combine all ghosts into one region
        let all: Vec<_> = progress
            .pending_scans
            .iter()
            .chain(progress.in_flight_scans.iter())
            .copied()
            .collect();
        if all.is_empty() {
            return;
        }
        let min_ts = all.iter().map(|(s, _)| *s).min().unwrap() as f64;
        let max_ts = all.iter().map(|(_, e)| *e).max().unwrap() as f64;
        let x_start = ts_to_x(min_ts).max(rect.left());
        let x_end = ts_to_x(max_ts).min(rect.right());
        if x_end > x_start {
            let pulse = (0.5 + 0.5 * (anim_time * 3.0).sin()) as f32;
            let alpha = (25.0 + 15.0 * pulse) as u8;
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x_start, rect.top() + 2.0),
                    Pos2::new(x_end, rect.bottom() - 2.0),
                ),
                2.0,
                Color32::from_rgba_unmultiplied(100, 150, 255, alpha),
            );
        }
        return;
    }

    let now_wall = js_sys::Date::now() / 1000.0;

    // Recently completed scans — brief green flash
    for &(scan_start, completion_time) in &progress.recently_completed {
        let age = now_wall - completion_time;
        if age > 1.0 {
            continue;
        }
        let flash_alpha = ((1.0 - age) * 80.0) as u8;
        // Find this scan's end time from timeline
        if let Some(scan) = timeline
            .scans_in_range(scan_start as f64, scan_start as f64 + 600.0)
            .find(|s| (s.start_time as i64 - scan_start).abs() < 30)
        {
            let x_start = ts_to_x(scan.start_time).max(rect.left());
            let x_end = ts_to_x(scan.end_time).min(rect.right());
            if x_end > x_start {
                let flash_rect = Rect::from_min_max(
                    Pos2::new(x_start, rect.top() + 2.0),
                    Pos2::new(x_end, rect.bottom() - 2.0),
                );
                painter.rect_filled(
                    flash_rect,
                    2.0,
                    Color32::from_rgba_unmultiplied(100, 220, 120, flash_alpha),
                );
            }
        }
    }

    // Helper: draw a ghost block for a scan boundary
    let draw_ghost = |scan_start: i64, scan_end: i64, is_active: bool, is_processing: bool| {
        let start_f64 = scan_start as f64;
        let end_f64 = scan_end as f64;
        if end_f64 < view_start || start_f64 > view_end {
            return;
        }

        // Skip if real data already covers this timestamp
        if timeline
            .scans_in_range(start_f64, end_f64)
            .any(|s| s.start_time <= start_f64 + 30.0 && s.end_time >= start_f64 - 30.0)
        {
            return;
        }

        let x_start = ts_to_x(start_f64).max(rect.left());
        let x_end = ts_to_x(end_f64).min(rect.right());
        if x_end <= x_start || (x_end - x_start) < 1.0 {
            return;
        }

        let ghost_rect = Rect::from_min_max(
            Pos2::new(x_start, rect.top() + 2.0),
            Pos2::new(x_end, rect.bottom() - 2.0),
        );

        if is_active {
            // Active download: pulsing blue fill
            let pulse = (0.5 + 0.5 * (anim_time * 3.0).sin()) as f32;
            let fill_alpha = (35.0 + 30.0 * pulse) as u8;
            let border_alpha = (60.0 + 35.0 * pulse) as u8;
            painter.rect_filled(
                ghost_rect,
                2.0,
                Color32::from_rgba_unmultiplied(100, 160, 255, fill_alpha),
            );
            painter.rect_stroke(
                ghost_rect,
                2.0,
                Stroke::new(
                    1.5,
                    Color32::from_rgba_unmultiplied(100, 160, 255, border_alpha),
                ),
                StrokeKind::Inside,
            );
        } else if is_processing {
            // Processing (ingesting): amber tint with subtle pulse
            let pulse = (0.5 + 0.5 * (anim_time * 2.0).sin()) as f32;
            let fill_alpha = (30.0 + 20.0 * pulse) as u8;
            painter.rect_filled(
                ghost_rect,
                2.0,
                Color32::from_rgba_unmultiplied(200, 160, 60, fill_alpha),
            );
            painter.rect_stroke(
                ghost_rect,
                2.0,
                Stroke::new(1.0, tl_colors::ghost_processing_border()),
                StrokeKind::Inside,
            );
        } else {
            // Pending: blue outline with diagonal stripe pattern
            painter.rect_stroke(
                ghost_rect,
                2.0,
                Stroke::new(1.0, tl_colors::ghost_pending_border()),
                StrokeKind::Inside,
            );
            // Diagonal stripes
            let width = x_end - x_start;
            let h = ghost_rect.height();
            let spacing = 8.0;
            let mut offset = 0.0;
            while offset < width + h {
                let x0 = ghost_rect.left() + offset;
                let x1 = x0 - h;
                let (cx0, cy0) = if x0 > ghost_rect.right() {
                    (
                        ghost_rect.right(),
                        ghost_rect.top() + (x0 - ghost_rect.right()),
                    )
                } else {
                    (x0, ghost_rect.top())
                };
                let (cx1, cy1) = if x1 < ghost_rect.left() {
                    (
                        ghost_rect.left(),
                        ghost_rect.bottom() - (ghost_rect.left() - x1),
                    )
                } else {
                    (x1, ghost_rect.bottom())
                };
                if cy0 < cy1 {
                    painter.line_segment(
                        [Pos2::new(cx0, cy0), Pos2::new(cx1, cy1)],
                        Stroke::new(0.5, tl_colors::ghost_pending_fill()),
                    );
                }
                offset += spacing;
            }
        }
    };

    // Draw pending scans
    for &(s, e) in &progress.pending_scans {
        draw_ghost(s, e, false, false);
    }

    // Draw active scan
    if let Some((s, e)) = progress.active_scan {
        draw_ghost(s, e, true, false);
    }

    // Draw in-flight (processing) scans
    for &(s, e) in &progress.in_flight_scans {
        draw_ghost(s, e, false, true);
    }
}

/// Render real-time streaming progress on the timeline.
///
/// Draws a unified view of the in-progress volume:
/// - **Scan track**: Single VCP-colored block spanning vol_start → expected_end,
///   with solid fill for elapsed time and dashed outline for projected remainder.
/// - **Sweep track**: All elevation sweeps with per-sweep state:
///   - Complete (downloaded & persisted): filled with cool elevation colors
///   - Downloading (in-progress): outline with chunk subdivision inside
///   - Future (not yet collected): dashed outline
///     Each non-complete sweep shows chunk subdivision where downloaded chunks
///     are clipped to the sweep's time range.
#[allow(clippy::too_many_arguments)]
fn render_realtime_progress(
    painter: &Painter,
    scan_rect: &Rect,
    sweep_rect: Option<&Rect>,
    live_state: &crate::state::LiveModeState,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    _anim_time: f64,
    now_secs: f64,
    target_elevation: f32,
    active_sweep: Option<(i64, u8)>,
) {
    let ts_to_x = |ts: f64| -> f32 { scan_rect.left() + ((ts - view_start) * zoom) as f32 };
    let now = now_secs;

    let vol_start = match live_state.current_volume_start {
        Some(v) => v,
        None => return, // No volume in progress yet
    };
    let vcp = live_state.current_vcp_number.unwrap_or(0);
    let expected_dur = live_state.last_volume_duration_secs.unwrap_or(300.0);
    let expected_end = vol_start + expected_dur;
    let expected_count = live_state.expected_elevation_count.unwrap_or(0) as usize;

    let x_vol_start = ts_to_x(vol_start).max(scan_rect.left());
    let x_vol_end = ts_to_x(expected_end).min(scan_rect.right());
    let x_now = ts_to_x(now).min(scan_rect.right());

    if x_vol_end <= x_vol_start || expected_end < view_start || vol_start > view_end {
        return;
    }

    // ═══════════════════════════════════════════════════════════════════
    // SCAN TRACK — single in-progress block (vol_start → expected_end)
    // ═══════════════════════════════════════════════════════════════════

    let scan_block = Rect::from_min_max(
        Pos2::new(x_vol_start, scan_rect.top() + 2.0),
        Pos2::new(x_vol_end, scan_rect.bottom() - 2.0),
    );

    // VCP-colored fill — use the same warm palette as completed scans but
    // with reduced alpha to indicate in-progress.
    let (vr, vg, vb) = tl_colors::vcp_base_rgb(vcp);

    // Elapsed portion: solid fill
    if x_now > x_vol_start {
        let elapsed_rect = Rect::from_min_max(
            scan_block.min,
            Pos2::new(x_now.min(x_vol_end), scan_block.max.y),
        );
        painter.rect_filled(
            elapsed_rect,
            2.0,
            Color32::from_rgba_unmultiplied(vr, vg, vb, 160),
        );
    }

    // Projected remainder: subtle fill indicating estimated extent
    if x_vol_end > x_now && x_now >= scan_rect.left() {
        let future_rect = Rect::from_min_max(Pos2::new(x_now, scan_block.min.y), scan_block.max);
        painter.rect_filled(
            future_rect,
            2.0,
            Color32::from_rgba_unmultiplied(vr, vg, vb, 55),
        );
    }

    // Border: solid on elapsed side, dashed on projected side
    // Left + top/bottom edges for elapsed portion
    if x_now > x_vol_start {
        let elapsed_rect = Rect::from_min_max(
            scan_block.min,
            Pos2::new(x_now.min(x_vol_end), scan_block.max.y),
        );
        painter.rect_stroke(
            elapsed_rect,
            2.0,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(vr, vg, vb, 180)),
            StrokeKind::Inside,
        );
    }
    // Dashed border for projected remainder
    if x_vol_end > x_now && x_now >= scan_rect.left() {
        let dash_color = Color32::from_rgba_unmultiplied(vr, vg, vb, 90);
        // Dashed right edge
        let mut y = scan_block.min.y;
        while y < scan_block.max.y {
            let y_end = (y + 4.0).min(scan_block.max.y);
            painter.line_segment(
                [Pos2::new(x_vol_end, y), Pos2::new(x_vol_end, y_end)],
                Stroke::new(1.0, dash_color),
            );
            y += 7.0;
        }
        // Dashed top and bottom
        let mut x = x_now;
        while x < x_vol_end {
            let x_seg_end = (x + 4.0).min(x_vol_end);
            painter.line_segment(
                [
                    Pos2::new(x, scan_block.min.y),
                    Pos2::new(x_seg_end, scan_block.min.y),
                ],
                Stroke::new(0.5, dash_color),
            );
            painter.line_segment(
                [
                    Pos2::new(x, scan_block.max.y),
                    Pos2::new(x_seg_end, scan_block.max.y),
                ],
                Stroke::new(0.5, dash_color),
            );
            x += 8.0;
        }
    }

    // Unified label centered across the full scan block
    let full_width = x_vol_end - x_vol_start;
    if full_width > 40.0 {
        let received = live_state.elevations_received.len();

        let label = if vcp > 0 && expected_count > 0 {
            if full_width > 120.0 {
                format!("VCP {} {}/{}", vcp, received, expected_count)
            } else if full_width > 70.0 {
                format!("{} {}/{}", vcp, received, expected_count)
            } else {
                format!("{}/{}", received, expected_count)
            }
        } else if vcp > 0 {
            format!("{}", vcp)
        } else if expected_count > 0 {
            format!("{}/{}", received, expected_count)
        } else {
            String::new()
        };

        if !label.is_empty() {
            painter.text(
                scan_block.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::monospace(8.0),
                Color32::from_rgba_unmultiplied(220, 240, 220, 180),
            );
        }
    }

    // ── Projected future scan boundaries (dashed lines) ──
    if expected_dur > 30.0 {
        let boundary_color = tl_colors::estimated_boundary();
        for i in 1..=2 {
            let projected_ts = vol_start + expected_dur * i as f64;
            let x = ts_to_x(projected_ts);
            if x >= scan_rect.left() && x <= scan_rect.right() {
                let mut y = scan_rect.top();
                while y < scan_rect.bottom() {
                    let y_end = (y + 4.0).min(scan_rect.bottom());
                    painter.line_segment(
                        [Pos2::new(x, y), Pos2::new(x, y_end)],
                        Stroke::new(1.0, boundary_color),
                    );
                    y += 7.0;
                }
                painter.text(
                    Pos2::new(x + 3.0, scan_rect.top() + 2.0),
                    egui::Align2::LEFT_TOP,
                    "est.",
                    egui::FontId::monospace(9.0),
                    boundary_color,
                );
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // SWEEP TRACK — all elevation sweeps with per-sweep state + chunks
    // ═══════════════════════════════════════════════════════════════════

    let sweep_rect = match sweep_rect {
        Some(r) => r,
        None => return,
    };
    if expected_count == 0 {
        return;
    }

    // Look up elevation angles from VCP definition (for coloring)
    let vcp_def = crate::state::get_vcp_definition(vcp);
    let elev_angle_for = |elev_num: u8| -> f32 {
        vcp_def
            .and_then(|d| d.elevations.get(elev_num.saturating_sub(1) as usize))
            .map(|e| e.angle)
            .unwrap_or(0.5 * elev_num as f32) // rough fallback
    };

    // Per-elevation sweep durations from VCP azimuth rates (Method A with B fallback).
    // Falls back to even distribution when weighted durations aren't available.
    let sweep_dur_for = |idx: usize| -> f64 {
        live_state
            .sweep_duration_for(idx)
            .unwrap_or(expected_dur / expected_count.max(1) as f64)
    };
    let sweep_start_offset_for = |idx: usize| -> f64 {
        live_state
            .sweep_start_offset(idx)
            .unwrap_or(idx as f64 * expected_dur / expected_count.max(1) as f64)
    };

    let received = &live_state.elevations_received;
    let in_progress_elev = live_state.current_in_progress_elevation;
    let in_progress_radials = live_state.current_in_progress_radials.unwrap_or(0);
    let countdown = live_state.countdown_remaining_secs(now);

    for elev_idx in 0..expected_count {
        let elev_num = (elev_idx + 1) as u8;
        let is_complete = received.contains(&elev_num);
        let this_sweep_dur = sweep_dur_for(elev_idx);

        // Use actual timestamps where available:
        // 1. Completed sweep -> use SweepMeta start/end
        // 2. In-progress sweep with chunk data -> derive bounds from chunk spans
        // 3. Future sweep -> estimate from last known anchor point
        let (sw_start, sw_end) = if is_complete {
            if let Some(meta) = live_state
                .completed_sweep_metas
                .iter()
                .find(|m| m.elevation_number == elev_num)
            {
                (meta.start, meta.end)
            } else {
                let offset = sweep_start_offset_for(elev_idx);
                (vol_start + offset, vol_start + offset + this_sweep_dur)
            }
        } else {
            // For non-completed sweeps, find the best anchor: the end time of
            // the highest completed sweep below this one.
            let anchor_end = live_state
                .completed_sweep_metas
                .iter()
                .filter(|m| m.elevation_number < elev_num)
                .max_by_key(|m| m.elevation_number)
                .map(|m| m.end);

            // Also check if we have actual chunk data for this elevation
            let chunk_min = live_state
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .map(|&(_, s, _, _)| s)
                .reduce(f64::min);
            let chunk_max = live_state
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .map(|&(_, _, e, _)| e)
                .reduce(f64::max);

            let sw_start_actual = match (chunk_min, anchor_end) {
                // Have chunk data: use actual chunk start as sweep start
                (Some(cm), _) => cm,
                // No chunk data but have anchor: estimate remaining sweeps
                // using weighted durations relative to their share of remaining time
                (None, Some(ae)) => {
                    let anchor_elev_num = live_state
                        .completed_sweep_metas
                        .iter()
                        .filter(|m| m.elevation_number < elev_num)
                        .max_by_key(|m| m.elevation_number)
                        .map(|m| m.elevation_number)
                        .unwrap_or(0);
                    let anchor_idx = anchor_elev_num as usize; // elev_num is 1-based, so this is the next idx
                    let remaining_dur = (vol_start + expected_dur) - ae;

                    // Sum the weights of remaining elevations for proportional distribution
                    let remaining_weight_sum: f64 =
                        (anchor_idx..expected_count).map(&sweep_dur_for).sum();

                    if remaining_weight_sum > 0.0 {
                        let offset_from_anchor: f64 = (anchor_idx..elev_idx)
                            .map(|i| (sweep_dur_for(i) / remaining_weight_sum) * remaining_dur)
                            .sum();
                        ae + offset_from_anchor
                    } else {
                        ae
                    }
                }
                // No data at all: use weighted offsets from volume start
                (None, None) => vol_start + sweep_start_offset_for(elev_idx),
            };

            let est_sweep_end = sw_start_actual + this_sweep_dur;
            let sw_end_actual = match chunk_max {
                // If we have chunk data, extend sweep end to at least cover it,
                // but also estimate further since we may not have all radials yet
                Some(cm) => cm.max(est_sweep_end),
                None => est_sweep_end,
            };

            (sw_start_actual, sw_end_actual)
        };

        let x_start = ts_to_x(sw_start).max(sweep_rect.left());
        let x_end = ts_to_x(sw_end).min(sweep_rect.right());
        if x_end - x_start < 1.0 || sw_end < view_start || sw_start > view_end {
            continue;
        }

        let elev_angle = elev_angle_for(elev_num);
        let matches_target = (elev_angle - target_elevation).abs() < 0.3;
        let is_downloading = !is_complete && in_progress_elev == Some(elev_num);
        let is_future = !is_complete && !is_downloading;

        let block = Rect::from_min_max(
            Pos2::new(x_start, sweep_rect.top() + 2.0),
            Pos2::new(x_end, sweep_rect.bottom() - 2.0),
        );
        let width = x_end - x_start;

        if is_complete {
            // -- Complete: filled with cool elevation colors --
            let is_active = active_sweep.is_some_and(|(_, active_en)| active_en == elev_num);
            let fill = tl_colors::sweep_fill(elev_angle, matches_target);
            let border = tl_colors::sweep_border(elev_angle, is_active);
            painter.rect_filled(block, 1.0, fill);
            if width > 3.0 {
                let stroke_width = if is_active { 2.0 } else { 0.5 };
                let stroke_kind = if is_active {
                    StrokeKind::Outside
                } else {
                    StrokeKind::Inside
                };
                painter.rect_stroke(block, 1.0, Stroke::new(stroke_width, border), stroke_kind);
            }
        } else if is_downloading {
            // -- Downloading: outline with chunk subdivision + progress bar --
            let border_color = Color32::from_rgba_unmultiplied(60, 140, 200, 100);

            // Total radials accumulated for this elevation across all chunks
            let total_radials: u32 = live_state
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .map(|&(_, _, _, r)| r)
                .sum::<u32>()
                + in_progress_radials;
            let expected_radials = 360u32; // NEXRAD standard full rotation

            // Progress fill: fraction of block width based on radials collected
            let frac = (total_radials as f32 / expected_radials as f32).clamp(0.0, 1.0);
            if frac > 0.0 {
                let progress_rect = Rect::from_min_max(
                    Pos2::new(block.min.x, block.min.y),
                    Pos2::new(block.min.x + (block.width() * frac), block.max.y),
                );
                painter.rect_filled(
                    progress_rect,
                    1.0,
                    Color32::from_rgba_unmultiplied(60, 140, 200, 45),
                );
            }

            // Dashed border: the extent of the downloading sweep is estimated,
            // so use dashes to communicate that these bounds are approximate.
            {
                let mut x = block.min.x;
                while x < block.max.x {
                    let x_seg_end = (x + 4.0).min(block.max.x);
                    painter.line_segment(
                        [Pos2::new(x, block.min.y), Pos2::new(x_seg_end, block.min.y)],
                        Stroke::new(1.0, border_color),
                    );
                    painter.line_segment(
                        [Pos2::new(x, block.max.y), Pos2::new(x_seg_end, block.max.y)],
                        Stroke::new(1.0, border_color),
                    );
                    x += 8.0;
                }
                let mut y = block.min.y;
                while y < block.max.y {
                    let y_end = (y + 3.0).min(block.max.y);
                    painter.line_segment(
                        [Pos2::new(block.min.x, y), Pos2::new(block.min.x, y_end)],
                        Stroke::new(1.0, border_color),
                    );
                    painter.line_segment(
                        [Pos2::new(block.max.x, y), Pos2::new(block.max.x, y_end)],
                        Stroke::new(1.0, border_color),
                    );
                    y += 6.0;
                }
            }

            // Draw downloaded chunks that belong to this elevation, with
            // clear separators between each chunk boundary.
            let mut prev_chunk_end_x: Option<f32> = None;
            for &(span_elev, span_start, span_end, _) in &live_state.chunk_elev_spans {
                if span_elev != elev_num {
                    continue;
                }
                let cx0 = ts_to_x(span_start).max(sweep_rect.left());
                let cx1 = ts_to_x(span_end).min(sweep_rect.right());
                if cx1 > cx0 {
                    let chunk_rect = Rect::from_min_max(
                        Pos2::new(cx0, block.min.y + 1.0),
                        Pos2::new(cx1, block.max.y - 1.0),
                    );
                    painter.rect_filled(
                        chunk_rect,
                        0.0,
                        Color32::from_rgba_unmultiplied(60, 140, 200, 55),
                    );

                    // Separator tick at each chunk boundary
                    if let Some(prev_x) = prev_chunk_end_x {
                        // Draw separator at the boundary between previous and current chunk
                        let sep_x = (prev_x + cx0) / 2.0;
                        painter.line_segment(
                            [
                                Pos2::new(sep_x, block.min.y + 1.0),
                                Pos2::new(sep_x, block.max.y - 1.0),
                            ],
                            Stroke::new(1.0, tl_colors::rt_chunk_separator()),
                        );
                    }
                    prev_chunk_end_x = Some(cx1);
                }
            }

            // Leading edge: bright vertical line at the progress front
            let edge_x = block.min.x + (block.width() * frac);
            if frac > 0.01 && frac < 0.99 {
                painter.line_segment(
                    [
                        Pos2::new(edge_x, block.min.y),
                        Pos2::new(edge_x, block.max.y),
                    ],
                    Stroke::new(1.5, tl_colors::rt_progress_edge()),
                );
            }

            // ── Next-chunk placeholder block ──
            // When waiting for the next chunk, render a distinct placeholder
            // right after the last received chunk with a dotted border and
            // countdown label. Sized to match chunk_interval in timeline scale.
            if let Some(remaining) = countdown {
                let nc_start_x = prev_chunk_end_x.unwrap_or(edge_x);
                let chunk_px = (live_state.chunk_interval_secs * zoom) as f32;
                let nc_width_raw = chunk_px.max(8.0);
                let nc_end_x = (nc_start_x + nc_width_raw).min(block.max.x);

                let nc_rect = Rect::from_min_max(
                    Pos2::new(nc_start_x, block.min.y),
                    Pos2::new(nc_end_x, block.max.y),
                );
                let nc_width = nc_rect.width();

                // Faint fill
                painter.rect_filled(nc_rect, 1.0, tl_colors::rt_next_chunk_fill());

                // Dotted border (shorter dashes than the regular dashed borders)
                let dot_color = tl_colors::rt_next_chunk_border();
                // Top and bottom dotted edges
                {
                    let mut x = nc_rect.min.x;
                    while x < nc_rect.max.x {
                        let x_seg_end = (x + 2.0).min(nc_rect.max.x);
                        painter.line_segment(
                            [
                                Pos2::new(x, nc_rect.min.y),
                                Pos2::new(x_seg_end, nc_rect.min.y),
                            ],
                            Stroke::new(1.0, dot_color),
                        );
                        painter.line_segment(
                            [
                                Pos2::new(x, nc_rect.max.y),
                                Pos2::new(x_seg_end, nc_rect.max.y),
                            ],
                            Stroke::new(1.0, dot_color),
                        );
                        x += 4.0; // 2px on, 2px off = dotted pattern
                    }
                }
                // Left and right dotted edges
                {
                    let mut y = nc_rect.min.y;
                    while y < nc_rect.max.y {
                        let y_end = (y + 2.0).min(nc_rect.max.y);
                        painter.line_segment(
                            [Pos2::new(nc_rect.min.x, y), Pos2::new(nc_rect.min.x, y_end)],
                            Stroke::new(1.0, dot_color),
                        );
                        painter.line_segment(
                            [Pos2::new(nc_rect.max.x, y), Pos2::new(nc_rect.max.x, y_end)],
                            Stroke::new(1.0, dot_color),
                        );
                        y += 4.0;
                    }
                }

                // Countdown label centered in the next-chunk placeholder
                if nc_width > 16.0 {
                    let label = format!("{}s", remaining.ceil() as i32);
                    painter.text(
                        nc_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::monospace(8.0),
                        tl_colors::rt_next_chunk_label(),
                    );
                }
            }

            // Radial progress label in the filled (collected) portion
            if countdown.is_none() && width > 30.0 {
                // Show radial progress as fraction while actively receiving
                let label = if width > 55.0 {
                    format!("{}/{}", total_radials, expected_radials)
                } else {
                    format!("{}", total_radials)
                };
                painter.text(
                    block.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::monospace(8.0),
                    Color32::from_rgba_unmultiplied(140, 200, 255, 180),
                );
            } else if countdown.is_some() && frac > 0.15 {
                // When waiting, show radial count in the collected portion
                let collected_center_x = (block.min.x + edge_x) / 2.0;
                let collected_width = edge_x - block.min.x;
                if collected_width > 25.0 {
                    let label = format!("{}", total_radials);
                    painter.text(
                        Pos2::new(collected_center_x, block.center().y),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::monospace(8.0),
                        Color32::from_rgba_unmultiplied(140, 200, 255, 140),
                    );
                }
            }
        } else if is_future {
            // Check if this is the first future sweep (next to receive data)
            // and we're waiting for a chunk with no downloading sweep active.
            let is_next_sweep = in_progress_elev.is_none()
                && countdown.is_some()
                && !received.iter().any(|&e| e > elev_num);

            // For the "next" sweep, also check it's the very first future one
            let is_first_future = is_next_sweep
                && (elev_num == 1 || received.last().is_some_and(|&last| last == elev_num - 1));

            if is_first_future {
                // ── Next-chunk placeholder on the first future sweep ──
                // Sized to one chunk interval at the start of the sweep block,
                // not the entire sweep.
                let chunk_px = (live_state.chunk_interval_secs * zoom) as f32;
                let nc_end_x = (block.min.x + chunk_px.max(8.0)).min(block.max.x);
                let nc_rect = Rect::from_min_max(
                    Pos2::new(block.min.x, block.min.y),
                    Pos2::new(nc_end_x, block.max.y),
                );
                let nc_width = nc_rect.width();

                let nc_fill = tl_colors::rt_next_chunk_fill();
                let dot_color = tl_colors::rt_next_chunk_border();

                painter.rect_filled(nc_rect, 1.0, nc_fill);

                // Dotted border (2px on, 2px off)
                {
                    let mut x = nc_rect.min.x;
                    while x < nc_rect.max.x {
                        let x_seg_end = (x + 2.0).min(nc_rect.max.x);
                        painter.line_segment(
                            [
                                Pos2::new(x, nc_rect.min.y),
                                Pos2::new(x_seg_end, nc_rect.min.y),
                            ],
                            Stroke::new(1.0, dot_color),
                        );
                        painter.line_segment(
                            [
                                Pos2::new(x, nc_rect.max.y),
                                Pos2::new(x_seg_end, nc_rect.max.y),
                            ],
                            Stroke::new(1.0, dot_color),
                        );
                        x += 4.0;
                    }
                    let mut y = nc_rect.min.y;
                    while y < nc_rect.max.y {
                        let y_end = (y + 2.0).min(nc_rect.max.y);
                        painter.line_segment(
                            [Pos2::new(nc_rect.min.x, y), Pos2::new(nc_rect.min.x, y_end)],
                            Stroke::new(1.0, dot_color),
                        );
                        painter.line_segment(
                            [Pos2::new(nc_rect.max.x, y), Pos2::new(nc_rect.max.x, y_end)],
                            Stroke::new(1.0, dot_color),
                        );
                        y += 4.0;
                    }
                }

                // Countdown label
                if let Some(remaining) = countdown {
                    if nc_width > 16.0 {
                        painter.text(
                            nc_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("{}s", remaining.ceil() as i32),
                            egui::FontId::monospace(8.0),
                            tl_colors::rt_next_chunk_label(),
                        );
                    }
                }

                // Still draw the rest of the sweep as regular future dashed outline
                if nc_end_x < block.max.x {
                    let rest_block = Rect::from_min_max(
                        Pos2::new(nc_end_x, block.min.y),
                        Pos2::new(block.max.x, block.max.y),
                    );
                    let dash_color = tl_colors::rt_pending_sweep_border();
                    let mut x = rest_block.min.x;
                    while x < rest_block.max.x {
                        let x_seg_end = (x + 4.0).min(rest_block.max.x);
                        painter.line_segment(
                            [
                                Pos2::new(x, rest_block.min.y),
                                Pos2::new(x_seg_end, rest_block.min.y),
                            ],
                            Stroke::new(0.5, dash_color),
                        );
                        painter.line_segment(
                            [
                                Pos2::new(x, rest_block.max.y),
                                Pos2::new(x_seg_end, rest_block.max.y),
                            ],
                            Stroke::new(0.5, dash_color),
                        );
                        x += 8.0;
                    }
                    let mut y = rest_block.min.y;
                    while y < rest_block.max.y {
                        let y_end = (y + 3.0).min(rest_block.max.y);
                        painter.line_segment(
                            [
                                Pos2::new(rest_block.max.x, y),
                                Pos2::new(rest_block.max.x, y_end),
                            ],
                            Stroke::new(0.5, dash_color),
                        );
                        y += 6.0;
                    }
                }
            } else {
                // -- Regular future: dashed outline to indicate estimated bounds --
                let dash_color = tl_colors::rt_pending_sweep_border();
                // Dashed top and bottom edges
                let mut x = block.min.x;
                while x < block.max.x {
                    let x_seg_end = (x + 4.0).min(block.max.x);
                    painter.line_segment(
                        [Pos2::new(x, block.min.y), Pos2::new(x_seg_end, block.min.y)],
                        Stroke::new(0.5, dash_color),
                    );
                    painter.line_segment(
                        [Pos2::new(x, block.max.y), Pos2::new(x_seg_end, block.max.y)],
                        Stroke::new(0.5, dash_color),
                    );
                    x += 8.0;
                }
                // Dashed left and right edges
                let mut y = block.min.y;
                while y < block.max.y {
                    let y_end = (y + 3.0).min(block.max.y);
                    painter.line_segment(
                        [Pos2::new(block.min.x, y), Pos2::new(block.min.x, y_end)],
                        Stroke::new(0.5, dash_color),
                    );
                    painter.line_segment(
                        [Pos2::new(block.max.x, y), Pos2::new(block.max.x, y_end)],
                        Stroke::new(0.5, dash_color),
                    );
                    y += 6.0;
                }
            }
        }

        // Elevation label (for all states, when wide enough)
        if width > 25.0 && !is_downloading {
            let label = if width > 50.0 {
                format!("{:.1}\u{00B0}", elev_angle)
            } else {
                format!("{:.0}", elev_angle)
            };
            let label_alpha = if is_complete { 180u8 } else { 100 };
            painter.text(
                block.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::monospace(8.0),
                Color32::from_rgba_unmultiplied(220, 230, 255, label_alpha),
            );
        }
    }
}

/// Render saved event overlays on the timeline.
fn render_saved_events(
    painter: &Painter,
    overlay_rect: &Rect,
    saved_events: &SavedEvents,
    current_site: &str,
    view_start: f64,
    zoom: f64,
) {
    let ts_to_x = |ts: f64| -> f32 { overlay_rect.left() + ((ts - view_start) * zoom) as f32 };

    for (i, event) in saved_events.events.iter().enumerate() {
        if event.site_id != current_site {
            continue;
        }

        let start_x = ts_to_x(event.start_time);
        let end_x = ts_to_x(event.end_time);

        // Skip if entirely outside the visible area
        if end_x < overlay_rect.left() || start_x > overlay_rect.right() {
            continue;
        }

        let visible_start = start_x.max(overlay_rect.left());
        let visible_end = end_x.min(overlay_rect.right());

        // Semi-transparent fill
        let event_rect = Rect::from_min_max(
            Pos2::new(visible_start, overlay_rect.top()),
            Pos2::new(visible_end, overlay_rect.bottom()),
        );
        painter.rect_filled(event_rect, 0.0, tl_colors::event_fill(i));

        // Boundary lines
        let border_color = tl_colors::event_border(i);
        if start_x >= overlay_rect.left() && start_x <= overlay_rect.right() {
            painter.line_segment(
                [
                    Pos2::new(start_x, overlay_rect.top()),
                    Pos2::new(start_x, overlay_rect.bottom()),
                ],
                Stroke::new(1.0, border_color),
            );
        }
        if end_x >= overlay_rect.left() && end_x <= overlay_rect.right() {
            painter.line_segment(
                [
                    Pos2::new(end_x, overlay_rect.top()),
                    Pos2::new(end_x, overlay_rect.bottom()),
                ],
                Stroke::new(1.0, border_color),
            );
        }

        // Event name label (at top of the rectangle, clipped to visible)
        let label_width = visible_end - visible_start;
        if label_width > 20.0 {
            let label_x = ((start_x + end_x) / 2.0)
                .clamp(overlay_rect.left() + 10.0, overlay_rect.right() - 10.0);
            painter.text(
                Pos2::new(label_x, overlay_rect.top() + 2.0),
                egui::Align2::CENTER_TOP,
                &event.name,
                egui::FontId::proportional(9.0),
                tl_colors::event_label(i),
            );
        }
    }
}

/// Render hover tooltip for timeline elements.
#[allow(clippy::too_many_arguments)]
fn render_timeline_tooltip(
    ui: &mut egui::Ui,
    timeline: &RadarTimeline,
    live_state: &crate::state::LiveModeState,
    hover_ts: f64,
    hover_pos: Pos2,
    scan_rect: &Rect,
    sweep_rect: &Rect,
    detail_level: DetailLevel,
    use_local: bool,
    now_secs: f64,
) {
    let in_sweep_track = detail_level == DetailLevel::Sweeps && hover_pos.y > sweep_rect.top();

    // Find the scan at the hovered timestamp
    let scan = timeline
        .scans_in_range(hover_ts - 0.5, hover_ts + 0.5)
        .find(|s| s.start_time <= hover_ts && s.end_time >= hover_ts);

    // Check if hovering within the active real-time volume (including projected future)
    let in_active_volume =
        scan.is_none() && live_state.is_active() && live_state.current_volume_start.is_some() && {
            let vol_start = live_state.current_volume_start.unwrap();
            let expected_dur = live_state.last_volume_duration_secs.unwrap_or(300.0);
            hover_ts >= vol_start && hover_ts <= vol_start + expected_dur
        };

    // If in sweep track, search for sweep across ALL visible scans (not just the
    // scan containing hover_ts). This handles edge cases where a sweep's time range
    // extends before its parent scan's start_time.
    let (sweep, sweep_parent_scan) = if in_sweep_track {
        let mut found = None;
        for s in timeline.scans_in_range(hover_ts - 600.0, hover_ts + 600.0) {
            if let Some(sw) = s
                .sweeps
                .iter()
                .find(|sw| sw.start_time <= hover_ts && sw.end_time >= hover_ts)
            {
                found = Some((sw, s));
                break;
            }
        }
        match found {
            Some((sw, s)) => (Some(sw), Some(s)),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    if scan.is_none() && sweep.is_none() && !in_active_volume {
        return;
    }

    egui::Tooltip::always_open(
        ui.ctx().clone(),
        egui::LayerId::new(egui::Order::Tooltip, ui.id()),
        ui.id().with("tl_tooltip"),
        Rect::from_center_size(hover_pos, Vec2::splat(20.0)),
    )
    .show(|ui: &mut egui::Ui| {
        if let Some(sweep) = sweep {
            render_sweep_tooltip_content(ui, sweep, sweep_parent_scan, use_local);
        } else if in_active_volume {
            render_realtime_volume_tooltip(
                ui,
                live_state,
                hover_ts,
                now_secs,
                in_sweep_track,
                use_local,
            );
        } else if let Some(scan) = scan {
            render_scan_tooltip_content(ui, scan, live_state, use_local);
        }
    });

    let _ = scan_rect; // suppress unused warning when not in sweep mode
}

/// Render tooltip content when hovering over a sweep block.
fn render_sweep_tooltip_content(
    ui: &mut egui::Ui,
    sweep: &crate::state::radar_data::Sweep,
    parent_scan: Option<&crate::state::radar_data::Scan>,
    use_local: bool,
) {
    ui.label(
        RichText::new(format!("Elevation Sweep #{}", sweep.elevation_number))
            .strong()
            .size(12.0),
    );
    ui.label(
        RichText::new("One 360\u{00B0} rotation at a single antenna tilt angle.")
            .size(10.0)
            .weak(),
    );
    ui.separator();

    let sweep_count = parent_scan
        .and_then(|s| s.vcp_pattern.as_ref().map(|v| v.elevations.len()))
        .or_else(|| parent_scan.map(|s| s.sweeps.len()))
        .unwrap_or(0);
    if sweep_count > 0 {
        ui.label(format!(
            "Elevation: {:.1}\u{00B0} (cut #{} of {})",
            sweep.elevation, sweep.elevation_number, sweep_count
        ));
    } else {
        ui.label(format!(
            "Elevation: {:.1}\u{00B0} (cut #{})",
            sweep.elevation, sweep.elevation_number
        ));
    }

    let duration = sweep.end_time - sweep.start_time;
    let start_str = format_timestamp_full(sweep.start_time, use_local);
    let end_str = format_timestamp_full(sweep.end_time, use_local);
    ui.label(format!(
        "Time: {} \u{2192} {} ({:.0}s)",
        start_str, end_str, duration
    ));

    // Warn if sweep extends outside its parent scan
    if let Some(ps) = parent_scan {
        if sweep.start_time < ps.start_time || sweep.end_time > ps.end_time {
            ui.label(
                RichText::new("Note: sweep time range extends outside its parent scan")
                    .size(9.0)
                    .italics()
                    .color(Color32::from_rgb(255, 200, 100)),
            );
        }
    }

    // Waveform and products from VCP
    if let Some(vcp) = parent_scan.and_then(|s| s.vcp_pattern.as_ref()) {
        if let Some(vcp_elev) = vcp
            .elevations
            .get(sweep.elevation_number.saturating_sub(1) as usize)
        {
            let wf_label = match vcp_elev.waveform.as_str() {
                "CS" | "ContiguousSurveillance" => "Contiguous Surveillance",
                "CDW" | "ContiguousDopplerWithGating" => "Contiguous Doppler (Gated)",
                "CDWO" | "ContiguousDopplerWithoutGating" => "Contiguous Doppler",
                "B" | "Batch" => "Batch",
                "SPP" | "StaggeredPulsePair" => "Staggered Pulse Pair",
                other => other,
            };
            let products = match vcp_elev.waveform.as_str() {
                "CS" | "ContiguousSurveillance" => "Reflectivity",
                "CDW"
                | "CDWO"
                | "ContiguousDopplerWithGating"
                | "ContiguousDopplerWithoutGating" => "Velocity",
                "B" | "Batch" => "Reflectivity / Velocity",
                "SPP" | "StaggeredPulsePair" => "Reflectivity / Velocity / Differential",
                _ => "Unknown",
            };
            ui.label(format!("Waveform: {}", wf_label));
            ui.label(format!("Products: {}", products));

            let mut flags = Vec::new();
            if vcp_elev.is_sails {
                flags.push("SAILS");
            }
            if vcp_elev.is_mrle {
                flags.push("MRLE");
            }
            if vcp_elev.is_base_tilt {
                flags.push("Base Tilt");
            }
            if !flags.is_empty() {
                ui.label(format!("Flags: {}", flags.join(", ")));
            }
        }
    }
}

/// Render tooltip for the in-progress realtime volume.
///
/// When hovering the sweep track, this identifies which realtime sweep block
/// is under the cursor and shows per-sweep details including chunk progress.
/// When hovering the scan track, it shows the volume-level summary.
#[allow(clippy::too_many_arguments)]
fn render_realtime_volume_tooltip(
    ui: &mut egui::Ui,
    live_state: &crate::state::LiveModeState,
    hover_ts: f64,
    now_secs: f64,
    in_sweep_track: bool,
    use_local: bool,
) {
    let vol_start = live_state.current_volume_start.unwrap();
    let expected_dur = live_state.last_volume_duration_secs.unwrap_or(300.0);
    let expected_end = vol_start + expected_dur;
    let now = now_secs;
    let past_now = hover_ts > now;
    let vcp_num = live_state.current_vcp_number.unwrap_or(0);
    let expected_count = live_state.expected_elevation_count.unwrap_or(0) as usize;

    // ── Per-sweep tooltip when hovering the sweep track ──────────────
    if in_sweep_track && expected_count > 0 {
        let vcp_def = crate::state::get_vcp_definition(vcp_num);

        // Per-elevation sweep durations (same logic as render_realtime_progress)
        let sweep_dur_for = |idx: usize| -> f64 {
            live_state
                .sweep_duration_for(idx)
                .unwrap_or(expected_dur / expected_count.max(1) as f64)
        };
        let sweep_start_offset_for = |idx: usize| -> f64 {
            live_state
                .sweep_start_offset(idx)
                .unwrap_or(idx as f64 * expected_dur / expected_count.max(1) as f64)
        };

        // Replicate the sweep-to-timestamp mapping from render_realtime_progress
        // to find which elevation block contains hover_ts.
        let mut hovered_elev: Option<(u8, f64, f64)> = None;
        let mut nearest_elev: Option<(u8, f64, f64)> = None;
        let mut nearest_dist: f64 = f64::MAX;
        for elev_idx in 0..expected_count {
            let elev_num = (elev_idx + 1) as u8;
            let is_complete = live_state.elevations_received.contains(&elev_num);
            let this_sweep_dur = sweep_dur_for(elev_idx);

            let (sw_start, sw_end) = if is_complete {
                if let Some(meta) = live_state
                    .completed_sweep_metas
                    .iter()
                    .find(|m| m.elevation_number == elev_num)
                {
                    (meta.start, meta.end)
                } else {
                    let offset = sweep_start_offset_for(elev_idx);
                    (vol_start + offset, vol_start + offset + this_sweep_dur)
                }
            } else {
                let anchor_end = live_state
                    .completed_sweep_metas
                    .iter()
                    .filter(|m| m.elevation_number < elev_num)
                    .max_by_key(|m| m.elevation_number)
                    .map(|m| m.end);

                let chunk_min = live_state
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .map(|&(_, s, _, _)| s)
                    .reduce(f64::min);
                let chunk_max = live_state
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .map(|&(_, _, e, _)| e)
                    .reduce(f64::max);

                let sw_start_actual = match (chunk_min, anchor_end) {
                    (Some(cm), _) => cm,
                    (None, Some(ae)) => {
                        let anchor_elev_num = live_state
                            .completed_sweep_metas
                            .iter()
                            .filter(|m| m.elevation_number < elev_num)
                            .max_by_key(|m| m.elevation_number)
                            .map(|m| m.elevation_number)
                            .unwrap_or(0);
                        let anchor_idx = anchor_elev_num as usize;
                        let remaining_dur = (vol_start + expected_dur) - ae;
                        let remaining_weight_sum: f64 =
                            (anchor_idx..expected_count).map(&sweep_dur_for).sum();
                        if remaining_weight_sum > 0.0 {
                            let offset_from_anchor: f64 = (anchor_idx..elev_idx)
                                .map(|i| (sweep_dur_for(i) / remaining_weight_sum) * remaining_dur)
                                .sum();
                            ae + offset_from_anchor
                        } else {
                            ae
                        }
                    }
                    (None, None) => vol_start + sweep_start_offset_for(elev_idx),
                };

                let est_sweep_end = sw_start_actual + this_sweep_dur;
                let sw_end_actual = match chunk_max {
                    Some(cm) => cm.max(est_sweep_end),
                    None => est_sweep_end,
                };

                (sw_start_actual, sw_end_actual)
            };

            if hover_ts >= sw_start && hover_ts <= sw_end {
                hovered_elev = Some((elev_num, sw_start, sw_end));
                break;
            }

            // Track nearest sweep so we can snap to it if hover_ts falls in a
            // gap (e.g. due to timeline auto-scroll shifting hover_ts between
            // frames). Without this, the tooltip flickers between per-sweep
            // and volume-level content as the cursor drifts across boundaries.
            let dist = if hover_ts < sw_start {
                sw_start - hover_ts
            } else {
                hover_ts - sw_end
            };
            if nearest_elev.is_none() || dist < nearest_dist {
                nearest_elev = Some((elev_num, sw_start, sw_end));
                nearest_dist = dist;
            }
        }

        // Snap to nearest sweep if hover_ts missed due to frame-to-frame drift
        if hovered_elev.is_none()
            && nearest_dist < (expected_dur / expected_count.max(1) as f64) * 0.5
        {
            hovered_elev = nearest_elev;
        }

        if let Some((elev_num, sw_start, sw_end)) = hovered_elev {
            let is_complete = live_state.elevations_received.contains(&elev_num);
            let is_downloading =
                !is_complete && live_state.current_in_progress_elevation == Some(elev_num);
            let elev_angle = vcp_def
                .and_then(|d| d.elevations.get(elev_num.saturating_sub(1) as usize))
                .map(|e| e.angle)
                .unwrap_or(0.5 * elev_num as f32);

            // Header
            let state_label = if is_complete {
                "Complete"
            } else if is_downloading {
                "Collecting"
            } else {
                "Pending"
            };
            ui.label(
                RichText::new(format!(
                    "Elevation Sweep #{} \u{2014} {}",
                    elev_num, state_label
                ))
                .strong()
                .size(12.0),
            );
            ui.label(
                RichText::new(format!(
                    "{:.1}\u{00B0} (cut #{} of {})",
                    elev_angle, elev_num, expected_count
                ))
                .size(10.0)
                .weak(),
            );
            ui.separator();

            if is_complete {
                // Show actual timing for completed sweeps
                if let Some(meta) = live_state
                    .completed_sweep_metas
                    .iter()
                    .find(|m| m.elevation_number == elev_num)
                {
                    let duration = meta.end - meta.start;
                    let start_str = format_timestamp_full(meta.start, use_local);
                    ui.label(format!("Time: {} ({:.0}s)", start_str, duration));
                }
                ui.label(
                    RichText::new("Data received and stored.")
                        .size(10.0)
                        .color(Color32::from_rgb(100, 200, 100)),
                );
            } else if is_downloading {
                // Show chunk-level progress
                let chunks_for_elev: Vec<_> = live_state
                    .chunk_elev_spans
                    .iter()
                    .filter(|&&(e, _, _, _)| e == elev_num)
                    .collect();
                let completed_chunks = chunks_for_elev.len();
                let in_progress_radials = live_state.current_in_progress_radials.unwrap_or(0);

                let total_radials: u32 =
                    chunks_for_elev.iter().map(|&&(_, _, _, r)| r).sum::<u32>()
                        + in_progress_radials;

                ui.label(format!("Radials: {}/360 collected", total_radials));

                // Total chunks received for the whole volume gives context
                let total_volume_chunks = live_state.chunks_received;

                // Show per-chunk breakdown
                if completed_chunks > 0 || in_progress_radials > 0 {
                    ui.separator();
                    let has_active = in_progress_radials > 0
                        || live_state.phase == crate::state::LivePhase::Streaming;
                    let display_total = if has_active {
                        completed_chunks + 1
                    } else {
                        completed_chunks
                    };
                    ui.label(
                        RichText::new(format!(
                            "Chunks for this elevation ({} total in volume):",
                            total_volume_chunks
                        ))
                        .size(10.0)
                        .weak(),
                    );
                    for (i, &&(_, _, _, cr)) in chunks_for_elev.iter().enumerate() {
                        let chunk_num = i + 1;
                        let label = format!(
                            "  Chunk {}/{}: {} radials, collected",
                            chunk_num, display_total, cr
                        );
                        ui.label(RichText::new(label).size(10.0));
                    }
                    if has_active {
                        let chunk_num = completed_chunks + 1;
                        let label = format!(
                            "  Chunk {}/{}: {} radials, collecting\u{2026}",
                            chunk_num, display_total, in_progress_radials
                        );
                        ui.label(
                            RichText::new(label)
                                .size(10.0)
                                .color(Color32::from_rgb(100, 180, 255)),
                        );
                    }
                }

                // Countdown if waiting
                let countdown = live_state.countdown_remaining_secs(now);
                if let Some(remaining) = countdown {
                    ui.label(format!("Next chunk in ~{}s", remaining.ceil() as i32));
                }
            } else {
                // Future/pending sweep
                let duration = sw_end - sw_start;
                ui.label(format!("Est. duration: ~{:.0}s", duration));
                ui.label(
                    RichText::new("Not yet started \u{2014} bounds are estimated.")
                        .size(10.0)
                        .italics()
                        .color(Color32::from_rgba_unmultiplied(180, 200, 220, 160)),
                );
            }

            // VCP waveform info if available
            if let Some(vcp_def) = vcp_def {
                if let Some(vcp_elev) = vcp_def.elevations.get(elev_num.saturating_sub(1) as usize)
                {
                    ui.separator();
                    let wf_label = match vcp_elev.waveform {
                        "CS" | "ContiguousSurveillance" => "Contiguous Surveillance",
                        "CDW" | "ContiguousDopplerWithGating" => "Contiguous Doppler (Gated)",
                        "CDWO" | "ContiguousDopplerWithoutGating" => "Contiguous Doppler",
                        "B" | "Batch" => "Batch",
                        "SPP" | "StaggeredPulsePair" => "Staggered Pulse Pair",
                        other => other,
                    };
                    ui.label(format!("Waveform: {}", wf_label));
                }
            }

            return;
        }
    }

    // ── Volume-level tooltip (scan track or no sweep match) ──────────
    let vcp_label = if vcp_num > 0 {
        format!("VCP {}", vcp_num)
    } else {
        "Unknown VCP".to_string()
    };
    ui.label(
        RichText::new(format!("Volume Scan In Progress ({})", vcp_label))
            .strong()
            .size(12.0),
    );

    let mode_desc = match vcp_num {
        215 | 212 => "Precipitation Mode",
        31 | 32 | 35 => "Clear Air Mode",
        12 | 121 => "Severe Weather Mode",
        _ if vcp_num > 0 => "Known Mode",
        _ => "Unknown Mode",
    };
    ui.label(
        RichText::new(format!(
            "Radar is actively collecting data. ({})",
            mode_desc
        ))
        .size(10.0)
        .weak(),
    );
    ui.separator();

    let start_str = format_timestamp_full(vol_start, use_local);
    ui.label(format!("Started: {}", start_str));
    // Round to whole seconds so text doesn't change every frame (avoids tooltip resize flicker)
    let elapsed = (now - vol_start).floor();
    let remaining = (expected_end - now).ceil();
    if remaining > 0.0 {
        ui.label(format!(
            "Elapsed: {}s / est. {:.0}s total",
            elapsed as i64, expected_dur
        ));
    } else {
        ui.label(format!(
            "Elapsed: {}s (expected ~{:.0}s)",
            elapsed as i64, expected_dur
        ));
    }

    let received = live_state.elevations_received.len();
    let expected = live_state.expected_elevation_count.unwrap_or(0);
    if expected > 0 {
        ui.label(format!("Elevations: {}/{} received", received, expected));
    } else if received > 0 {
        ui.label(format!("Elevations: {} received", received));
    }

    if past_now {
        ui.separator();
        ui.label(
            RichText::new("Projected area \u{2014} data not yet collected")
                .size(10.0)
                .italics()
                .color(Color32::from_rgba_unmultiplied(180, 200, 180, 160)),
        );
        if remaining > 0.0 {
            ui.label(format!("Est. ~{}s remaining", remaining as i64));
        }
    } else {
        ui.separator();
        ui.label(
            RichText::new(format!(
                "Live: {}/{} elevations received",
                received, expected
            ))
            .color(Color32::from_rgb(100, 200, 100)),
        );
    }
}

/// Render tooltip content when hovering over a scan block.
fn render_scan_tooltip_content(
    ui: &mut egui::Ui,
    scan: &crate::state::radar_data::Scan,
    live_state: &crate::state::LiveModeState,
    use_local: bool,
) {
    let vcp_label = if scan.vcp > 0 {
        format!("VCP {}", scan.vcp)
    } else {
        "Unknown VCP".to_string()
    };
    ui.label(
        RichText::new(format!("Volume Scan ({})", vcp_label))
            .strong()
            .size(12.0),
    );

    let mode_desc = match scan.vcp {
        215 | 212 => "Precipitation Mode",
        31 | 32 | 35 => "Clear Air Mode",
        12 | 121 => "Severe Weather Mode",
        _ if scan.vcp > 0 => "Known Mode",
        _ => "Unknown Mode",
    };
    let elev_count = scan
        .vcp_pattern
        .as_ref()
        .map(|v| v.elevations.len())
        .unwrap_or(scan.sweeps.len());
    let desc = if elev_count > 0 {
        format!(
            "A complete 360\u{00B0} survey at {} elevation angles. ({})",
            elev_count, mode_desc
        )
    } else {
        format!("A volume scan using {}.", mode_desc)
    };
    ui.label(RichText::new(desc).size(10.0).weak());
    ui.separator();

    let duration = scan.end_time - scan.start_time;
    let start_str = format_timestamp_full(scan.start_time, use_local);
    let end_str = format_timestamp_full(scan.end_time, use_local);
    ui.label(format!("Start: {}", start_str));
    ui.label(format!("End:   {} ({:.0}s)", end_str, duration));

    if elev_count > 0 {
        ui.label(format!("Elevations: {} sweeps", elev_count));
    }

    // Completeness
    let completeness_str = match scan.completeness {
        Some(ScanCompleteness::Complete) => "Complete",
        Some(ScanCompleteness::PartialWithVcp) => "Partial (VCP known)",
        Some(ScanCompleteness::PartialNoVcp) => "Partial (no VCP)",
        Some(ScanCompleteness::Missing) => "Missing",
        None => "Unknown",
    };
    if let (Some(present), Some(expected)) = (scan.present_records, scan.expected_records) {
        ui.label(format!(
            "Records: {}/{} ({})",
            present, expected, completeness_str
        ));
    } else {
        ui.label(format!("Status: {}", completeness_str));
    }

    // Live mode info if this scan matches the active volume
    if live_state.is_active() {
        if let Some(vol_start) = live_state.current_volume_start {
            if (scan.start_time - vol_start).abs() < 30.0 {
                ui.separator();
                let received = live_state.elevations_received.len();
                let expected = live_state.expected_elevation_count.unwrap_or(0);
                ui.label(
                    RichText::new(format!(
                        "Live: {}/{} elevations received",
                        received, expected
                    ))
                    .color(Color32::from_rgb(100, 200, 100)),
                );
            }
        }
    }
}
