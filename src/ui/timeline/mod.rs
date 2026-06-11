//! Timeline rendering: time ruler, scan/sweep tracks, tooltip, and overlays.

mod interaction;
mod now_edge;
mod overlays;
mod ruler;
mod scan_track;
mod strokes;
pub(super) mod style;
mod sweep_track;
mod tooltips;

use super::colors::timeline as tl_colors;
use crate::state::{AppState, LivePhase, WidthTier};
use chrono::{Datelike, TimeZone, Timelike, Utc};
use eframe::egui::{self, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use interaction::handle_timeline_interaction;
use now_edge::render_now_affordance;
use overlays::{render_download_ghosts, render_realtime_progress, render_saved_events};
use ruler::{render_playback_cursor, render_tick_marks};
use scan_track::{render_scan_track, render_shadow_boundaries};
use sweep_track::{render_connector_lines, render_sweep_track};
use tooltips::render_timeline_tooltip;

/// Level of detail for radar data rendering, selected by zoom.
/// The names match the on-screen track headers (VOLUMES / TILTS).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum DetailLevel {
    /// Zoomed far out: just show solid color where data exists
    Coverage,
    /// Show individual volume-scan blocks
    Volumes,
    /// Show tilt (sweep) blocks within volume scans
    Tilts,
}

/// Sub-rects of the timeline widget's stacked tracks.
pub(super) struct TrackRects {
    /// Timestamp lane above the data tracks.
    pub tick: Rect,
    /// Volume-scan track.
    pub scan: Rect,
    /// Tilt (sweep) track — present only at [`DetailLevel::Tilts`].
    pub sweep: Option<Rect>,
    /// Scan + sweep span: cursor, selection, and now-affordance overlays.
    pub overlay: Rect,
}

/// Read-only per-frame context shared by every timeline renderer.
///
/// Bundles the view adapter, the frame clock, and the view geometry so
/// child renderers take `(painter, &TimelineFrame, <specifics>)` instead of
/// re-threading a dozen positional args. Borrows only the `Timeline`
/// subsystem (via [`TimelineView`]), so `&mut` access to state / live /
/// playback stays available alongside it for the interaction paths.
pub(super) struct TimelineFrame<'a> {
    pub view: crate::state::TimelineView<'a>,
    /// This frame's wall clock (`AppState::frame_now`).
    pub now_secs: f64,
    /// Absolute timestamp of the view's left edge (Unix seconds).
    pub view_start: f64,
    /// Absolute timestamp of the view's right edge (Unix seconds).
    pub view_end: f64,
    /// Pixels per second.
    pub zoom: f64,
    pub rects: TrackRects,
    pub detail: DetailLevel,
    pub dark: bool,
    pub use_local: bool,
    /// On-GPU sweep `(scan_key_ts, elevation_number)` for the active border
    /// highlight. `None` below [`DetailLevel::Tilts`].
    pub active_sweep: Option<(f64, u8)>,
    /// Prior on-GPU sweep while sweep animation blends, snapshotted so it
    /// flips atomically with `active_sweep`.
    pub prev_active_sweep: Option<(f64, u8)>,
}

impl TimelineFrame<'_> {
    /// Timestamp → x pixel. The single definition all renderers share.
    pub fn ts_to_x(&self, ts: f64) -> f32 {
        self.rects.scan.left() + ((ts - self.view_start) * self.zoom) as f32
    }

    /// X pixel → timestamp (inverse of [`Self::ts_to_x`]); used by the
    /// interaction handlers to resolve clicks/drags.
    pub fn x_to_ts(&self, x: f32) -> f64 {
        self.view_start + (x - self.rects.scan.left()) as f64 / self.zoom
    }
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

/// Format a timestamp for the playback readout, shortening it as horizontal
/// space tightens so it doesn't crowd the transport controls:
/// - [`WidthTier::Full`]: full date + time + milliseconds (same as
///   [`format_timestamp_full`]).
/// - [`WidthTier::Compact`]: date + time, milliseconds dropped.
/// - [`WidthTier::Cramped`]: time only (`HH:MM:SS`), date dropped.
pub(super) fn format_timestamp_compact(ts: f64, use_local: bool, tier: WidthTier) -> String {
    if tier >= WidthTier::Full {
        return format_timestamp_full(ts, use_local);
    }
    let secs = ts.floor() as i64;
    let dt = DateTimeComponents::from_timestamp(secs, use_local);
    if tier == WidthTier::Compact {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
        )
    } else {
        format!("{:02}:{:02}:{:02}", dt.hour, dt.minute, dt.second)
    }
}

/// Draw a small lane-name header pinned to the bottom-left inside a track,
/// over a background-colored chip so it stays readable above block content.
fn draw_track_header(painter: &egui::Painter, track_rect: &Rect, text: &str, dark: bool) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        style::header_font(),
        tl_colors::track_header(dark),
    );
    let pos = Pos2::new(
        track_rect.left() + 4.0,
        track_rect.bottom() - 2.0 - galley.size().y,
    );
    let chip = Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(3.0, 1.0));
    painter.rect_filled(chip, 2.0, tl_colors::track_header_backdrop(dark));
    painter.galley(pos, galley, tl_colors::track_header(dark));
}

pub(super) fn render_timeline(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    derived: &crate::subsystem::Derived,
) {
    let use_local = state.use_local_time;
    let available_width = ui.available_width() as f64;
    playback.state.timeline_width_px = available_width;

    let zoom = playback.state.timeline_zoom;
    let detail_level = if zoom < 0.2 {
        DetailLevel::Coverage
    } else if zoom < 1.0 {
        DetailLevel::Volumes
    } else {
        DetailLevel::Tilts
    };

    // Track heights — timestamp lane sits above the scan track so labels
    // never overlap scan block content. The total timeline height is
    // constant across macro/micro transitions: at Sweeps detail the
    // scan + separator + sweep tracks share the space; at lower detail
    // the scan track expands to fill the same total so the bottom
    // panel doesn't reflow when zooming. All dimensions live in `style`.
    let tick_lane_h = style::TICK_LANE_H;
    let (scan_track_h, separator_h, sweep_track_h) = if detail_level == DetailLevel::Tilts {
        (
            style::SCAN_TRACK_H,
            style::TRACK_SEPARATOR_H,
            style::SWEEP_TRACK_H,
        )
    } else {
        (style::EXPANDED_SCAN_TRACK_H, 0.0_f32, 0.0_f32)
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
    let sweep_rect = (detail_level == DetailLevel::Tilts).then(|| {
        Rect::from_min_max(
            Pos2::new(full_rect.min.x, scan_rect.max.y + separator_h),
            Pos2::new(
                full_rect.max.x,
                scan_rect.max.y + separator_h + sweep_track_h,
            ),
        )
    });

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
    if let Some(sweep_rect) = sweep_rect {
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

    let view_start = playback.state.timeline_view_start;
    let visible_secs = available_width / zoom;
    let view_end = view_start + visible_secs;

    // Active border tracks the on-GPU sweep — `displayed` is set only after
    // a successful update_data() in handle_decoded_outcome, so the highlight
    // matches the pixels the user is actually looking at (not the resolver's
    // intent, which may have a render in flight).
    let active_sweep = if detail_level == DetailLevel::Tilts {
        state.viz_state.displayed.as_ref().map(|d| {
            (
                d.identity.scan_timestamp_secs(),
                d.identity.elevation_number,
            )
        })
    } else {
        None
    };
    // Previous border tracks the prior on-GPU sweep — snapshotted from
    // `displayed` at the moment a new sweep is uploaded, so it flips
    // atomically with `active_sweep`.
    let prev_active_sweep = if derived.effective_sweep_animation {
        state.viz_state.previous_displayed.as_ref().map(|d| {
            (
                d.identity.scan_timestamp_secs(),
                d.identity.elevation_number,
            )
        })
    } else {
        None
    };

    // The overlay rect spans all data tracks (scan + sweep, not the ticks).
    let overlay_rect = Rect::from_min_max(
        scan_rect.min,
        Pos2::new(
            scan_rect.max.x,
            sweep_rect.map_or(scan_rect.max.y, |r| r.max.y),
        ),
    );

    // -- Build the per-frame TimelineFrame --
    // One adapter (`TimelineView`) merges every timeline source (cache,
    // archive shadows, the live stream + its projection) into a single
    // source-agnostic view; the frame bundles it with the view geometry and
    // the frame clock. Renderers ask the view availability questions
    // ("what's cached? what's collecting? is this covered?") and never read
    // a raw source directly. The cache↔live merge that keeps a resumed
    // volume's already-downloaded sweeps visible lives inside
    // `TimelineView::build`.
    let frame = TimelineFrame {
        view: crate::state::TimelineView::build(
            &timeline.scans,
            &timeline.shadow_scan_boundaries,
            Some(&live.mode_state),
            live.radar_model.position.as_ref(),
            live.frame_projection.as_ref(),
            state.viz_state.elevation_selection.elevation_number(),
            state.frame_now.secs(),
        ),
        now_secs: state.frame_now.secs(),
        view_start,
        view_end,
        zoom,
        rects: TrackRects {
            tick: tick_rect,
            scan: scan_rect,
            sweep: sweep_rect,
            overlay: overlay_rect,
        },
        detail: detail_level,
        dark,
        use_local,
        active_sweep,
        prev_active_sweep,
    };
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);

    // -- Render shadow scan boundaries from archive index --
    if !frame.view.shadow_boundaries().is_empty() {
        render_shadow_boundaries(&painter, &frame);
    }

    // -- Render scan track --
    render_scan_track(&painter, &frame);

    // -- Render sweep track (only at Tilts detail) --
    if detail_level == DetailLevel::Tilts {
        render_sweep_track(&painter, &frame);
        render_connector_lines(&painter, &frame);
    }

    // -- Render ghost markers for pending downloads --
    if state.download_progress.is_active() {
        let anim_time = ui.ctx().input(|i| i.time);
        render_download_ghosts(&painter, &frame, &state.download_progress, anim_time);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(67));
    }

    // -- Render real-time partial scan progress --
    // The merged in-progress volume and its overlay context come from the
    // view; `frame.now_secs` keeps render + tooltip on a consistent boundary.
    if let (Some(position), Some(overlay_ctx)) =
        (frame.view.live_volume(), frame.view.live_context())
    {
        let anim_time = ui.ctx().input(|i| i.time);
        render_realtime_progress(&painter, &frame, position, overlay_ctx, anim_time);
        if live.mode_state.phase == LivePhase::WaitingForChunk {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    // -- Track headers --
    // Tiny lane-name labels pinned bottom-left inside each track, the only
    // legend-like element: they name structure, not color. Bottom-left
    // stays clear of the LIVE edge chip and tick labels (both at the top).
    if detail_level != DetailLevel::Coverage {
        draw_track_header(&painter, &scan_rect, "VOLUMES", dark);
    }
    if let Some(sweep_rect) = sweep_rect {
        draw_track_header(&painter, &sweep_rect, "TILTS", dark);
    }

    // Draw tick marks and labels in the dedicated tick lane above the scan
    // track. Tick spacing/alignment is derived inside from the frame's zoom
    // and view bounds.
    render_tick_marks(&painter, &frame);

    // Draw saved event overlays (behind the selection range)
    render_saved_events(
        &painter,
        &frame,
        &state.saved_events,
        &state.viz_state.site_id,
    );

    // Draw selection range (if user has selected a range via shift+drag)
    if let Some((range_start, range_end)) = playback.state.selection_range() {
        let start_x = ts_to_x(range_start);
        let end_x = ts_to_x(range_end);

        if end_x >= overlay_rect.left() && start_x <= overlay_rect.right() {
            let visible_start = start_x.max(overlay_rect.left());
            let visible_end = end_x.min(overlay_rect.right());

            let range_rect = Rect::from_min_max(
                Pos2::new(visible_start, overlay_rect.top()),
                Pos2::new(visible_end, overlay_rect.bottom()),
            );
            painter.rect_filled(range_rect, 0.0, tl_colors::selection_fill());

            if start_x >= overlay_rect.left() && start_x <= overlay_rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(start_x, overlay_rect.top()),
                        Pos2::new(start_x, overlay_rect.bottom()),
                    ],
                    Stroke::new(1.5, tl_colors::selection_edge()),
                );
            }
            if end_x >= overlay_rect.left() && end_x <= overlay_rect.right() {
                painter.line_segment(
                    [
                        Pos2::new(end_x, overlay_rect.top()),
                        Pos2::new(end_x, overlay_rect.bottom()),
                    ],
                    Stroke::new(1.5, tl_colors::selection_edge()),
                );
            }
        }
    }

    // Draw the playback-position cursor (the neutral "needle").
    render_playback_cursor(&painter, &frame, playback.state.playback_position());

    // The now affordance: the live edge of the timeline — both the streaming
    // indicator and the control to start/stop it. When now is on-screen it's
    // an inline line + clickable cap; when scrolled off it's an edge chip.
    // Returns the rect it occupies so the seek handler ignores clicks on it.
    let now_affordance_rect = render_now_affordance(ui, &painter, state, live, playback, &frame);

    // Draw selection range labels (boundaries and duration)
    if let Some((range_start, range_end)) = playback.state.selection_range() {
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
                style::block_font(),
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
                        style::block_font(),
                        label_color,
                    );
                }
                if end_x >= scan_rect.left() && end_x <= scan_rect.right() {
                    painter.text(
                        Pos2::new(end_x - 2.0, scan_rect.bottom() - 2.0),
                        egui::Align2::RIGHT_BOTTOM,
                        &end_label,
                        style::block_font(),
                        label_color,
                    );
                }
            }
        }
    }

    // -- Hover tooltips --
    if response.hovered() {
        if let Some(hover_pos) = response.hover_pos() {
            let hover_ts = frame.x_to_ts(hover_pos.x);
            render_timeline_tooltip(ui, &frame, live, hover_ts, hover_pos);
        }
    }

    // -- Interaction handling --
    handle_timeline_interaction(
        ui,
        state,
        live,
        playback,
        &response,
        &frame,
        now_affordance_rect,
    );
}
