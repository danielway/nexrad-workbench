//! Timeline rendering: time ruler, the frames-first main track (scan
//! containers + frame cells in Micro; uniform ticks + coverage in Macro),
//! tooltip, and overlays.

mod frame_cells;
mod interaction;
mod marks;
pub(in crate::ui) mod mini;
mod now_edge;
mod overlays;
mod overview;
mod ruler;
mod scan_track;
mod selection_strip;
mod strokes;
pub(super) mod style;
mod tooltips;

use crate::core::TimelineTier;
use crate::core::{FrameJoinInputs, LivePhase};
use crate::state::AppState;
use crate::ui::theme::timeline as tl_colors;
use chrono::{Datelike, TimeZone, Timelike, Utc};
use eframe::egui::{self, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use frame_cells::{paint_container, CellAnim};
use interaction::handle_timeline_interaction;
use marks::paint_strip_marks;
use now_edge::render_now_affordance;
use overlays::render_saved_events;
use ruler::{render_playback_cursor, render_tick_marks};
use scan_track::render_macro_track;
use tooltips::render_timeline_tooltip;

/// Sub-rects of the timeline widget. One main track now (frames-first), with
/// a tick rail above. The `overlay` rect spans the main track for the cursor,
/// selection, and now-affordance.
pub(super) struct TrackRects {
    /// Timestamp rail above the main track.
    pub tick: Rect,
    /// The single main track: scan containers + frame cells (Micro), or
    /// uniform ticks / coverage fill (Macro).
    pub scan: Rect,
    /// Main-track span for cursor, selection, and now-affordance overlays.
    pub overlay: Rect,
}

/// Treat a context as reduced-motion when egui's animation time is zeroed —
/// the cheapest proxy for the OS/browser "reduce motion" preference (spec §16;
/// egui doesn't expose it directly). When reduced, pulses render as a static
/// partial fill and the tier morph switches instantly.
pub(super) fn reduced_motion(ctx: &egui::Context) -> bool {
    ctx.global_style().animation_time == 0.0
}

/// Read-only per-frame context shared by every timeline renderer.
///
/// Bundles the view adapter, the frame clock, and the view geometry so
/// child renderers take `(painter, &TimelineFrame, <specifics>)` instead of
/// re-threading a dozen positional args. Borrows only the `Timeline`
/// subsystem (via [`TimelineView`]), so `&mut` access to state / live /
/// playback stays available alongside it for the interaction paths.
pub(super) struct TimelineFrame<'a> {
    pub view: crate::core::TimelineView<'a>,
    /// This frame's wall clock (`AppState::frame_now`).
    pub now_secs: f64,
    /// Absolute timestamp of the view's left edge (Unix seconds).
    pub view_start: f64,
    /// Absolute timestamp of the view's right edge (Unix seconds).
    pub view_end: f64,
    /// Pixels per second.
    pub zoom: f64,
    pub rects: TrackRects,
    /// The stored timeline tier (single source of truth, with hysteresis) this
    /// frame renders at. Renderers branch on it directly.
    pub tier: TimelineTier,
    pub use_local: bool,
}

impl TimelineFrame<'_> {
    /// Timestamp → x pixel. The single definition all renderers share.
    pub(crate) fn ts_to_x(&self, ts: f64) -> f32 {
        self.rects.scan.left() + ((ts - self.view_start) * self.zoom) as f32
    }

    /// X pixel → timestamp (inverse of [`Self::ts_to_x`]); used by the
    /// interaction handlers to resolve clicks/drags.
    pub(crate) fn x_to_ts(&self, x: f32) -> f64 {
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

// The tick ladder stops at days: the strip shows at most three days, so it
// never needs week/month/year ticks. The overview labels its wider spans
// itself.
const TICK_CONFIGS: &[TickConfig] = &[
    // Days (coarsest the linear strip needs; also the `select_tick_config`
    // fallback when nothing finer fits).
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

/// The chosen zone's offset east of UTC at `ts` (seconds; 0 in UTC). A
/// browser fact, so it is read here and handed to the core as a number.
pub(in crate::ui) fn tz_offset_secs(ts: f64, use_local: bool) -> i64 {
    if !use_local {
        return 0;
    }
    // getTimezoneOffset() is minutes positive-west.
    let d = js_sys::Date::new_0();
    d.set_time(ts * 1000.0);
    -(d.get_timezone_offset() as i64) * 60
}

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

#[allow(clippy::too_many_arguments)]
pub(super) fn render_timeline(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    acquisition: &crate::subsystem::Acquisition,
    derived: &crate::subsystem::Derived,
    chrome: &mut crate::subsystem::Chrome,
) {
    let use_local = state.use_local_time;
    let available_width = ui.available_width() as f64;
    playback.state.timeline_width_px = available_width;

    // The overview and the main strip are two stacked widgets; butt them
    // together (the overview's own padding gives the visual gap) so the panel
    // height stays exactly TIMELINE_TOTAL_H with no surprise inter-widget gap.
    ui.spacing_mut().item_spacing.y = 0.0;

    let zoom = playback.state.timeline_zoom;
    let tier = playback.state.timeline_tier;
    // The strip is ONE main track at constant height across every tier — no
    // panel reflow — and renderers branch on `tier` directly.

    let tick_lane_h = style::TICK_LANE_H;
    let main_track_h = style::MAIN_TRACK_H;

    // -- Overview ---------------------------------------------------------
    // 48 h of context with the view box, ABOVE the main strip in its OWN
    // allocated widget (distinct response id) so its drag/click never shares a
    // hit-test layer with the main strip's press-seek/scrub. Desktop only —
    // the mini timeline plays this role on phones.
    overview::render_overview(ui, state, timeline, live, playback);

    // The main strip widget covers the tick rail, the selection strip and the
    // main track; the overview above is its own allocated widget. The
    // selection strip registers its own interact rect over its band after the
    // strip's interaction runs, so it owns presses there.
    let selection_strip_h = style::SELECTION_STRIP_H;
    let (response, painter) = ui.allocate_painter(
        Vec2::new(
            available_width as f32,
            tick_lane_h + selection_strip_h + main_track_h,
        ),
        Sense::click_and_drag(),
    );
    let full_rect = response.rect;

    // Sub-rects: tick rail -> selection strip -> main track.
    let tick_rect = Rect::from_min_max(
        Pos2::new(full_rect.min.x, full_rect.min.y),
        Pos2::new(full_rect.max.x, full_rect.min.y + tick_lane_h),
    );
    let selection_rect = Rect::from_min_max(
        Pos2::new(full_rect.min.x, tick_rect.max.y),
        Pos2::new(full_rect.max.x, tick_rect.max.y + selection_strip_h),
    );
    let scan_rect = Rect::from_min_max(
        Pos2::new(full_rect.min.x, selection_rect.max.y),
        Pos2::new(full_rect.max.x, selection_rect.max.y + main_track_h),
    );

    // Background for the main track.
    painter.rect_filled(scan_rect, 2.0, tl_colors::BACKGROUND);
    painter.rect_stroke(
        scan_rect,
        2.0,
        Stroke::new(1.0_f32, tl_colors::BORDER),
        StrokeKind::Outside,
    );

    if zoom <= 0.0 {
        return;
    }

    let view_start = playback.state.timeline_view_start;
    let visible_secs = available_width / zoom;
    let view_end = view_start + visible_secs;

    // Active ring tracks the on-GPU sweep — `displayed` is set only after a
    // successful update_data() in handle_decoded_outcome, so the highlight
    // matches the pixels the user is actually looking at (not the resolver's
    // intent, which may have a render in flight). Now applies in the Micro
    // tier (frames-first).
    let active_sweep = if tier == TimelineTier::Micro {
        state.viz_state.displayed.as_ref().map(|d| {
            (
                d.identity.scan_timestamp_secs(),
                d.identity.elevation_number,
            )
        })
    } else {
        None
    };
    let prev_active_sweep = if derived.effective_sweep_animation && tier == TimelineTier::Micro {
        state.viz_state.previous_displayed.as_ref().map(|d| {
            (
                d.identity.scan_timestamp_secs(),
                d.identity.elevation_number,
            )
        })
    } else {
        None
    };

    // The overlay rect spans the main track (not the tick rail).
    let overlay_rect = scan_rect;

    // -- Build the per-frame TimelineFrame --
    // One adapter (`TimelineView`) merges every timeline source (cache,
    // archive shadows, the live stream + its projection) into a single
    // source-agnostic view; the frame bundles it with the view geometry and
    // the frame clock. Renderers ask the view availability questions and never
    // read a raw source directly. The cache↔live merge that keeps a resumed
    // volume's already-downloaded sweeps visible lives inside
    // `TimelineView::build`.
    let frame = TimelineFrame {
        view: crate::core::TimelineView::build(
            &timeline.scans,
            &timeline.shadow_scan_boundaries,
            Some(&live.mode_state),
            live.radar_model.position.as_ref(),
        ),
        now_secs: state.frame_now.secs(),
        view_start,
        view_end,
        zoom,
        rects: TrackRects {
            tick: tick_rect,
            scan: scan_rect,
            overlay: overlay_rect,
        },
        tier,
        use_local,
    };
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);

    // -- Morph animation (spec §6.4) -------------------------------------
    // A 0..1 factor that eases between Macro (0) and Micro (1) on tier
    // transitions; the playhead + live edge stay position-stable (only cell
    // geometry collapses / expands). Reduced motion → instant switch.
    let reduced = reduced_motion(ui.ctx());
    let micro_target = if tier == TimelineTier::Micro {
        1.0
    } else {
        0.0
    };
    let micro_t = if reduced {
        micro_target
    } else {
        ui.ctx()
            .animate_value_with_time(response.id.with("tier_morph"), micro_target, 0.2)
    };
    let morphing = micro_t > 0.001 && micro_t < 0.999;

    // -- Render the main track per tier ----------------------------------
    // Micro renders frames-first (scan containers + frame cells); Macro
    // renders the uniform-tick / coverage strip. During a morph both
    // cross-fade. The download-progress ghosts feed the join (queued / in
    // flight) so queued cells render; failed scan-starts come from the
    // acquisition operations (so a retry clears the mark — the error ring
    // never forgets). The same containers give the strip marks in both
    // tiers, so queued, failed, projected and filling states never vanish
    // when zoomed out.
    let failed_secs = acquisition.state.failed_scan_starts();
    // Active in-flight downloads + still-ingesting both count as in-flight.
    let mut in_flight_all = state.download_progress.in_flight_scans.clone();
    in_flight_all.extend_from_slice(&state.download_progress.active_scans);
    let product = state.viz_state.product.to_worker_string();
    let join = FrameJoinInputs {
        queued: &state.download_progress.pending_scans,
        in_flight: &in_flight_all,
        failed: &failed_secs,
        product,
        tilt: (&state.viz_state.elevation_selection).into(),
        active: active_sweep,
        prev_active: prev_active_sweep,
    };
    let containers = frame
        .view
        .frame_containers_in_range(view_start, view_end, join);
    let strip_marks = crate::core::timeline_view::strip_marks(&containers, zoom, tier);

    // Micro path (frames-first). Drawn when in Micro or morphing into/out of
    // it; cell heights + alpha scale by `micro_t` so they collapse/expand.
    if tier == TimelineTier::Micro || morphing {
        // Free-running pulse for archive in-flight cells — independent of the
        // live-stream pulse (which is frozen at 0 when not streaming), so a
        // pure archive download still animates. Same 0..1 sine the old
        // download ghosts used.
        let anim_time = ui.ctx().input(|i| i.time);
        let pulse = (0.5 + 0.5 * (anim_time * 3.0).sin()) as f32;
        let anim = CellAnim {
            pulse,
            reduced_motion: reduced,
            morph: micro_t,
        };

        // Which container carries the "next data" countdown (the core's
        // call: the live container's next cut, else the soonest ghost).
        let countdown = live.countdown_remaining_secs(frame.now_secs);
        let countdown_idx =
            crate::core::timeline_view::countdown_container(&containers, frame.now_secs);

        // The morph cross-fade is the cell-height collapse applied inside
        // `paint_container` (cells fold into ticks), keyed on `micro_t` via
        // `anim.morph`. The Macro layer fades in opposite via its `fade`.
        for (i, container) in containers.iter().enumerate() {
            let carries = countdown_idx == Some(i);
            paint_container(&painter, &frame, container, anim, countdown, carries);
        }

        // Keep the strip animating while live chunks fill or while morphing.
        if frame.view.live_volume().is_some() && live.mode_state.phase == LivePhase::WaitingForChunk
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        if !reduced && (state.download_progress.is_active() || morphing) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(67));
        }
    }

    if tier == TimelineTier::Macro || morphing {
        // Macro track (uniform ticks + coverage + gap glyphs). Drawn in Macro,
        // or while morphing into/out of Micro (cross-fade with the Micro layer).
        render_macro_track(&painter, &frame, &state.download_progress, 1.0 - micro_t);
        if !reduced && morphing {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(67));
        }
    }

    // Draw tick marks and labels in the dedicated tick lane above the main
    // track. Tick spacing/alignment is derived inside from the frame's zoom
    // and view bounds.
    render_tick_marks(&painter, &frame);

    // Draw saved event overlays (behind the selection range).
    render_saved_events(
        &painter,
        &frame,
        &state.saved_events,
        &state.viz_state.site_id,
    );

    // The range as the core places it: a pinned range runs to the now line.
    let range_marks = playback.state.selection_range().and_then(|(s, e)| {
        let site = crate::data::SiteId::new(state.viz_state.site_id.clone());
        let sizes = timeline.activity.scans(&site, s.min(e), s.max(e));
        let cost = frame.view.range_cost_sized(s, e, &sizes);
        crate::core::timeline_view::range_marks(
            Some((s, e)),
            playback.state.is_live_range(),
            playback.state.selection_in_progress(),
            frame.now_secs,
            cost,
        )
    });

    // Draw the selected range over the frames.
    if let Some(rm) = &range_marks {
        let start_x = ts_to_x(rm.start);
        let end_x = ts_to_x(rm.end);

        if end_x >= overlay_rect.left() && start_x <= overlay_rect.right() {
            let visible_start = start_x.max(overlay_rect.left());
            let visible_end = end_x.min(overlay_rect.right());

            let range_rect = Rect::from_min_max(
                Pos2::new(visible_start, overlay_rect.top()),
                Pos2::new(visible_end, overlay_rect.bottom()),
            );
            painter.rect_filled(range_rect, 0.0, tl_colors::selection_fill());

            for x in [start_x, end_x] {
                if x >= overlay_rect.left() && x <= overlay_rect.right() {
                    painter.line_segment(
                        [
                            Pos2::new(x, overlay_rect.top()),
                            Pos2::new(x, overlay_rect.bottom()),
                        ],
                        Stroke::new(1.5_f32, tl_colors::selection_edge()),
                    );
                }
            }
        }
    }

    // Queued, failed, projected and filling marks, over cells and blocks.
    let failed_ticks = paint_strip_marks(&painter, &frame, &strip_marks, response.hover_pos());

    // Draw the playback-position cursor (the white "needle"), or its edge
    // arrow when it is out of view. While it rides now the now marker
    // stands for it.
    let playhead_at_now = live.mode_state.is_active() && playback.state.is_pinned();
    let edges = crate::core::timeline_view::edge_markers(
        view_start,
        view_end,
        (!playhead_at_now).then(|| playback.state.playback_position()),
        frame.now_secs,
    );
    render_playback_cursor(
        &painter,
        &frame,
        playback.state.playback_position(),
        edges.playhead,
    );

    // The now affordance: the live edge of the timeline — both the streaming
    // indicator and the control to start/stop it. When now is on-screen it's
    // an inline line + clickable cap; when scrolled off it's an edge chip.
    // Returns the rect it occupies so the seek handler ignores clicks on it.
    let now_affordance_rect = render_now_affordance(ui, &painter, state, live, &frame, edges.now);

    // Range labels: the cost at the top (`6:00 · 2 scans · 30 MB`, shown
    // while drawing and after), the boundary times at the bottom corners.
    if let Some(rm) = &range_marks {
        let start_x = ts_to_x(rm.start);
        let end_x = ts_to_x(rm.end);

        if end_x >= scan_rect.left() && start_x <= scan_rect.right() {
            let label_color = tl_colors::SELECTION_LABEL;
            let center_x =
                ((start_x + end_x) / 2.0).clamp(scan_rect.left() + 40.0, scan_rect.right() - 40.0);
            let galley = painter.layout_no_wrap(rm.label.clone(), style::block_font(), label_color);
            let chip = Rect::from_center_size(
                Pos2::new(center_x, scan_rect.top() + 3.0 + galley.size().y / 2.0),
                galley.size() + Vec2::new(6.0, 0.0),
            );
            painter.rect_filled(chip, 2.0, tl_colors::block_label_backdrop());
            painter.galley(chip.min + Vec2::new(3.0, 0.0), galley, label_color);

            let tick_config_sel = select_tick_config(zoom);
            if (end_x - start_x) > 100.0 {
                let start_label = format_timestamp(rm.start as i64, tick_config_sel, use_local);
                let end_label = format_timestamp(rm.end as i64, tick_config_sel, use_local);
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

    // -- Failure retry marks --
    // A click on a ⚠ pushes the existing `Intent::RetryFailed` for each
    // failed operation the mark stands for (one when zoomed in; every merged
    // scan when zoomed out). The mark rects are added to `suppress_rects`
    // unconditionally below so the generic press-seek never also fires on one.
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(tick) = failed_ticks.iter().find(|t| t.rect.contains(pos)) {
                for key in &tick.keys {
                    if let Some(op_id) = acquisition.state.failed_operation_for_scan_start(
                        key.round() as i64,
                        crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
                    ) {
                        state.push_command(crate::core::Intent::RetryFailed(op_id));
                    }
                }
            }
        }
    }
    // Hover hint on a failure mark.
    let over_failure = response
        .hover_pos()
        .and_then(|h| failed_ticks.iter().find(|t| t.rect.contains(h)));
    if over_failure.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // -- Interaction handling --
    // Runs BEFORE the tooltip so a primary-drag scrub can suppress the hover
    // popup. Rects whose presses are owned by another control and must not also
    // seek: the now-affordance cap/chip, the selection strip, and the
    // failed-cell retry ticks.
    let mut suppress_rects: Vec<Rect> = now_affordance_rect.into_iter().collect();
    suppress_rects.push(selection_rect);
    // Failed ticks suppress the generic seek UNCONDITIONALLY: the retry fires on
    // `clicked()` (release) but the strip's press-seek fires earlier on the
    // primary press, so suppressing only after a release-time click would let a
    // press on a tick seek before the retry ever runs.
    suppress_rects.extend(failed_ticks.iter().map(|t| t.rect));
    let interaction = handle_timeline_interaction(
        ui,
        state,
        live,
        playback,
        chrome,
        &response,
        &frame,
        &suppress_rects,
    );

    // -- Hover tooltips --
    // Suppressed during an active scrub so the popup doesn't chase the pointer.
    if response.hovered() && !interaction.scrubbing {
        if let Some(hover_pos) = response.hover_pos() {
            let hover_ts = frame.x_to_ts(hover_pos.x);
            render_timeline_tooltip(ui, &frame, live, hover_ts, hover_pos);
        }
    }

    // -- Selection strip --------------------------------------------------
    // After the strip's interaction, so its interact rect is on top over its
    // band (the strip also suppresses seeks there).
    selection_strip::render_selection_strip(ui, state, live, playback, &frame, selection_rect);
}
