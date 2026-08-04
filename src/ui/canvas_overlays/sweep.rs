//! Radar sweep overlay: range rings, radial lines, sweep animation, and donut chart.
//!
//! Draws the radar coverage grid (range rings, 30-degree radials, cardinal labels)
//! centered on the active site. During sweep animation, renders a rotating sweep
//! line and a donut chart showing current vs. previous sweep angular coverage.
//! In live mode, the donut chart shows per-chunk coverage with distinct colors.

use super::super::canvas::{format_age_compact, format_time_short};
use super::super::colors::{canvas as canvas_colors, radar};
use crate::geo::MapProjection;
use crate::nexrad::RADAR_COVERAGE_RANGE_KM;
use crate::state::AppState;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, Vec2};
use geo_types::Coord;
use std::cell::RefCell;
use std::f32::consts::PI;

/// Cached range-ring lon-degree extent for the active site. The km→deg
/// conversion is invariant under pan/zoom — only `(site_lat, site_lon,
/// range_km)` matter. Recomputing it each frame is cheap, but caching means
/// no trig per-frame and no allocation. Keyed on bit-pattern equality so
/// continuous playback with constant lat/lon hits the cache exactly.
#[derive(Default)]
struct RangeRingCache {
    /// `(lat_bits, lon_bits, range_km_bits)` of the most recent compute.
    key: Option<(u64, u64, u64)>,
    /// Cached lon-range in degrees corresponding to `range_km`.
    lon_range_deg: f64,
}

thread_local! {
    static RANGE_RING_CACHE: RefCell<RangeRingCache> = RefCell::new(RangeRingCache::default());
}

fn cached_lon_range_deg(lat: f64, lon: f64, range_km: f64) -> f64 {
    let key = (lat.to_bits(), lon.to_bits(), range_km.to_bits());
    RANGE_RING_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if cache.key == Some(key) {
            return cache.lon_range_deg;
        }
        let km_to_deg = 1.0 / 111.0;
        let lat_correction = lat.to_radians().cos();
        let v = range_km * km_to_deg / lat_correction;
        cache.key = Some(key);
        cache.lon_range_deg = v;
        v
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_radar_sweep(
    painter: &Painter,
    projection: &MapProjection,
    state: &AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
    chrome: &crate::subsystem::Chrome,
    derived: &crate::subsystem::Derived,
    sweep_info: Option<(f32, f32)>,
    stale: bool,
) {
    let radar_lat = state.viz_state.center_lat;
    let radar_lon = state.viz_state.center_lon;
    let dark = state.is_dark;

    // Compute center from geographic projection (same as GPU renderer)
    let center = projection.geo_to_screen(Coord {
        x: radar_lon,
        y: radar_lat,
    });

    // Compute radius in screen pixels for the coverage range. The
    // km→deg conversion is invariant under pan/zoom — cached behind a
    // thread-local so changing the projection alone doesn't redo the
    // trig.
    let lon_range = cached_lon_range_deg(radar_lat, radar_lon, RADAR_COVERAGE_RANGE_KM);

    let edge = projection.geo_to_screen(Coord {
        x: radar_lon + lon_range,
        y: radar_lat,
    });
    let radius = (edge.x - center.x).abs();

    // Draw range rings
    let ring_color = canvas_colors::ring(dark);
    let ring_major_color = canvas_colors::ring_major(dark);
    let num_rings = 6;
    for i in 1..=num_rings {
        let ring_radius = radius * (i as f32 / num_rings as f32);
        let is_major = i % 2 == 0;
        let color = if is_major {
            ring_major_color
        } else {
            ring_color
        };
        let width = if is_major { 1.5_f32 } else { 1.0_f32 };
        painter.circle_stroke(center, ring_radius, Stroke::new(width, color));
    }

    // Draw radial lines (every 30 degrees)
    let radial_color = canvas_colors::radial(dark);
    for i in 0..12 {
        let angle = (i as f32) * 30.0 * PI / 180.0 - PI / 2.0;
        let end_x = center.x + radius * angle.cos();
        let end_y = center.y + radius * angle.sin();
        painter.line_segment(
            [center, Pos2::new(end_x, end_y)],
            Stroke::new(0.5_f32, radial_color),
        );
    }

    // Draw cardinal direction labels. Lay out fresh each frame (egui's
    // internal galley cache memoizes the identical layout job across frames).
    let label_offset = radius + 15.0;
    let cardinal_color = canvas_colors::cardinal_label(dark);
    let font = egui::FontId::proportional(12.0);
    let cardinal_specs: [(&str, Vec2, egui::Align2); 4] = [
        (
            "N",
            Vec2::new(0.0, -label_offset),
            egui::Align2::CENTER_BOTTOM,
        ),
        ("E", Vec2::new(label_offset, 0.0), egui::Align2::LEFT_CENTER),
        ("S", Vec2::new(0.0, label_offset), egui::Align2::CENTER_TOP),
        (
            "W",
            Vec2::new(-label_offset, 0.0),
            egui::Align2::RIGHT_CENTER,
        ),
    ];
    for (label, offset, align) in cardinal_specs.iter() {
        let galley = painter.layout_no_wrap(label.to_string(), font.clone(), cardinal_color);
        let anchor = center + *offset;
        let size = galley.size();
        let pos = align_pos(anchor, size, *align);
        painter.galley(pos, galley, cardinal_color);
    }

    // Draw center marker (radar site)
    painter.circle_filled(center, 4.0, canvas_colors::center_marker(dark));
    painter.circle_stroke(
        center,
        4.0,
        Stroke::new(1.0_f32, canvas_colors::center_marker_stroke(dark)),
    );

    // Sweep overlay drawing. The donut (data-age coverage indicator)
    // shows in live mode regardless of the toggle so the user always
    // has spatial context for what's stale; the rotating lines (start,
    // data edge, NOW, per-chunk boundaries) only draw when the user
    // has sweep_animation enabled.
    if let Some((az, start_az)) = sweep_info {
        let is_live = live.radar_model.active;
        let show_lines = derived.effective_sweep_animation;

        let (start_line_color, data_edge_color, data_edge_width) = if stale {
            (
                radar::sweep_start_line_stale(),
                radar::sweep_line_stale(),
                2.0_f32,
            )
        } else {
            (radar::sweep_start_line(), radar::SWEEP_LINE, 3.0_f32)
        };

        if show_lines {
            // Line at data start boundary
            let start_angle_rad = (start_az - 90.0) * PI / 180.0;
            let start_end = Pos2::new(
                center.x + radius * start_angle_rad.cos(),
                center.y + radius * start_angle_rad.sin(),
            );
            painter.line_segment([center, start_end], Stroke::new(1.5_f32, start_line_color));

            // Line at data trailing edge
            let data_angle_rad = (az - 90.0) * PI / 180.0;
            painter.line_segment(
                [
                    center,
                    Pos2::new(
                        center.x + radius * data_angle_rad.cos(),
                        center.y + radius * data_angle_rad.sin(),
                    ),
                ],
                Stroke::new(
                    if is_live { 2.0_f32 } else { data_edge_width },
                    data_edge_color,
                ),
            );
        }

        // In live mode, draw a separate "NOW" line at the estimated antenna position
        if is_live && show_lines {
            if let Some(now_az) = live.radar_model.estimated_azimuth {
                let now_rad = (now_az - 90.0) * PI / 180.0;
                let now_color = Color32::from_rgb(255, 80, 80);
                painter.line_segment(
                    [
                        center,
                        Pos2::new(
                            center.x + radius * now_rad.cos(),
                            center.y + radius * now_rad.sin(),
                        ),
                    ],
                    Stroke::new(2.0_f32, now_color),
                );

                // "NOW" label — same metadata style as slice labels.
                // Look up the elevation at the current time from the position
                // model's projected sweep timing. This correctly identifies the
                // elevation the antenna is on even when the worker is still
                // downloading a previous elevation's data.
                let now_label_radius = radius + 4.0 + 6.0 + 14.0; // donut_outer + offset
                let now_secs = state.frame_now.secs();
                let collecting_label = live
                    .radar_model
                    .position
                    .as_ref()
                    .and_then(|p| {
                        p.elevation_index_at(now_secs).and_then(|idx| {
                            p.sweeps.get(idx).map(|s| {
                                let angle = live
                                    .radar_model
                                    .volume
                                    .as_ref()
                                    .and_then(|v| v.vcp_pattern.as_ref())
                                    .and_then(|vcp| {
                                        vcp.elevations
                                            .get(s.elevation_number.saturating_sub(1) as usize)
                                            .map(|el| format!("{:.1}\u{00B0}", el.angle))
                                    })
                                    .unwrap_or_default();
                                format!("NOW \u{00B7} Elev {} {}", s.elevation_number, angle)
                            })
                        })
                    })
                    .or_else(|| {
                        // Fallback: the engine's in-progress elevation.
                        live.radar_model
                            .active_sweep
                            .as_ref()
                            .map(|s| format!("NOW \u{00B7} Elev {}", s.elevation_number))
                    })
                    .unwrap_or_else(|| "NOW".to_string());

                draw_boundary_label(
                    painter,
                    center,
                    now_label_radius,
                    now_az,
                    &collecting_label,
                    None,
                    now_color,
                    now_color,
                    &egui::FontId::monospace(10.0),
                );
            }
        }

        // Per-chunk boundary lines across the radar render during live
        // streaming. Treated as part of "the sweeping line" — gated on
        // the toggle so they vanish when sweep animation is off.
        if show_lines {
            if let Some(sweep) = live.radar_model.active_sweep.as_ref() {
                let boundary_line_color = Color32::from_rgba_unmultiplied(200, 200, 220, 100);
                for c in sweep
                    .chunks
                    .iter()
                    .take(sweep.chunks.len().saturating_sub(1))
                {
                    let a = (c.last_az - 90.0) * PI / 180.0;
                    let p_end = Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin());
                    painter
                        .line_segment([center, p_end], Stroke::new(1.0_f32, boundary_line_color));
                }
            }
        }

        // Donut chart showing current vs previous sweep coverage,
        // shaded by data age. In live mode we keep this visible even
        // when sweep_animation is off so the user has spatial context
        // for which sectors are recent. Archive mode respects the
        // toggle (where it's purely an animation artifact).
        if is_live || derived.effective_sweep_animation {
            if stale {
                draw_sweep_donut_stale(painter, center, radius);
            } else {
                draw_sweep_donut(
                    painter, center, radius, az, start_az, state, timeline, live, playback, chrome,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_boundary_label(
    painter: &Painter,
    center: Pos2,
    label_radius: f32,
    azimuth_deg: f32,
    left_text: &str,
    right_text: Option<&str>,
    left_color: Color32,
    right_color: Color32,
    font: &egui::FontId,
) {
    let label_angle = (azimuth_deg - 90.0) * PI / 180.0;
    let label_pos = Pos2::new(
        center.x + label_radius * label_angle.cos(),
        center.y + label_radius * label_angle.sin(),
    );
    let align = sweep_label_align(azimuth_deg);

    // Build a LayoutJob with colored segments
    let mut job = egui::text::LayoutJob::default();
    job.append(
        left_text,
        0.0,
        egui::TextFormat {
            font_id: font.clone(),
            color: left_color,
            ..Default::default()
        },
    );
    if let Some(right) = right_text {
        job.append(
            " | ",
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: Color32::from_rgb(120, 120, 140),
                ..Default::default()
            },
        );
        job.append(
            right,
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: right_color,
                ..Default::default()
            },
        );
    }

    let galley = painter.layout_job(job);
    let text_size = galley.size();
    let bg_pos = align_pos(label_pos, text_size, align);
    let padding = Vec2::new(3.0, 2.0);
    painter.rect_filled(
        Rect::from_min_size(bg_pos - padding, text_size + padding * 2.0),
        3.0,
        Color32::from_rgba_unmultiplied(15, 15, 25, 200),
    );
    painter.galley(bg_pos, galley, Color32::WHITE);
}

fn draw_sweep_donut_stale(painter: &Painter, center: Pos2, radius: f32) {
    let donut_inner = radius + 4.0;
    let donut_outer = radius + 10.0;
    let donut_mid = (donut_inner + donut_outer) / 2.0;
    let donut_width = donut_outer - donut_inner;

    let color = Color32::from_rgba_unmultiplied(100, 100, 110, 80);
    let segments = 180;

    for i in 0..segments {
        let frac_start = i as f32 / segments as f32;
        let frac_end = (i + 1) as f32 / segments as f32;

        let a1 = (frac_start * 360.0 - 90.0) * PI / 180.0;
        let a2 = (frac_end * 360.0 - 90.0) * PI / 180.0;

        let p1 = Pos2::new(
            center.x + donut_mid * a1.cos(),
            center.y + donut_mid * a1.sin(),
        );
        let p2 = Pos2::new(
            center.x + donut_mid * a2.cos(),
            center.y + donut_mid * a2.sin(),
        );

        painter.line_segment([p1, p2], Stroke::new(donut_width, color));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_sweep_donut(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    sweep_az: f32,
    sweep_start: f32,
    state: &AppState,
    timeline: &crate::subsystem::Timeline,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
    _chrome: &crate::subsystem::Chrome,
) {
    let donut_inner = radius + 4.0;
    let donut_outer = radius + 10.0;
    let donut_mid = (donut_inner + donut_outer) / 2.0;
    let donut_width = donut_outer - donut_inner;

    let current_color = Color32::from_rgba_unmultiplied(80, 200, 120, 160);
    let prev_color = Color32::from_rgba_unmultiplied(120, 120, 180, 120);
    let current_text_color = Color32::from_rgb(100, 220, 140);
    let prev_text_color = Color32::from_rgb(160, 160, 220);

    let swept_arc_deg = (sweep_az - sweep_start).rem_euclid(360.0);
    let prev_arc_deg = 360.0 - swept_arc_deg;

    // Draw the two-tone arc ring
    let segments = 180;
    for i in 0..segments {
        let frac_start = i as f32 / segments as f32;
        let frac_end = (i + 1) as f32 / segments as f32;
        let mid_deg = (frac_start + frac_end) / 2.0 * 360.0;
        let color = if mid_deg < swept_arc_deg {
            current_color
        } else {
            prev_color
        };
        let a1 = ((sweep_start + frac_start * 360.0) - 90.0) * PI / 180.0;
        let a2 = ((sweep_start + frac_end * 360.0) - 90.0) * PI / 180.0;
        painter.line_segment(
            [
                Pos2::new(
                    center.x + donut_mid * a1.cos(),
                    center.y + donut_mid * a1.sin(),
                ),
                Pos2::new(
                    center.x + donut_mid * a2.cos(),
                    center.y + donut_mid * a2.sin(),
                ),
            ],
            Stroke::new(donut_width, color),
        );
    }

    let label_radius = donut_outer + 14.0;
    let label_font = egui::FontId::monospace(10.0);
    let use_local = state.use_local_time;
    let is_live = live.radar_model.active;

    // ── Gather sweep metadata for both slices ─────────────────────────
    // Helper to format a timestamp with age
    let frame_now = state.frame_now.secs();
    let fmt_time = |ts: f64| -> String {
        let mut s = format_time_short(ts, use_local);
        if let Some(age) = format_age_compact(frame_now, ts) {
            s.push(' ');
            s.push_str(&age);
        }
        s
    };

    // Helper to look up elevation angle from VCP pattern
    let elev_angle_str = |elev_num: u8, vcp: Option<&crate::data::ExtractedVcp>| -> String {
        vcp.and_then(|v| {
            v.elevations
                .get(elev_num.saturating_sub(1) as usize)
                .map(|el| format!("{:.1}\u{00B0}", el.angle))
        })
        .unwrap_or_default()
    };

    // Current sweep: time at data edge, time at sweep start, metadata
    let (cur_edge_time, cur_start_time, cur_meta): (
        Option<String>,
        Option<String>,
        Option<String>,
    );
    // Previous sweep: time at data edge (=sweep start boundary), metadata
    let (prev_edge_time, prev_meta): (Option<String>, Option<String>);

    if is_live {
        let model = &live.radar_model;
        let sweep = model.active_sweep.as_ref();
        let vcp = model.volume.as_ref().and_then(|v| v.vcp_pattern.as_ref());

        // Current sweep edge time: latest chunk end time
        cur_edge_time =
            sweep.and_then(|s| s.chunk_time_spans.last().map(|&(_, end, _)| fmt_time(end)));
        // Current sweep start time: first chunk start time
        cur_start_time = sweep.and_then(|s| {
            s.chunk_time_spans
                .first()
                .map(|&(start, _, _)| fmt_time(start))
        });

        // Current sweep metadata
        cur_meta = sweep.map(|s| {
            let angle = elev_angle_str(s.elevation_number, vcp);
            format!("Elev {} {}", s.elevation_number, angle)
        });

        // Previous sweep: last completed elevation, with timing from CachedSweep
        let prev_elev = model
            .volume
            .as_ref()
            .and_then(|v| v.roster.received.last().copied());
        let prev_sweep = prev_elev.and_then(|pe| {
            model
                .position
                .as_ref()?
                .sweeps
                .iter()
                .find(|s| s.elevation_number == pe && s.is_observed())
        });
        prev_edge_time = prev_sweep.map(|s| fmt_time(s.collection_end_secs));
        prev_meta = prev_elev.map(|pe| {
            let angle = elev_angle_str(pe, vcp);
            format!("Elev {} {}", pe, angle)
        });
    } else {
        // Cached playback
        let playback_ts = playback.state.playback_position();
        let displayed_elev = state
            .viz_state
            .displayed
            .as_ref()
            .map(|d| d.identity.elevation_number);

        // Look up current sweep from timeline. Display the VCP target
        // angle (the cut's identity) rather than the encoder's
        // measured average — see `Scan::display_angle`.
        let current_sweep_info = timeline
            .scans
            .find_recent_scan(playback_ts, 15.0 * 60.0)
            .and_then(|scan| {
                scan.sweeps
                    .iter()
                    .filter(|s| Some(s.elevation_number) == displayed_elev)
                    .rfind(|s| s.start_time <= playback_ts)
                    .or_else(|| {
                        scan.sweeps
                            .iter()
                            .find(|s| Some(s.elevation_number) == displayed_elev)
                    })
                    .map(|s| {
                        (
                            s.elevation_number,
                            scan.display_angle(s),
                            s.start_time,
                            s.end_time,
                        )
                    })
            });

        cur_edge_time = Some(fmt_time(playback_ts));
        cur_start_time = current_sweep_info.map(|(_, _, start, _)| fmt_time(start));
        cur_meta =
            current_sweep_info.map(|(en, angle, _, _)| format!("Elev {} {:.1}\u{00B0}", en, angle));

        // Previous sweep times — read from the on-GPU prior `displayed`
        // snapshot so labels track real pixel state.
        let prev = state.viz_state.previous_displayed.as_ref();
        prev_edge_time = prev.map(|p| fmt_time(p.end_time));
        prev_meta = prev.map(|p| {
            format!(
                "Elev {} {:.1}\u{00B0}",
                p.identity.elevation_number, p.elevation_deg
            )
        });

        // Also compute prev time at data edge for the data-edge boundary label
        // (interpolated from prev sweep's time range at the current sweep azimuth)
    }

    // Prev sweep time interpolated at the data-edge azimuth
    let prev_at_edge_time = if is_live {
        // Live: interpolate within the previous sweep's time range
        if let Some(sw) = live
            .radar_model
            .volume
            .as_ref()
            .and_then(|v| v.roster.received.last().copied())
            .and_then(|pe| {
                live.radar_model
                    .position
                    .as_ref()?
                    .sweeps
                    .iter()
                    .find(|s| s.elevation_number == pe && s.is_observed())
            })
        {
            let frac = (swept_arc_deg / 360.0).clamp(0.0, 1.0) as f64;
            Some(fmt_time(
                sw.collection_start_secs
                    + frac * (sw.collection_end_secs - sw.collection_start_secs),
            ))
        } else {
            None
        }
    } else {
        state.viz_state.previous_displayed.as_ref().map(|p| {
            let frac = (swept_arc_deg / 360.0).clamp(0.0, 1.0) as f64;
            fmt_time(p.start_time + frac * (p.end_time - p.start_time))
        })
    };

    // When the sweep is nearly complete (arc >= 350°), the two boundary
    // labels would overlap. Show a single "start | end" label instead.
    let full_sweep = swept_arc_deg >= 350.0;

    // Labels are drawn bottom-to-top: older/less-important first so that
    // more-recent labels paint over them when they overlap.

    if full_sweep {
        // ── Single combined boundary label for complete sweep ─────────
        let left = cur_start_time.as_deref().unwrap_or("");
        let right = cur_edge_time.as_deref();
        if !left.is_empty() || right.is_some() {
            draw_boundary_label(
                painter,
                center,
                label_radius,
                sweep_az,
                left,
                right,
                current_text_color,
                current_text_color,
                &label_font,
            );
        }
    } else {
        // ── Boundary label at sweep start (older boundary) ───────────
        if swept_arc_deg >= 30.0 {
            let left = cur_start_time.as_deref();
            let right = prev_edge_time.as_deref();
            match (left, right) {
                (Some(l), Some(r)) => {
                    draw_boundary_label(
                        painter,
                        center,
                        label_radius,
                        sweep_start,
                        l,
                        Some(r),
                        current_text_color,
                        prev_text_color,
                        &label_font,
                    );
                }
                (Some(l), None) => {
                    draw_boundary_label(
                        painter,
                        center,
                        label_radius,
                        sweep_start,
                        l,
                        None,
                        current_text_color,
                        current_text_color,
                        &label_font,
                    );
                }
                (None, Some(r)) => {
                    draw_boundary_label(
                        painter,
                        center,
                        label_radius,
                        sweep_start,
                        r,
                        None,
                        prev_text_color,
                        prev_text_color,
                        &label_font,
                    );
                }
                (None, None) => {}
            }
        }

        // ── Boundary label at data edge (most recent, drawn last) ────
        {
            let left = cur_edge_time.as_deref().unwrap_or("");
            let right = prev_at_edge_time.as_deref();
            if !left.is_empty() || right.is_some() {
                draw_boundary_label(
                    painter,
                    center,
                    label_radius,
                    sweep_az,
                    left,
                    right,
                    current_text_color,
                    prev_text_color,
                    &label_font,
                );
            }
        }
    }

    // ── Metadata label centered over the previous (purple) slice ─────
    if prev_arc_deg >= 30.0 && !full_sweep {
        if let Some(ref meta) = prev_meta {
            let mid_az = sweep_az + prev_arc_deg / 2.0;
            draw_boundary_label(
                painter,
                center,
                label_radius,
                mid_az,
                meta,
                None,
                prev_text_color,
                prev_text_color,
                &label_font,
            );
        }
    }

    // ── Metadata label centered over the current (green) slice ───────
    if swept_arc_deg >= 30.0 {
        if let Some(ref meta) = cur_meta {
            let mid_az = sweep_start + swept_arc_deg / 2.0;
            draw_boundary_label(
                painter,
                center,
                label_radius,
                mid_az,
                meta,
                None,
                current_text_color,
                current_text_color,
                &label_font,
            );
        }
    }
}

fn sweep_label_align(az_deg: f32) -> egui::Align2 {
    // Determine which quadrant the label falls in
    let az = az_deg.rem_euclid(360.0);
    if !(45.0..315.0).contains(&az) {
        egui::Align2::CENTER_BOTTOM // top
    } else if az < 135.0 {
        egui::Align2::LEFT_CENTER // right
    } else if az < 225.0 {
        egui::Align2::CENTER_TOP // bottom
    } else {
        egui::Align2::RIGHT_CENTER // left
    }
}

fn align_pos(pos: Pos2, size: Vec2, align: egui::Align2) -> Pos2 {
    let x = match align.x() {
        egui::Align::Min => pos.x,
        egui::Align::Center => pos.x - size.x / 2.0,
        egui::Align::Max => pos.x - size.x,
    };
    let y = match align.y() {
        egui::Align::Min => pos.y,
        egui::Align::Center => pos.y - size.y / 2.0,
        egui::Align::Max => pos.y - size.y,
    };
    Pos2::new(x, y)
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use eframe::egui::{Align2, Pos2, Vec2};
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---- cached_lon_range_deg ----
    // v = range_km * (1/111) / cos(lat_rad)

    #[wasm_bindgen_test]
    fn lon_range_at_equator_no_lat_correction() {
        // cos(0) = 1, so v = range_km / 111.
        let v = cached_lon_range_deg(0.0, -97.0, 300.0);
        assert!((v - 300.0 / 111.0).abs() < 1e-9, "got {}", v);
    }

    #[wasm_bindgen_test]
    fn lon_range_grows_with_latitude() {
        // cos(60deg) = 0.5, so the lon-degree extent roughly doubles vs equator.
        let eq = cached_lon_range_deg(0.0, 10.0, 300.0);
        let high = cached_lon_range_deg(60.0, 10.0, 300.0);
        assert!(high > eq, "high={} eq={}", high, eq);
        assert!((high - eq * 2.0).abs() < 1e-6, "got {}", high);
    }

    #[wasm_bindgen_test]
    fn lon_range_scales_linearly_with_range_km() {
        let single = cached_lon_range_deg(30.0, -80.0, 100.0);
        let triple = cached_lon_range_deg(30.0, -80.0, 300.0);
        assert!((triple - single * 3.0).abs() < 1e-9, "got {}", triple);
    }

    #[wasm_bindgen_test]
    fn lon_range_repeat_call_is_stable() {
        // Identical inputs hit the thread-local cache and must return identically.
        let a = cached_lon_range_deg(42.5, -71.0, 300.0);
        let b = cached_lon_range_deg(42.5, -71.0, 300.0);
        assert!((a - b).abs() < 1e-12, "a={} b={}", a, b);
    }

    // ---- sweep_label_align ----
    // Top sector (az not in [45,315)) -> CENTER_BOTTOM
    // right (45..135) -> LEFT_CENTER; bottom (135..225) -> CENTER_TOP;
    // left (225..315) -> RIGHT_CENTER.

    #[wasm_bindgen_test]
    fn sweep_align_north_is_center_bottom() {
        assert!(sweep_label_align(0.0) == Align2::CENTER_BOTTOM);
    }

    #[wasm_bindgen_test]
    fn sweep_align_east_is_left_center() {
        assert!(sweep_label_align(90.0) == Align2::LEFT_CENTER);
    }

    #[wasm_bindgen_test]
    fn sweep_align_south_is_center_top() {
        assert!(sweep_label_align(180.0) == Align2::CENTER_TOP);
    }

    #[wasm_bindgen_test]
    fn sweep_align_west_is_right_center() {
        assert!(sweep_label_align(270.0) == Align2::RIGHT_CENTER);
    }

    #[wasm_bindgen_test]
    fn sweep_align_boundaries() {
        // 45.0 is included in the right sector; 44.9 falls in the top sector.
        assert!(sweep_label_align(45.0) == Align2::LEFT_CENTER);
        assert!(sweep_label_align(44.9) == Align2::CENTER_BOTTOM);
        // 315.0 is excluded from the left sector -> top.
        assert!(sweep_label_align(315.0) == Align2::CENTER_BOTTOM);
        assert!(sweep_label_align(314.9) == Align2::RIGHT_CENTER);
    }

    #[wasm_bindgen_test]
    fn sweep_align_wraps_negative_via_rem_euclid() {
        // -10 -> 350 -> top sector.
        assert!(sweep_label_align(-10.0) == Align2::CENTER_BOTTOM);
        // -90 -> 270 -> left sector.
        assert!(sweep_label_align(-90.0) == Align2::RIGHT_CENTER);
    }

    // ---- align_pos ----

    #[wasm_bindgen_test]
    fn align_pos_min_min_is_identity() {
        let p = align_pos(
            Pos2::new(10.0, 20.0),
            Vec2::new(40.0, 8.0),
            Align2::LEFT_TOP,
        );
        assert!((p.x - 10.0).abs() < 1e-6, "x={}", p.x);
        assert!((p.y - 20.0).abs() < 1e-6, "y={}", p.y);
    }

    #[wasm_bindgen_test]
    fn align_pos_center_bottom() {
        // x centered (minus half width), y anchored to bottom (minus full height).
        let p = align_pos(
            Pos2::new(100.0, 50.0),
            Vec2::new(40.0, 8.0),
            Align2::CENTER_BOTTOM,
        );
        assert!((p.x - 80.0).abs() < 1e-6, "x={}", p.x);
        assert!((p.y - 42.0).abs() < 1e-6, "y={}", p.y);
    }

    #[wasm_bindgen_test]
    fn align_pos_right_center() {
        // x anchored to max (minus full width), y centered (minus half height).
        let p = align_pos(
            Pos2::new(100.0, 50.0),
            Vec2::new(40.0, 8.0),
            Align2::RIGHT_CENTER,
        );
        assert!((p.x - 60.0).abs() < 1e-6, "x={}", p.x);
        assert!((p.y - 46.0).abs() < 1e-6, "y={}", p.y);
    }

    #[wasm_bindgen_test]
    fn align_pos_left_center() {
        // x identity, y centered.
        let p = align_pos(
            Pos2::new(5.0, 30.0),
            Vec2::new(20.0, 10.0),
            Align2::LEFT_CENTER,
        );
        assert!((p.x - 5.0).abs() < 1e-6, "x={}", p.x);
        assert!((p.y - 25.0).abs() < 1e-6, "y={}", p.y);
    }

    #[wasm_bindgen_test]
    fn align_pos_center_top() {
        // x centered, y identity (Min).
        let p = align_pos(
            Pos2::new(100.0, 50.0),
            Vec2::new(40.0, 8.0),
            Align2::CENTER_TOP,
        );
        assert!((p.x - 80.0).abs() < 1e-6, "x={}", p.x);
        assert!((p.y - 50.0).abs() < 1e-6, "y={}", p.y);
    }
}
