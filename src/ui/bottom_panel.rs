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

/// Render radar data on the timeline at the appropriate detail level
fn render_radar_data(
    painter: &Painter,
    rect: &Rect,
    timeline: &RadarTimeline,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    detail_level: DetailLevel,
    active_sweep: Option<(i64, u8)>,
    target_elevation: f32,
) {
    // Helper to convert timestamp to x position
    let ts_to_x = |ts: f64| -> f32 { rect.left() + ((ts - view_start) * zoom) as f32 };

    // Color function based on VCP number with completeness as opacity modifier.
    // Different VCPs get distinct hues; incomplete scans get reduced opacity.
    let vcp_to_color = |vcp: u16, completeness: Option<ScanCompleteness>| -> (Color32, Color32) {
        // Base color by VCP category — muted tones to avoid overwhelming at zoom
        let (r, g, b) = match vcp {
            // Precipitation modes (VCP 21x) — muted green
            215 => (40, 90, 55),
            212 => (45, 85, 60),
            // Clear air modes (VCP 3x) — muted blue
            31 | 32 | 35 => (40, 70, 110),
            // Severe weather modes — muted orange
            12 | 121 => (130, 75, 40),
            // Other known VCPs — muted teal
            _ if vcp > 0 => (45, 80, 80),
            // Unknown (vcp == 0) — gray
            _ => (60, 60, 60),
        };

        // Opacity based on completeness
        let alpha = match completeness {
            Some(ScanCompleteness::Complete) | None => 200u8,
            Some(ScanCompleteness::PartialWithVcp) => 160,
            Some(ScanCompleteness::PartialNoVcp) => 120,
            Some(ScanCompleteness::Missing) => 60,
        };

        let fill = Color32::from_rgba_unmultiplied(r, g, b, alpha);
        let border_alpha = (alpha as u16 * 7 / 10) as u8;
        let border = Color32::from_rgba_unmultiplied(
            (r as u16 * 7 / 10) as u8,
            (g as u16 * 7 / 10) as u8,
            (b as u16 * 7 / 10) as u8,
            border_alpha,
        );
        (fill, border)
    };

    // Color function based on elevation angle (0-20 degrees typical range)
    // Muted palette: lower elevations are darker, higher are slightly brighter
    let elevation_to_color = |elevation: f32| -> Color32 {
        let t = (elevation / 20.0).clamp(0.0, 1.0);
        // Muted blue-gray to slate-blue range
        let r = (35.0 + t * 30.0) as u8; // 35-65
        let g = (55.0 + t * 50.0) as u8; // 55-105
        let b = (50.0 + t * 45.0) as u8; // 50-95
        Color32::from_rgb(r, g, b)
    };

    match detail_level {
        DetailLevel::Solid => {
            // Draw solid regions for each contiguous time range
            for range in timeline.time_ranges() {
                let x_start = ts_to_x(range.start).max(rect.left());
                let x_end = ts_to_x(range.end).min(rect.right());

                // Enforce minimum visual width for sub-pixel data regions
                let visual_width = x_end - x_start;
                let x_end = if visual_width > 0.0 && visual_width < 8.0 {
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
                        Color32::from_rgba_unmultiplied(45, 80, 60, 140),
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

                    let (scan_fill, scan_stroke) = vcp_to_color(scan.vcp, scan.completeness);
                    painter.rect_filled(scan_rect, 2.0, scan_fill);
                    painter.rect_stroke(
                        scan_rect,
                        2.0,
                        Stroke::new(1.0, scan_stroke),
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

                    let (scan_fill, scan_stroke) = vcp_to_color(scan.vcp, scan.completeness);
                    painter.rect_filled(scan_rect, 2.0, scan_fill);
                    painter.rect_stroke(
                        scan_rect,
                        2.0,
                        Stroke::new(1.0, scan_stroke),
                        StrokeKind::Inside,
                    );

                    // Draw individual sweep blocks inside the scan (if loaded)
                    if !scan.sweeps.is_empty() {
                        // Look up VCP elevation info for product annotations
                        let vcp_elevations = scan.vcp_pattern.as_ref().map(|v| &v.elevations);

                        for sweep in scan.sweeps.iter() {
                            let x_start = ts_to_x(sweep.start_time).max(rect.left());
                            let x_end = ts_to_x(sweep.end_time).min(rect.right());

                            if x_end > x_start && (x_end - x_start) > 0.5 {
                                // Determine if this sweep matches the user's target elevation
                                let matches_elevation = (sweep.elevation - target_elevation).abs() < 0.3;

                                // Color: matching sweeps are brighter, non-matching are dimmed
                                let base = elevation_to_color(sweep.elevation);
                                let color = if matches_elevation {
                                    // Brighten matching sweeps
                                    Color32::from_rgb(
                                        (base.r() as u16 + 30).min(255) as u8,
                                        (base.g() as u16 + 40).min(255) as u8,
                                        (base.b() as u16 + 30).min(255) as u8,
                                    )
                                } else {
                                    // Dim non-matching sweeps
                                    Color32::from_rgba_unmultiplied(
                                        base.r(),
                                        base.g(),
                                        base.b(),
                                        120,
                                    )
                                };

                                // Sweeps are narrower (more inset) than the scan block
                                let sweep_rect = Rect::from_min_max(
                                    Pos2::new(x_start, rect.top() + 6.0),
                                    Pos2::new(x_end, rect.bottom() - 6.0),
                                );

                                painter.rect_filled(sweep_rect, 1.0, color);

                                // Subtle border between sweeps
                                if (x_end - x_start) > 3.0 {
                                    painter.rect_stroke(
                                        sweep_rect,
                                        1.0,
                                        Stroke::new(0.5, Color32::from_rgba_unmultiplied(30, 50, 40, 100)),
                                        StrokeKind::Inside,
                                    );
                                }

                                // Highlight the actively rendered sweep
                                let is_active = active_sweep.is_some_and(|(scan_ts, elev_num)| {
                                    scan.start_time as i64 == scan_ts
                                        && sweep.elevation_number == elev_num
                                });
                                if is_active {
                                    painter.rect_stroke(
                                        sweep_rect,
                                        1.0,
                                        Stroke::new(2.0, tl_colors::ACTIVE_SWEEP),
                                        StrokeKind::Outside,
                                    );
                                }

                                // Elevation + product labels on wide sweep blocks
                                let sweep_width = x_end - x_start;
                                if sweep_width > 25.0 {
                                    // Build label with product info from VCP
                                    let mut label = if sweep_width > 60.0 {
                                        format!("E{} {:.1}\u{00B0}", sweep.elevation_number, sweep.elevation)
                                    } else {
                                        format!("{:.1}", sweep.elevation)
                                    };

                                    // Append product codes if VCP info is available and wide enough
                                    if sweep_width > 80.0 {
                                        if let Some(elevs) = vcp_elevations {
                                            // Match VCP elevation entry by number
                                            if let Some(vcp_elev) = elevs.get(sweep.elevation_number.saturating_sub(1) as usize) {
                                                let wf = &vcp_elev.waveform;
                                                let products = match wf.as_str() {
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
                                        Color32::from_rgba_unmultiplied(255, 255, 255, 180),
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

        // Pin playback position at ~quarter of the visible timeline during playback.
        let view_width_secs = state.playback_state.view_width_secs();
        if view_width_secs > 0.0 {
            let target_offset = view_width_secs * 0.25; // pin at 25% from left
            let pos = state.playback_state.playback_position();
            state.playback_state.timeline_view_start = pos - target_offset;
        }

        // Request continuous repaint while playing
        ctx.request_repaint();
    }

    egui::TopBottomPanel::bottom("bottom_panel")
        .exact_height(96.0)
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
    // Store actual pixel width so centering calculations outside this
    // function use the real value instead of the 1000px approximation.
    state.playback_state.timeline_width_px = available_width;
    let vcp_track_height = 6.0;
    let timeline_height = 36.0 + vcp_track_height;

    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_width as f32, timeline_height),
        Sense::click_and_drag(),
    );
    let full_rect = response.rect;

    // Split into main timeline and VCP track
    let rect = Rect::from_min_max(
        full_rect.min,
        Pos2::new(full_rect.max.x, full_rect.max.y - vcp_track_height),
    );
    let vcp_rect = Rect::from_min_max(
        Pos2::new(full_rect.min.x, full_rect.max.y - vcp_track_height),
        full_rect.max,
    );

    let dark = state.is_dark;

    // Background
    painter.rect_filled(rect, 2.0, tl_colors::background(dark));
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, tl_colors::border(dark)),
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

    render_radar_data(
        &painter,
        &rect,
        &state.radar_timeline,
        view_start,
        view_end,
        zoom,
        detail_level,
        active_sweep,
        state.viz_state.target_elevation,
    );

    // Render ghost markers for pending downloads
    if state.download_progress.is_active() {
        let anim_time = ui.ctx().input(|i| i.time);
        render_download_ghosts(
            &painter,
            &rect,
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

    // VCP info track — thin colored bar showing VCP mode transitions
    {
        let vcp_color = |vcp: u16| -> Color32 {
            match vcp {
                215 => Color32::from_rgb(50, 120, 70),   // Precipitation — green
                212 => Color32::from_rgb(55, 110, 75),   // Precipitation fast — green
                31 | 32 | 35 => Color32::from_rgb(50, 85, 140), // Clear air — blue
                12 | 121 => Color32::from_rgb(160, 85, 45),     // Severe — orange
                _ if vcp > 0 => Color32::from_rgb(55, 90, 90),  // Other — teal
                _ => Color32::from_rgb(50, 50, 50),              // Unknown — gray
            }
        };

        // Background for VCP track
        painter.rect_filled(
            vcp_rect,
            0.0,
            Color32::from_rgb(22, 22, 30),
        );

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

        // VCP label at scan zoom when there's space
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
                tl_colors::tick_major(dark)
            } else {
                tl_colors::tick_minor(dark)
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
                let label = format_timestamp(tick, tick_config, use_local);
                painter.text(
                    Pos2::new(x, rect.top() + 10.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::monospace(9.0),
                    tl_colors::tick_label(dark),
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

    // Draw selection marker (playback position indicator)
    {
        let selected_ts = state.playback_state.playback_position();
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

    // Draw "now" marker (current wall-clock time)
    {
        let now_ts = current_timestamp_secs();
        let now_x = ts_to_x(now_ts);

        if now_x >= rect.left() && now_x <= rect.right() {
            let now_color = tl_colors::NOW_MARKER;

            // Dashed-style: short line at top and bottom
            painter.line_segment(
                [
                    Pos2::new(now_x, rect.top()),
                    Pos2::new(now_x, rect.top() + 4.0),
                ],
                Stroke::new(1.5, now_color),
            );
            painter.line_segment(
                [
                    Pos2::new(now_x, rect.bottom() - 4.0),
                    Pos2::new(now_x, rect.bottom()),
                ],
                Stroke::new(1.5, now_color),
            );
            // Thin line through middle
            painter.line_segment(
                [
                    Pos2::new(now_x, rect.top() + 4.0),
                    Pos2::new(now_x, rect.bottom() - 4.0),
                ],
                Stroke::new(0.5, Color32::from_rgba_unmultiplied(
                    now_color.r(), now_color.g(), now_color.b(), 100,
                )),
            );
            // Small diamond at bottom
            let d = 3.0;
            let diamond = vec![
                Pos2::new(now_x, rect.bottom() - d),
                Pos2::new(now_x + d, rect.bottom()),
                Pos2::new(now_x, rect.bottom() + d),
                Pos2::new(now_x - d, rect.bottom()),
            ];
            painter.add(egui::Shape::convex_polygon(
                diamond,
                now_color,
                Stroke::NONE,
            ));
        }
    }

    // Draw selection range labels (boundaries and duration)
    if let Some((range_start, range_end)) = state.playback_state.selection_range() {
        let start_x = ts_to_x(range_start);
        let end_x = ts_to_x(range_end);

        if end_x >= rect.left() && start_x <= rect.right() {
            let label_color = tl_colors::SELECTION_LABEL;
            let duration_secs = range_end - range_start;
            let duration_text = if duration_secs < 60.0 {
                format!("{:.0}s", duration_secs)
            } else if duration_secs < 3600.0 {
                format!("{:.1}m", duration_secs / 60.0)
            } else {
                format!("{:.1}h", duration_secs / 3600.0)
            };

            // Duration label centered in the selection
            let center_x = ((start_x + end_x) / 2.0).clamp(rect.left() + 20.0, rect.right() - 20.0);
            painter.text(
                Pos2::new(center_x, rect.top() + 3.0),
                egui::Align2::CENTER_TOP,
                &duration_text,
                egui::FontId::monospace(8.0),
                label_color,
            );

            // Boundary timestamps at sufficient zoom
            let tick_config = select_tick_config(zoom);
            if (end_x - start_x) > 100.0 {
                let start_label = format_timestamp(range_start as i64, tick_config, use_local);
                let end_label = format_timestamp(range_end as i64, tick_config, use_local);
                if start_x >= rect.left() && start_x <= rect.right() {
                    painter.text(
                        Pos2::new(start_x + 2.0, rect.bottom() - 2.0),
                        egui::Align2::LEFT_BOTTOM,
                        &start_label,
                        egui::FontId::monospace(7.0),
                        label_color,
                    );
                }
                if end_x >= rect.left() && end_x <= rect.right() {
                    painter.text(
                        Pos2::new(end_x - 2.0, rect.bottom() - 2.0),
                        egui::Align2::RIGHT_BOTTOM,
                        &end_label,
                        egui::FontId::monospace(7.0),
                        label_color,
                    );
                }
            }
        }
    }

    // Check if shift is held
    let shift_held = ui.input(|i| i.modifiers.shift);

    // Handle shift+click to create range from current playback position to click point
    if shift_held && response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let clicked_ts = view_start + (pos.x - rect.left()) as f64 / zoom;
            let current_pos = state.playback_state.playback_position();
            state.playback_state.selection_start = Some(current_pos);
            state.playback_state.selection_end = Some(clicked_ts);
            state.playback_state.apply_selection_as_bounds();
            let duration_mins =
                (clicked_ts - current_pos).abs() / 60.0;
            log::info!("Shift+click range: {:.0} minutes", duration_mins);
        }
    }

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
        // Apply selection as playback bounds for loop/ping-pong behavior
        if let Some((start, end)) = state.playback_state.selection_range() {
            let duration_mins = (end - start) / 60.0;
            log::info!("Selected time range: {:.0} minutes", duration_mins);
            state.playback_state.apply_selection_as_bounds();
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

            // Snap to nearest scan/sweep boundary if within 10 pixels
            let snap_dist_secs = 10.0 / zoom;
            let snapped_ts = state
                .radar_timeline
                .snap_to_boundary(clicked_ts, snap_dist_secs)
                .unwrap_or(clicked_ts);

            state.playback_state.set_playback_position(snapped_ts);

            // Clear any selection range on regular click
            state.playback_state.clear_selection();

            // If clicked within loaded data range, also seek to that frame
            if let Some(frame) = state.playback_state.timestamp_to_frame(snapped_ts as i64) {
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
fn format_timestamp_full(ts: f64, use_local: bool) -> String {
    let secs = ts.floor() as i64;
    let millis = ((ts.fract()) * 1000.0).round() as u32;
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

/// Render translucent ghost blocks on the timeline for pending downloads.
///
/// Each pending scan gets a ghost block spanning its actual `[start, end)`
/// boundary (derived from the archive listing). The currently-active download
/// pulses to distinguish it from queued items. Ghosts that overlap with
/// already-loaded scans are skipped.
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

    // Combine pending and in-flight scan boundaries for ghost rendering.
    let all_ghost_scans: Vec<(i64, i64)> = progress
        .pending_scans
        .iter()
        .chain(progress.in_flight_scans.iter())
        .copied()
        .collect();

    if detail_level == DetailLevel::Solid {
        // At solid detail level, render a single translucent region spanning all ghosts
        if all_ghost_scans.is_empty() {
            return;
        }
        let min_ts = all_ghost_scans.iter().map(|(s, _)| *s).min().unwrap() as f64;
        let max_ts = all_ghost_scans.iter().map(|(_, e)| *e).max().unwrap() as f64;

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

    // At Scans/Sweeps detail level, render individual ghost blocks
    for &(scan_start, scan_end) in &all_ghost_scans {
        let start_f64 = scan_start as f64;
        let end_f64 = scan_end as f64;

        // Skip if outside visible range
        if end_f64 < view_start || start_f64 > view_end {
            continue;
        }

        // Skip if a real scan already covers this timestamp
        if timeline
            .scans_in_range(start_f64, end_f64)
            .any(|s| s.start_time <= start_f64 + 30.0 && s.end_time >= start_f64 - 30.0)
        {
            continue;
        }

        let x_start = ts_to_x(start_f64).max(rect.left());
        let x_end = ts_to_x(end_f64).min(rect.right());
        if x_end <= x_start || (x_end - x_start) < 1.0 {
            continue;
        }

        let is_active = progress.active_scan.map(|(s, _)| s) == Some(scan_start);

        // Pulse animation for the active ghost
        let pulse = if is_active {
            (0.5 + 0.5 * (anim_time * 3.0).sin()) as f32
        } else {
            0.0
        };

        let fill_alpha = if is_active {
            (35.0 + 25.0 * pulse) as u8
        } else {
            30u8
        };
        let border_alpha = if is_active {
            (55.0 + 30.0 * pulse) as u8
        } else {
            45u8
        };

        let ghost_rect = Rect::from_min_max(
            Pos2::new(x_start, rect.top() + 3.0),
            Pos2::new(x_end, rect.bottom() - 3.0),
        );

        painter.rect_filled(
            ghost_rect,
            2.0,
            Color32::from_rgba_unmultiplied(100, 150, 255, fill_alpha),
        );
        painter.rect_stroke(
            ghost_rect,
            2.0,
            Stroke::new(
                1.0,
                Color32::from_rgba_unmultiplied(100, 150, 255, border_alpha),
            ),
            StrokeKind::Inside,
        );
    }
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

