//! Bottom panel UI: playback controls, timeline, and session statistics.

use super::colors::{live, timeline as tl_colors, ui as ui_colors};
use crate::data::ScanCompleteness;
use crate::state::radar_data::RadarTimeline;
use crate::state::{AppState, LiveExitReason, LivePhase, LoopMode, PlaybackSpeed};
use chrono::{Datelike, TimeZone, Timelike, Utc};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

/// Get current Unix timestamp in seconds.
#[allow(dead_code)] // Utility function for UI timing
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
                    painter.rect_stroke(scan_rect, 2.0, Stroke::new(1.0, border), StrokeKind::Inside);
                } else {
                    painter.rect_filled(scan_rect, 2.0, fill);
                    painter.rect_stroke(scan_rect, 2.0, Stroke::new(1.0, border), StrokeKind::Inside);

                    // Hatch pattern for PartialWithVcp
                    if scan.completeness == Some(ScanCompleteness::PartialWithVcp) {
                        let hatch_color = tl_colors::scan_hatch(scan.vcp);
                        let spacing = 6.0;
                        let mut offset = 0.0;
                        while offset < width + scan_rect.height() {
                            let x0 = scan_rect.left() + offset;
                            let y0 = scan_rect.top();
                            let x1 = x0 - scan_rect.height();
                            let y1 = scan_rect.bottom();
                            painter.line_segment(
                                [
                                    Pos2::new(x0.max(scan_rect.left()).min(scan_rect.right()), y0.max(scan_rect.top())),
                                    Pos2::new(x1.max(scan_rect.left()).min(scan_rect.right()), y1.min(scan_rect.bottom())),
                                ],
                                Stroke::new(0.5, hatch_color),
                            );
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
                        let (p, e) = (scan.present_records.unwrap(), scan.expected_records.unwrap());
                        if width > 120.0 {
                            format!("VCP {} {}/{}", scan.vcp, p, e)
                        } else {
                            format!("{} {}/{}", scan.vcp, p, e)
                        }
                    } else if width > 100.0 {
                        let elev_count = scan.vcp_pattern.as_ref()
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
                let stroke_kind = if is_active { StrokeKind::Outside } else { StrokeKind::Inside };
                painter.rect_stroke(sweep_rect, 1.0, Stroke::new(stroke_width, border), stroke_kind);
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
                        if let Some(vcp_elev) = elevs.get(sweep.elevation_number.saturating_sub(1) as usize) {
                            let products = match vcp_elev.waveform.as_str() {
                                "CS" | "ContiguousSurveillance" => "R",
                                "CDW" | "CDWO" | "ContiguousDopplerWithGating" | "ContiguousDopplerWithoutGating" => "V",
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

pub fn render_bottom_panel(ctx: &egui::Context, state: &mut AppState) {
    let dt = ctx.input(|i| i.stable_dt);

    // Update live mode pulse animation
    state.live_mode_state.update_pulse(dt);

    // Handle spacebar to toggle playback (only when no text input is focused)
    let space_pressed = ctx.input(|i| i.key_pressed(egui::Key::Space) && !i.modifiers.any());
    let has_focus = ctx.memory(|m| m.focused().is_some());
    if space_pressed && !has_focus {
        if state.playback_state.playing {
            // Stop - also exits live mode if active
            if state.live_mode_state.is_active() {
                state.live_mode_state.stop(LiveExitReason::UserStopped);
                state.playback_state.time_model.disable_realtime_lock();
                state.status_message = state
                    .live_mode_state
                    .last_exit_reason
                    .map(|r| r.message().to_string())
                    .unwrap_or_default();
            }
            state.playback_state.playing = false;
        } else {
            // Only allow playback if zoom permits
            if state.playback_state.is_playback_allowed() {
                state.playback_state.playing = true;
            }
        }
    }

    // Advance playback position when playing
    // The time_model handles real-time lock mode internally
    if state.playback_state.playing {
        state.playback_state.advance(dt as f64);

        // Pin playback position on the visible timeline during playback.
        // In live/real-time mode, pin at 75% from left (right quarter) so more
        // history is visible. In archive playback, pin at 25% from left.
        let view_width_secs = state.playback_state.view_width_secs();
        if view_width_secs > 0.0 {
            let pin_fraction = if state.live_mode_state.is_active() {
                0.75
            } else {
                0.25
            };
            let target_offset = view_width_secs * pin_fraction;
            let pos = state.playback_state.playback_position();
            state.playback_state.timeline_view_start = pos - target_offset;
        }

        // Request continuous repaint while playing
        ctx.request_repaint();
    }

    egui::TopBottomPanel::bottom("bottom_panel")
        .exact_height(104.0)
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                // Mode and acquisition status bar
                ui.horizontal(|ui| {
                    let mode_label = if state.live_mode_state.is_active() {
                        "REAL-TIME"
                    } else {
                        "NAVIGATE"
                    };
                    let mode_color = if state.live_mode_state.is_active() {
                        live::STREAMING
                    } else {
                        ui_colors::label(state.is_dark)
                    };
                    ui.label(
                        RichText::new(mode_label)
                            .size(10.0)
                            .strong()
                            .color(mode_color),
                    );

                    // Show data staleness if available
                    if let Some(staleness) = state.viz_state.data_staleness_secs {
                        ui.separator();
                        let age_text = if staleness < 60.0 {
                            format!("{:.0}s old", staleness)
                        } else if staleness < 3600.0 {
                            format!("{:.0}m old", staleness / 60.0)
                        } else {
                            format!("{:.1}h old", staleness / 3600.0)
                        };
                        let age_color = if staleness < 60.0 {
                            ui_colors::SUCCESS
                        } else if staleness < 300.0 {
                            ui_colors::ACTIVE
                        } else {
                            Color32::from_rgb(220, 80, 80)
                        };
                        ui.label(RichText::new(age_text).size(10.0).color(age_color));
                    }
                });

                // Timeline row
                ui.add_space(2.0);
                render_timeline(ui, state);

                ui.add_space(2.0);

                // Playback controls row
                ui.horizontal(|ui| {
                    render_playback_controls(ui, state);
                });
            });
        });

    // Stats detail is now a proper modal rendered from main.rs via render_stats_modal.
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
            1 => "Jan", 2 => "Feb", 3 => "Mar", 4 => "Apr",
            5 => "May", 6 => "Jun", 7 => "Jul", 8 => "Aug",
            9 => "Sep", 10 => "Oct", 11 => "Nov", 12 => "Dec",
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

fn render_timeline(ui: &mut egui::Ui, state: &mut AppState) {
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

    // Track heights — sweep track only shown at Sweeps detail level
    // Scan track needs enough room for: block fills (top) + time tick labels (bottom)
    let scan_track_h: f32 = if detail_level == DetailLevel::Sweeps { 24.0 } else { 40.0 };
    let sweep_track_h: f32 = if detail_level == DetailLevel::Sweeps { 22.0 } else { 0.0 };
    let separator_h: f32 = if detail_level == DetailLevel::Sweeps { 1.0 } else { 0.0 };
    let vcp_track_h: f32 = 6.0;
    let timeline_height = scan_track_h + separator_h + sweep_track_h + vcp_track_h;

    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_width as f32, timeline_height),
        Sense::click_and_drag(),
    );
    let full_rect = response.rect;

    // Sub-rects for each track
    let scan_rect = Rect::from_min_max(
        full_rect.min,
        Pos2::new(full_rect.max.x, full_rect.min.y + scan_track_h),
    );
    let sweep_rect = if detail_level == DetailLevel::Sweeps {
        Rect::from_min_max(
            Pos2::new(full_rect.min.x, scan_rect.max.y + separator_h),
            Pos2::new(full_rect.max.x, scan_rect.max.y + separator_h + sweep_track_h),
        )
    } else {
        Rect::NOTHING // not used
    };
    let vcp_rect = Rect::from_min_max(
        Pos2::new(full_rect.min.x, full_rect.max.y - vcp_track_h),
        full_rect.max,
    );

    let dark = state.is_dark;

    // Background for scan track
    painter.rect_filled(scan_rect, 2.0, tl_colors::background(dark));
    painter.rect_stroke(scan_rect, 2.0, Stroke::new(1.0, tl_colors::border(dark)), StrokeKind::Outside);

    // Background for sweep track (when visible)
    if detail_level == DetailLevel::Sweeps {
        painter.rect_filled(sweep_rect, 0.0, tl_colors::background(dark));
        painter.rect_stroke(sweep_rect, 0.0, Stroke::new(0.5, tl_colors::border(dark)), StrokeKind::Outside);
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

    // ── Render scan track ─────────────────────────────────────────────
    // Extract the scan key timestamp (seconds) for the active real-time volume
    // so we can skip it in normal timeline rendering.
    let active_scan_key_ts: Option<f64> = if state.live_mode_state.is_active() {
        state.live_mode_state.current_scan_key.as_ref().and_then(|key| {
            // Scan key format: "SITE|TIMESTAMP_MS"
            key.split('|').nth(1)?.parse::<i64>().ok().map(|ms| ms as f64 / 1000.0)
        })
    } else {
        None
    };
    render_scan_track(
        &painter, &scan_rect, &state.radar_timeline,
        view_start, view_end, zoom, detail_level, active_scan_key_ts,
    );

    // ── Render sweep track (only at Sweeps detail) ────────────────────
    if detail_level == DetailLevel::Sweeps {
        render_sweep_track(
            &painter, &sweep_rect, &state.radar_timeline,
            view_start, view_end, zoom, active_sweep,
            state.viz_state.target_elevation, active_scan_key_ts,
        );
        render_connector_lines(
            &painter, &scan_rect, &sweep_rect, &state.radar_timeline,
            view_start, view_end, zoom,
        );
    }

    // ── Render ghost markers for pending downloads ────────────────────
    if state.download_progress.is_active() {
        let anim_time = ui.ctx().input(|i| i.time);
        render_download_ghosts(
            &painter, &scan_rect, &state.download_progress,
            &state.radar_timeline, view_start, view_end, zoom,
            detail_level, anim_time,
        );
        ui.ctx().request_repaint();
    }

    // ── Render real-time partial scan progress ────────────────────────
    // Compute `now` once per frame so render + tooltip use a consistent boundary.
    let frame_now_secs = js_sys::Date::now() / 1000.0;
    if state.live_mode_state.is_active() {
        let anim_time = ui.ctx().input(|i| i.time);
        render_realtime_progress(
            &painter, &scan_rect,
            if detail_level == DetailLevel::Sweeps { Some(&sweep_rect) } else { None },
            &state.live_mode_state, view_start, view_end, zoom, anim_time,
            frame_now_secs, state.viz_state.target_elevation, active_sweep,
        );
        ui.ctx().request_repaint();
    }

    // ── VCP info track ────────────────────────────────────────────────
    {
        let vcp_color = |vcp: u16| -> Color32 {
            let (r, g, b) = tl_colors::vcp_base_rgb(vcp);
            Color32::from_rgb(r, g, b)
        };

        painter.rect_filled(vcp_rect, 0.0, Color32::from_rgb(22, 22, 30));

        for scan in state.radar_timeline.scans_in_range(view_start, view_end) {
            let x_start = ts_to_x(scan.start_time).max(vcp_rect.left());
            let x_end = ts_to_x(scan.end_time).min(vcp_rect.right());

            if x_end > x_start {
                let bar_rect = Rect::from_min_max(
                    Pos2::new(x_start, vcp_rect.top() + 1.0),
                    Pos2::new(x_end, vcp_rect.bottom() - 1.0),
                );
                painter.rect_filled(bar_rect, 0.0, vcp_color(scan.vcp));
            }
        }

        if detail_level != DetailLevel::Solid {
            let mut last_vcp = 0u16;
            for scan in state.radar_timeline.scans_in_range(view_start, view_end) {
                if scan.vcp != last_vcp && scan.vcp > 0 {
                    last_vcp = scan.vcp;
                    let x = ts_to_x(scan.start_time);
                    if x >= vcp_rect.left() && x <= vcp_rect.right() - 30.0 {
                        painter.text(
                            Pos2::new(x + 2.0, vcp_rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("{}", scan.vcp),
                            egui::FontId::monospace(7.0),
                            Color32::from_rgba_unmultiplied(220, 220, 240, 180),
                        );
                    }
                }
            }
        }
    }

    // Track headers removed — the tracks are self-evident from content and
    // color palette, and the headers overlapped with scan/sweep block labels.
    // Hover tooltips provide the educational labeling instead.

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

    // Draw tick marks on scan track
    let mut tick = first_tick;
    while tick <= last_tick {
        let x = ts_to_x(tick as f64);

        if x >= scan_rect.left() && x <= scan_rect.right() {
            let local_tick = tick + tz_offset_secs;
            let is_major = local_tick % major_interval == 0;
            let tick_height = if is_major { 10.0 } else { 5.0 };
            let tick_color = if is_major {
                tl_colors::tick_major(dark)
            } else {
                tl_colors::tick_minor(dark)
            };

            painter.line_segment(
                [
                    Pos2::new(x, scan_rect.bottom() - tick_height),
                    Pos2::new(x, scan_rect.bottom()),
                ],
                Stroke::new(1.0, tick_color),
            );

            // Draw label for major ticks — positioned at bottom of scan track
            // to avoid overlapping with scan block content (VCP labels, fraction labels)
            if is_major {
                let label = format_timestamp(tick, tick_config, use_local);
                painter.text(
                    Pos2::new(x, scan_rect.bottom() - tick_height - 1.0),
                    egui::Align2::CENTER_BOTTOM,
                    label,
                    egui::FontId::monospace(9.0),
                    tl_colors::tick_label(dark),
                );
            }
        }

        tick += minor_interval;
    }

    // The overlay_rect spans all data tracks (scan + sweep, not VCP)
    let overlay_rect = Rect::from_min_max(
        scan_rect.min,
        Pos2::new(
            scan_rect.max.x,
            if detail_level == DetailLevel::Sweeps { sweep_rect.max.y } else { scan_rect.max.y },
        ),
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
            painter.rect_filled(range_rect, 0.0, Color32::from_rgba_unmultiplied(100, 150, 255, 40));

            if start_x >= overlay_rect.left() && start_x <= overlay_rect.right() {
                painter.line_segment(
                    [Pos2::new(start_x, overlay_rect.top()), Pos2::new(start_x, overlay_rect.bottom())],
                    Stroke::new(1.5, Color32::from_rgb(100, 150, 255)),
                );
            }
            if end_x >= overlay_rect.left() && end_x <= overlay_rect.right() {
                painter.line_segment(
                    [Pos2::new(end_x, overlay_rect.top()), Pos2::new(end_x, overlay_rect.bottom())],
                    Stroke::new(1.5, Color32::from_rgb(100, 150, 255)),
                );
            }
        }
    }

    // Draw selection marker (playback position indicator)
    {
        let selected_ts = state.playback_state.playback_position();
        let sel_x = ts_to_x(selected_ts);

        if sel_x >= overlay_rect.left() && sel_x <= overlay_rect.right() {
            let marker_color = tl_colors::SELECTION;

            painter.line_segment(
                [Pos2::new(sel_x, overlay_rect.top()), Pos2::new(sel_x, overlay_rect.bottom())],
                Stroke::new(2.0, marker_color),
            );

            let triangle = vec![
                Pos2::new(sel_x - 5.0, overlay_rect.top()),
                Pos2::new(sel_x + 5.0, overlay_rect.top()),
                Pos2::new(sel_x, overlay_rect.top() + 8.0),
            ];
            painter.add(egui::Shape::convex_polygon(triangle, marker_color, Stroke::NONE));
        }
    }

    // Draw "now" marker (current wall-clock time)
    {
        let now_ts = current_timestamp_secs();
        let now_x = ts_to_x(now_ts);

        if now_x >= overlay_rect.left() && now_x <= overlay_rect.right() {
            let now_color = tl_colors::NOW_MARKER;

            painter.line_segment(
                [Pos2::new(now_x, overlay_rect.top()), Pos2::new(now_x, overlay_rect.top() + 4.0)],
                Stroke::new(1.5, now_color),
            );
            painter.line_segment(
                [Pos2::new(now_x, overlay_rect.bottom() - 4.0), Pos2::new(now_x, overlay_rect.bottom())],
                Stroke::new(1.5, now_color),
            );
            painter.line_segment(
                [Pos2::new(now_x, overlay_rect.top() + 4.0), Pos2::new(now_x, overlay_rect.bottom() - 4.0)],
                Stroke::new(0.5, Color32::from_rgba_unmultiplied(now_color.r(), now_color.g(), now_color.b(), 100)),
            );
            let d = 3.0;
            let diamond = vec![
                Pos2::new(now_x, overlay_rect.bottom() - d),
                Pos2::new(now_x + d, overlay_rect.bottom()),
                Pos2::new(now_x, overlay_rect.bottom() + d),
                Pos2::new(now_x - d, overlay_rect.bottom()),
            ];
            painter.add(egui::Shape::convex_polygon(diamond, now_color, Stroke::NONE));
        }
    }

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

            let center_x = ((start_x + end_x) / 2.0).clamp(scan_rect.left() + 20.0, scan_rect.right() - 20.0);
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
            render_timeline_tooltip(ui, &state.radar_timeline, &state.live_mode_state, hover_ts, hover_pos, &scan_rect, &sweep_rect, detail_level, use_local, frame_now_secs);
        }
    }

    // ── Interaction handling ──────────────────────────────────────────
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
                let new_view_start = cursor_ts - (cursor_pos.x - full_rect.left()) as f64 / new_zoom;
                state.playback_state.timeline_view_start = new_view_start;
            }

            state.playback_state.timeline_zoom = new_zoom;
        }
    }
}

/// Format a timestamp (f64 unix seconds) for display with sub-second precision
fn format_timestamp_full(ts: f64, use_local: bool) -> String {
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

/// Render the datetime picker popup for jumping to a specific time.
fn render_datetime_picker_popup(ui: &mut egui::Ui, state: &mut AppState) {
    if !state.datetime_picker.open {
        return;
    }

    let use_local = state.use_local_time;
    let tz_label = if use_local { "Local" } else { "UTC" };
    let popup_id = ui.make_persistent_id("datetime_picker_popup");

    egui::Area::new(popup_id)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(280.0);

                ui.vertical(|ui| {
                    ui.heading(format!("Jump to Date/Time ({tz_label})"));
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
                        ui.label(tz_label);
                    });

                    ui.add_space(12.0);

                    // Validation feedback
                    let valid_ts = state.datetime_picker.to_timestamp(use_local);
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
                                    state.playback_state.set_playback_position(ts);

                                    // Left-align timeline view on new position
                                    // Place the jumped-to position at ~5% from the left edge
                                    let view_width_secs = state.playback_state.view_width_secs();
                                    state.playback_state.timeline_view_start =
                                        ts - view_width_secs * 0.05;

                                    // Exit live mode if active
                                    if state.live_mode_state.is_active() {
                                        state.live_mode_state.stop(LiveExitReason::UserSeeked);
                                        state.playback_state.time_model.disable_realtime_lock();
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
    let use_local = state.use_local_time;

    // Current position timestamp display (clickable to open datetime picker)
    {
        let selected_ts = state.playback_state.playback_position();
        let tz_suffix = if use_local { "" } else { " Z" };
        let timestamp_btn = ui.add(
            egui::Button::new(
                RichText::new(format!("{}{}", format_timestamp_full(selected_ts, use_local), tz_suffix))
                    .monospace()
                    .size(13.0)
                    .color(tl_colors::SELECTION),
            )
            .frame(false),
        );

        if timestamp_btn.clicked() {
            state.datetime_picker.init_from_timestamp(selected_ts, use_local);
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
                state.playback_state.time_model.disable_realtime_lock();
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
            state.playback_state.time_model.disable_realtime_lock();
            state.status_message = state
                .live_mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        }
        let new_pos = state.playback_state.playback_position() - jog_amount;
        state.playback_state.set_playback_position(new_pos);
    }

    // Step forward
    if ui.button(RichText::new("\u{25B6}").size(14.0)).clicked() {
        // ▶
        // Exit live mode when jogging
        if state.live_mode_state.is_active() {
            state.live_mode_state.stop(LiveExitReason::UserJogged);
            state.playback_state.time_model.disable_realtime_lock();
            state.status_message = state
                .live_mode_state
                .last_exit_reason
                .map(|r| r.message().to_string())
                .unwrap_or_default();
        }
        let new_pos = state.playback_state.playback_position() + jog_amount;
        state.playback_state.set_playback_position(new_pos);
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

    // Loop mode selector (only show when playback bounds are set)
    if state.playback_state.time_model.playback_bounds.is_some() {
        ui.separator();
        ui.label(RichText::new("Loop:").size(11.0));
        egui::ComboBox::from_id_salt("loop_mode_selector")
            .selected_text(state.playback_state.time_model.loop_mode.label())
            .width(70.0)
            .show_ui(ui, |ui| {
                for mode in LoopMode::all() {
                    ui.selectable_value(
                        &mut state.playback_state.time_model.loop_mode,
                        *mode,
                        mode.label(),
                    );
                }
            });

        // Clear selection button
        if ui
            .small_button("\u{2715}") // ×
            .on_hover_text("Clear selection and playback bounds")
            .clicked()
        {
            state.playback_state.clear_selection();
        }
    }

    ui.separator();

    // Download button
    let has_selection = state.playback_state.selection_range().is_some();
    let download_in_progress = state.download_selection_in_progress;

    if download_in_progress {
        let label = if state.download_progress.is_batch() {
            format!(
                "Downloading {}/{}...",
                (state.download_progress.batch_completed + 1)
                    .min(state.download_progress.batch_total),
                state.download_progress.batch_total
            )
        } else {
            "Downloading...".to_string()
        };
        ui.add_enabled(
            false,
            egui::Button::new(RichText::new(label).size(11.0)),
        );
    } else if has_selection {
        if ui
            .button(RichText::new("Download Selection").size(11.0))
            .on_hover_text("Download all scans in the selected time range")
            .clicked()
        {
            state.download_selection_requested = true;
        }
    } else if ui
        .button(RichText::new("\u{2B07} Download").size(11.0))
        .on_hover_text("Download the scan at the current playback position")
        .clicked()
    {
        state.download_at_position_requested = true;
    }

    ui.separator();

    // UTC/Local toggle
    {
        let label = if state.use_local_time { "Local" } else { "UTC" };
        if ui
            .button(RichText::new(label).size(10.0).monospace())
            .on_hover_text("Toggle between UTC and local time")
            .clicked()
        {
            state.use_local_time = !state.use_local_time;
        }
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
    let now = state.playback_state.playback_position();

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
                        .color(ui_colors::value(state.is_dark)),
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
///
/// Layout (right-to-left): FPS | pipeline (clickable) | download | cache
fn render_session_stats(ui: &mut egui::Ui, state: &mut AppState) {
    let dark = state.is_dark;

    // FPS (rightmost) — read value before mutable borrow
    let fps = state.session_stats.avg_fps;
    let active_count = state.session_stats.active_request_count;
    let request_count = state.session_stats.session_request_count;
    let transferred = state.session_stats.format_transferred();
    let cache_size = state.session_stats.format_cache_size();

    if let Some(fps) = fps {
        ui.label(
            RichText::new(format!("{:.0} fps", fps))
                .size(11.0)
                .color(ui_colors::value(dark)),
        );
        ui.separator();
    }

    // Pipeline status — clickable phase boxes open detail modal
    render_pipeline_indicator(ui, state);

    // Download group: requests + transferred
    if active_count > 0 {
        ui.label(
            RichText::new(format!("({} active)", active_count))
                .size(10.0)
                .italics()
                .color(ui_colors::ACTIVE),
        );
    }
    if request_count > 0 {
        ui.label(
            RichText::new(format!("{} req / {}", request_count, transferred))
                .size(10.0)
                .color(ui_colors::value(dark)),
        );
        ui.separator();
    }

    // Cache group: size with clear button
    if ui.small_button("x").on_hover_text("Clear cache").clicked() {
        state.clear_cache_requested = true;
    }
    ui.label(
        RichText::new(cache_size)
            .size(10.0)
            .color(ui_colors::value(dark)),
    );
}

/// Render ghost blocks on the scan track for pending/active/processing downloads.
///
/// Distinct visual styles per state:
/// - Pending (queued): blue outline with diagonal stripe pattern
/// - Active (downloading): pulsing blue fill
/// - Processing (in_flight after download): amber tint
/// - Recently completed: brief green flash
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
        let all: Vec<_> = progress.pending_scans.iter()
            .chain(progress.in_flight_scans.iter())
            .copied().collect();
        if all.is_empty() { return; }
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
        if age > 1.0 { continue; }
        let flash_alpha = ((1.0 - age) * 80.0) as u8;
        // Find this scan's end time from timeline
        if let Some(scan) = timeline.scans_in_range(scan_start as f64, scan_start as f64 + 600.0)
            .find(|s| (s.start_time as i64 - scan_start).abs() < 30)
        {
            let x_start = ts_to_x(scan.start_time).max(rect.left());
            let x_end = ts_to_x(scan.end_time).min(rect.right());
            if x_end > x_start {
                let flash_rect = Rect::from_min_max(
                    Pos2::new(x_start, rect.top() + 2.0),
                    Pos2::new(x_end, rect.bottom() - 2.0),
                );
                painter.rect_filled(flash_rect, 2.0,
                    Color32::from_rgba_unmultiplied(100, 220, 120, flash_alpha));
            }
        }
    }

    // Helper: draw a ghost block for a scan boundary
    let draw_ghost = |scan_start: i64, scan_end: i64, is_active: bool, is_processing: bool| {
        let start_f64 = scan_start as f64;
        let end_f64 = scan_end as f64;
        if end_f64 < view_start || start_f64 > view_end { return; }

        // Skip if real data already covers this timestamp
        if timeline.scans_in_range(start_f64, end_f64)
            .any(|s| s.start_time <= start_f64 + 30.0 && s.end_time >= start_f64 - 30.0)
        { return; }

        let x_start = ts_to_x(start_f64).max(rect.left());
        let x_end = ts_to_x(end_f64).min(rect.right());
        if x_end <= x_start || (x_end - x_start) < 1.0 { return; }

        let ghost_rect = Rect::from_min_max(
            Pos2::new(x_start, rect.top() + 2.0),
            Pos2::new(x_end, rect.bottom() - 2.0),
        );

        if is_active {
            // Active download: pulsing blue fill
            let pulse = (0.5 + 0.5 * (anim_time * 3.0).sin()) as f32;
            let fill_alpha = (35.0 + 30.0 * pulse) as u8;
            let border_alpha = (60.0 + 35.0 * pulse) as u8;
            painter.rect_filled(ghost_rect, 2.0,
                Color32::from_rgba_unmultiplied(100, 160, 255, fill_alpha));
            painter.rect_stroke(ghost_rect, 2.0,
                Stroke::new(1.5, Color32::from_rgba_unmultiplied(100, 160, 255, border_alpha)),
                StrokeKind::Inside);
        } else if is_processing {
            // Processing (ingesting): amber tint with subtle pulse
            let pulse = (0.5 + 0.5 * (anim_time * 2.0).sin()) as f32;
            let fill_alpha = (30.0 + 20.0 * pulse) as u8;
            painter.rect_filled(ghost_rect, 2.0,
                Color32::from_rgba_unmultiplied(200, 160, 60, fill_alpha));
            painter.rect_stroke(ghost_rect, 2.0,
                Stroke::new(1.0, tl_colors::ghost_processing_border()), StrokeKind::Inside);
        } else {
            // Pending: blue outline with diagonal stripe pattern
            painter.rect_stroke(ghost_rect, 2.0,
                Stroke::new(1.0, tl_colors::ghost_pending_border()), StrokeKind::Inside);
            // Diagonal stripes
            let width = x_end - x_start;
            let height = ghost_rect.height();
            let spacing = 8.0;
            let mut offset = 0.0;
            while offset < width + height {
                let x0 = ghost_rect.left() + offset;
                let y0 = ghost_rect.top();
                let x1 = x0 - height;
                let y1 = ghost_rect.bottom();
                painter.line_segment(
                    [
                        Pos2::new(x0.max(ghost_rect.left()).min(ghost_rect.right()), y0),
                        Pos2::new(x1.max(ghost_rect.left()).min(ghost_rect.right()), y1),
                    ],
                    Stroke::new(0.5, tl_colors::ghost_pending_fill()),
                );
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
///   Each non-complete sweep shows chunk subdivision where downloaded chunks
///   are clipped to the sweep's time range.
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
        painter.rect_filled(elapsed_rect, 2.0,
            Color32::from_rgba_unmultiplied(vr, vg, vb, 160));
    }

    // Projected remainder: very subtle fill
    if x_vol_end > x_now && x_now >= scan_rect.left() {
        let future_rect = Rect::from_min_max(
            Pos2::new(x_now, scan_block.min.y),
            scan_block.max,
        );
        painter.rect_filled(future_rect, 2.0,
            Color32::from_rgba_unmultiplied(vr, vg, vb, 40));
    }

    // Border: solid on elapsed side, dashed on projected side
    // Left + top/bottom edges for elapsed portion
    if x_now > x_vol_start {
        let elapsed_rect = Rect::from_min_max(
            scan_block.min,
            Pos2::new(x_now.min(x_vol_end), scan_block.max.y),
        );
        painter.rect_stroke(elapsed_rect, 2.0,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(vr, vg, vb, 180)),
            StrokeKind::Inside);
    }
    // Dashed border for projected remainder
    if x_vol_end > x_now && x_now >= scan_rect.left() {
        let dash_color = Color32::from_rgba_unmultiplied(vr, vg, vb, 60);
        // Dashed right edge
        let mut y = scan_block.min.y;
        while y < scan_block.max.y {
            let y_end = (y + 3.0).min(scan_block.max.y);
            painter.line_segment(
                [Pos2::new(x_vol_end, y), Pos2::new(x_vol_end, y_end)],
                Stroke::new(0.5, dash_color),
            );
            y += 6.0;
        }
        // Dashed top and bottom
        let mut x = x_now;
        while x < x_vol_end {
            let x_seg_end = (x + 4.0).min(x_vol_end);
            painter.line_segment(
                [Pos2::new(x, scan_block.min.y), Pos2::new(x_seg_end, scan_block.min.y)],
                Stroke::new(0.5, dash_color),
            );
            painter.line_segment(
                [Pos2::new(x, scan_block.max.y), Pos2::new(x_seg_end, scan_block.max.y)],
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
                egui::FontId::monospace(7.0),
                Color32::from_rgba_unmultiplied(220, 240, 220, 180),
            );
        }
    }

    // ── Projected future scan boundaries (dashed lines) ──
    if expected_dur > 30.0 {
        for i in 1..=2 {
            let projected_ts = vol_start + expected_dur * i as f64;
            let x = ts_to_x(projected_ts);
            if x >= scan_rect.left() && x <= scan_rect.right() {
                let mut y = scan_rect.top();
                while y < scan_rect.bottom() {
                    let y_end = (y + 3.0).min(scan_rect.bottom());
                    painter.line_segment(
                        [Pos2::new(x, y), Pos2::new(x, y_end)],
                        Stroke::new(0.5, tl_colors::estimated_boundary()),
                    );
                    y += 6.0;
                }
                painter.text(
                    Pos2::new(x + 2.0, scan_rect.top() + 2.0),
                    egui::Align2::LEFT_TOP,
                    "est.",
                    egui::FontId::monospace(6.0),
                    tl_colors::estimated_boundary(),
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

    // Estimated sweep duration (for non-completed sweeps that lack real timestamps)
    let sweep_dur = expected_dur / expected_count as f64;

    let received = &live_state.elevations_received;
    let in_progress_elev = live_state.current_in_progress_elevation;
    let in_progress_radials = live_state.current_in_progress_radials.unwrap_or(0);
    let countdown = live_state.countdown_remaining_secs(now);

    for elev_idx in 0..expected_count {
        let elev_num = (elev_idx + 1) as u8;
        let is_complete = received.contains(&elev_num);

        // Use actual timestamps where available:
        // 1. Completed sweep → use SweepMeta start/end
        // 2. In-progress sweep with chunk data → derive bounds from chunk spans
        // 3. Future sweep → estimate from last known anchor point
        let (sw_start, sw_end) = if is_complete {
            if let Some(meta) = live_state.completed_sweep_metas.iter()
                .find(|m| m.elevation_number == elev_num)
            {
                (meta.start, meta.end)
            } else {
                (vol_start + elev_idx as f64 * sweep_dur, vol_start + (elev_idx + 1) as f64 * sweep_dur)
            }
        } else {
            // For non-completed sweeps, find the best anchor: the end time of
            // the highest completed sweep below this one.
            let anchor_end = live_state.completed_sweep_metas.iter()
                .filter(|m| m.elevation_number < elev_num)
                .max_by_key(|m| m.elevation_number)
                .map(|m| m.end);

            // Also check if we have actual chunk data for this elevation
            let chunk_min = live_state.chunk_elev_spans.iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .map(|&(_, s, _, _)| s)
                .reduce(f64::min);
            let chunk_max = live_state.chunk_elev_spans.iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .map(|&(_, _, e, _)| e)
                .reduce(f64::max);

            let sw_start_actual = match (chunk_min, anchor_end) {
                // Have chunk data: use actual chunk start as sweep start
                (Some(cm), _) => cm,
                // No chunk data but have anchor: estimate from anchor
                (None, Some(ae)) => {
                    let completed_count = live_state.completed_sweep_metas.iter()
                        .filter(|m| m.elevation_number < elev_num)
                        .count();
                    let steps = elev_idx - completed_count;
                    let remaining_count = expected_count - completed_count;
                    let remaining_dur = (vol_start + expected_dur) - ae;
                    let est_dur = if remaining_count > 0 { remaining_dur / remaining_count as f64 } else { sweep_dur };
                    ae + steps as f64 * est_dur
                }
                // No data at all: even distribution
                (None, None) => vol_start + elev_idx as f64 * sweep_dur,
            };

            let est_sweep_end = sw_start_actual + sweep_dur;
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
            // ── Complete: filled with cool elevation colors ──
            let is_active = active_sweep.is_some_and(|(_, active_en)| active_en == elev_num);
            let fill = tl_colors::sweep_fill(elev_angle, matches_target);
            let border = tl_colors::sweep_border(elev_angle, is_active);
            painter.rect_filled(block, 1.0, fill);
            if width > 3.0 {
                let stroke_width = if is_active { 2.0 } else { 0.5 };
                let stroke_kind = if is_active { StrokeKind::Outside } else { StrokeKind::Inside };
                painter.rect_stroke(block, 1.0, Stroke::new(stroke_width, border), stroke_kind);
            }
        } else if is_downloading {
            // ── Downloading: outline with chunk subdivision ──
            let border_color = Color32::from_rgba_unmultiplied(60, 140, 200, 100);
            painter.rect_stroke(block, 1.0, Stroke::new(1.0, border_color), StrokeKind::Inside);

            // Draw downloaded chunks that belong to this elevation
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
                    painter.rect_filled(chunk_rect, 0.0,
                        Color32::from_rgba_unmultiplied(60, 140, 200, 55));
                }
            }

            // Next-chunk countdown if we're waiting
            if let Some(remaining) = countdown {
                if width > 20.0 {
                    painter.text(
                        block.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("~{}s", remaining.ceil() as i32),
                        egui::FontId::monospace(6.0),
                        Color32::from_rgba_unmultiplied(140, 200, 255, 160),
                    );
                }
            } else if width > 30.0 {
                // Show radial count while actively receiving
                painter.text(
                    block.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{}", in_progress_radials),
                    egui::FontId::monospace(6.0),
                    Color32::from_rgba_unmultiplied(140, 200, 255, 160),
                );
            }
        } else if is_future {
            // ── Future: dashed outline ──
            painter.rect_stroke(block, 1.0,
                Stroke::new(0.5, tl_colors::rt_pending_sweep_border()),
                StrokeKind::Inside);
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
                egui::FontId::monospace(7.0),
                Color32::from_rgba_unmultiplied(220, 230, 255, label_alpha),
            );
        }
    }
}

/// Render hover tooltip for timeline elements.
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
    let scan = timeline.scans_in_range(hover_ts - 0.5, hover_ts + 0.5)
        .find(|s| s.start_time <= hover_ts && s.end_time >= hover_ts);

    // Check if hovering within the active real-time volume (including projected future)
    let in_active_volume = scan.is_none() && live_state.is_active() && live_state.current_volume_start.is_some() && {
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
            if let Some(sw) = s.sweeps.iter().find(|sw| sw.start_time <= hover_ts && sw.end_time >= hover_ts) {
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
        hover_pos + egui::vec2(10.0, 10.0),
    ).show(|ui: &mut egui::Ui| {
        if let Some(sweep) = sweep {
            let parent_scan = sweep_parent_scan; // may differ from `scan` if sweep extends outside scan boundary
            // Sweep tooltip
            ui.label(RichText::new(format!("Elevation Sweep #{}", sweep.elevation_number)).strong().size(12.0));
            ui.label(RichText::new("One 360\u{00B0} rotation at a single antenna tilt angle.").size(10.0).weak());
            ui.separator();

            let sweep_count = parent_scan
                .and_then(|s| s.vcp_pattern.as_ref().map(|v| v.elevations.len()))
                .or_else(|| parent_scan.map(|s| s.sweeps.len()))
                .unwrap_or(0);
            if sweep_count > 0 {
                ui.label(format!("Elevation: {:.1}\u{00B0} (cut #{} of {})", sweep.elevation, sweep.elevation_number, sweep_count));
            } else {
                ui.label(format!("Elevation: {:.1}\u{00B0} (cut #{})", sweep.elevation, sweep.elevation_number));
            }

            let duration = sweep.end_time - sweep.start_time;
            let start_str = format_timestamp_full(sweep.start_time, use_local);
            let end_str = format_timestamp_full(sweep.end_time, use_local);
            ui.label(format!("Time: {} \u{2192} {} ({:.0}s)", start_str, end_str, duration));

            // Warn if sweep extends outside its parent scan
            if let Some(ps) = parent_scan {
                if sweep.start_time < ps.start_time || sweep.end_time > ps.end_time {
                    ui.label(RichText::new("Note: sweep time range extends outside its parent scan")
                        .size(9.0).italics()
                        .color(Color32::from_rgb(255, 200, 100)));
                }
            }

            // Waveform and products from VCP
            if let Some(vcp) = parent_scan.and_then(|s| s.vcp_pattern.as_ref()) {
                if let Some(vcp_elev) = vcp.elevations.get(sweep.elevation_number.saturating_sub(1) as usize) {
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
                        "CDW" | "CDWO" | "ContiguousDopplerWithGating" | "ContiguousDopplerWithoutGating" => "Velocity",
                        "B" | "Batch" => "Reflectivity / Velocity",
                        "SPP" | "StaggeredPulsePair" => "Reflectivity / Velocity / Differential",
                        _ => "Unknown",
                    };
                    ui.label(format!("Waveform: {}", wf_label));
                    ui.label(format!("Products: {}", products));

                    let mut flags = Vec::new();
                    if vcp_elev.is_sails { flags.push("SAILS"); }
                    if vcp_elev.is_mrle { flags.push("MRLE"); }
                    if vcp_elev.is_base_tilt { flags.push("Base Tilt"); }
                    if !flags.is_empty() {
                        ui.label(format!("Flags: {}", flags.join(", ")));
                    }
                }
            }
        } else if in_active_volume {
            // Tooltip for in-progress real-time volume (including projected future)
            let vol_start = live_state.current_volume_start.unwrap();
            let expected_dur = live_state.last_volume_duration_secs.unwrap_or(300.0);
            let expected_end = vol_start + expected_dur;
            let now = now_secs;
            let past_now = hover_ts > now;

            let vcp_num = live_state.current_vcp_number.unwrap_or(0);
            let vcp_label = if vcp_num > 0 { format!("VCP {}", vcp_num) } else { "Unknown VCP".to_string() };
            ui.label(RichText::new(format!("Volume Scan In Progress ({})", vcp_label)).strong().size(12.0));

            let mode_desc = match vcp_num {
                215 | 212 => "Precipitation Mode",
                31 | 32 | 35 => "Clear Air Mode",
                12 | 121 => "Severe Weather Mode",
                _ if vcp_num > 0 => "Known Mode",
                _ => "Unknown Mode",
            };
            ui.label(RichText::new(format!("Radar is actively collecting data. ({})", mode_desc)).size(10.0).weak());
            ui.separator();

            let start_str = format_timestamp_full(vol_start, use_local);
            ui.label(format!("Started: {}", start_str));
            // Round to whole seconds so text doesn't change every frame (avoids tooltip resize flicker)
            let elapsed = (now - vol_start).floor();
            let remaining = (expected_end - now).ceil();
            if remaining > 0.0 {
                ui.label(format!("Elapsed: {}s / est. {:.0}s total", elapsed as i64, expected_dur));
            } else {
                ui.label(format!("Elapsed: {}s (expected ~{:.0}s)", elapsed as i64, expected_dur));
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
                ui.label(RichText::new("Projected area \u{2014} data not yet collected")
                    .size(10.0).italics()
                    .color(Color32::from_rgba_unmultiplied(180, 200, 180, 160)));
                if remaining > 0.0 {
                    ui.label(format!("Est. ~{}s remaining", remaining as i64));
                }
            } else {
                ui.separator();
                ui.label(RichText::new(format!("Live: {}/{} elevations received", received, expected))
                    .color(Color32::from_rgb(100, 200, 100)));
            }
        } else if let Some(scan) = scan {
            // Scan tooltip (persisted data)
            let vcp_label = if scan.vcp > 0 { format!("VCP {}", scan.vcp) } else { "Unknown VCP".to_string() };
            ui.label(RichText::new(format!("Volume Scan ({})", vcp_label)).strong().size(12.0));

            let mode_desc = match scan.vcp {
                215 | 212 => "Precipitation Mode",
                31 | 32 | 35 => "Clear Air Mode",
                12 | 121 => "Severe Weather Mode",
                _ if scan.vcp > 0 => "Known Mode",
                _ => "Unknown Mode",
            };
            let elev_count = scan.vcp_pattern.as_ref().map(|v| v.elevations.len()).unwrap_or(scan.sweeps.len());
            let desc = if elev_count > 0 {
                format!("A complete 360\u{00B0} survey at {} elevation angles. ({})", elev_count, mode_desc)
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
                ui.label(format!("Records: {}/{} ({})", present, expected, completeness_str));
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
                        ui.label(RichText::new(format!("Live: {}/{} elevations received", received, expected))
                            .color(Color32::from_rgb(100, 200, 100)));
                    }
                }
            }
        }
    });

    let _ = scan_rect; // suppress unused warning when not in sweep mode
}

/// Render pipeline phase indicator boxes (3 high-level groups).
///
/// Shows a row of small clickable phase labels (DL, PROC, GPU). Active or
/// recently-completed phases are highlighted; idle ones are dimmed.
/// Clicking any phase opens the detailed stats modal.
/// The indicator stays visible for 1.5 s after the last phase completes
/// so the user can see which stages ran.
fn render_pipeline_indicator(ui: &mut egui::Ui, state: &mut AppState) {
    let pipeline = &state.session_stats.pipeline;
    let progress = &state.download_progress;
    let dark = state.is_dark;

    // Each entry: (label, is_lit)
    // "lit" means actively running OR recently completed (within linger window)
    let dl_lit = pipeline.phase_visible(pipeline.downloading > 0, pipeline.last_download_done_ms);
    let proc_lit =
        pipeline.phase_visible(pipeline.processing, pipeline.last_processing_done_ms);
    let gpu_lit = pipeline.phase_visible(pipeline.rendering, pipeline.last_render_done_ms);

    // Show batch count on DL when doing a multi-file download
    let dl_label: String = if progress.is_batch() {
        format!(
            "DL {}/{}",
            (progress.batch_completed + 1).min(progress.batch_total),
            progress.batch_total
        )
    } else if pipeline.downloading > 1 {
        "DL+".to_string()
    } else {
        "DL".to_string()
    };

    let phases: &[(&str, bool)] = &[
        (&dl_label, dl_lit),
        ("PROC", proc_lit),
        ("GPU", gpu_lit),
    ];

    // Also show compact latency summary after the indicator
    let has_any_timing = state.session_stats.median_chunk_latency_ms.is_some()
        || state.session_stats.median_processing_time_ms.is_some()
        || state.session_stats.avg_render_time_ms.is_some();

    let summary_text = if has_any_timing {
        Some(state.session_stats.format_latency_stats())
    } else {
        None
    };

    // Wider when showing batch count
    let base_width = if progress.is_batch() { 140.0 } else { 110.0 };
    let summary_width = summary_text.as_ref().map(|s| s.len() as f32 * 6.0 + 16.0).unwrap_or(0.0);
    let indicator_width = base_width + summary_width;

    // Use a fixed-width left-to-right sub-layout so phases read correctly
    // and don't consume all remaining horizontal space in the parent R-to-L layout.
    let mut clicked = false;
    ui.allocate_ui_with_layout(
        Vec2::new(indicator_width, ui.available_height()),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            let anim_time = ui.ctx().input(|i| i.time);
            let pulse = (0.5 + 0.5 * (anim_time * 3.0).sin()) as f32;

            for (i, (label, lit)) in phases.iter().enumerate() {
                if i > 0 {
                    ui.label(
                        RichText::new("\u{203A}")
                            .size(9.0)
                            .color(Color32::from_rgb(70, 70, 80)),
                    );
                }
                let color = if *lit {
                    // Pulse the active phase for visual emphasis
                    let base = ui_colors::ACTIVE;
                    let alpha = (180.0 + 75.0 * pulse) as u8;
                    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha)
                } else if dark {
                    Color32::from_rgb(55, 55, 65)
                } else {
                    Color32::from_rgb(180, 180, 190)
                };
                let btn = ui.add(
                    egui::Button::new(RichText::new(*label).size(9.0).monospace().color(color))
                        .frame(false),
                );
                if btn.clicked() {
                    clicked = true;
                }
                btn.on_hover_text("Click for detailed timing breakdown");
            }

            // Compact latency summary inline after the indicator
            if let Some(ref summary) = summary_text {
                ui.add_space(4.0);
                let btn = ui.add(
                    egui::Button::new(
                        RichText::new(summary)
                            .size(10.0)
                            .color(ui_colors::value(dark)),
                    )
                    .frame(false),
                );
                if btn.clicked() {
                    clicked = true;
                }
                btn.on_hover_text("Click for detailed timing breakdown");
            }
        },
    );

    if clicked {
        state.stats_detail_open = !state.stats_detail_open;
    }

    ui.separator();

    // Request repaint while lingering so phases fade out smoothly
    if pipeline.should_show() && !pipeline.is_active() {
        ui.ctx().request_repaint();
    }
    // Also repaint during batch downloads for pulse animation
    if progress.is_active() {
        ui.ctx().request_repaint();
    }
}

