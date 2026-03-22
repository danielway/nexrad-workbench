//! Overlay rendering: download ghosts, realtime progress, and saved events.

use super::DetailLevel;
use crate::state::radar_data::RadarTimeline;
use crate::state::SavedEvents;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, StrokeKind};

/// Render ghost blocks on the scan track for pending/active/processing downloads.
///
/// Distinct visual styles per state:
/// - Pending (queued): blue outline with diagonal stripe pattern
/// - Active (downloading): pulsing blue fill
/// - Processing (in_flight after download): amber tint
/// - Recently completed: brief green flash
#[allow(clippy::too_many_arguments)]
pub(super) fn render_download_ghosts(
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
/// - **Scan track**: Single VCP-colored block spanning vol_start -> expected_end,
///   with solid fill for elapsed time and dashed outline for projected remainder.
/// - **Sweep track**: All elevation sweeps with per-sweep state:
///   - Complete (downloaded & persisted): filled with cool elevation colors
///   - Downloading (in-progress): outline with chunk subdivision inside
///   - Future (not yet collected): dashed outline
///     Each non-complete sweep shows chunk subdivision where downloaded chunks
///     are clipped to the sweep's time range.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_realtime_progress(
    painter: &Painter,
    scan_rect: &Rect,
    sweep_rect: Option<&Rect>,
    live_state: &crate::state::LiveModeState,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    _anim_time: f64,
    now_secs: f64,
    selected_elevation_number: Option<u8>,
    active_sweep: Option<(i64, u8)>,
    prev_active_sweep: Option<(i64, u8)>,
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

    // ===================================================================
    // SCAN TRACK -- single in-progress block (vol_start -> expected_end)
    // ===================================================================

    let scan_block = Rect::from_min_max(
        Pos2::new(x_vol_start, scan_rect.top() + 2.0),
        Pos2::new(x_vol_end, scan_rect.bottom() - 2.0),
    );

    // VCP-colored fill -- use the same warm palette as completed scans but
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

    // -- Projected future scan boundaries (dashed lines) --
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

    // ===================================================================
    // SWEEP TRACK -- all elevation sweeps with per-sweep state + chunks
    // ===================================================================

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
        let matches_target = selected_elevation_number.is_none_or(|num| elev_num == num);
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
            let is_prev_active =
                !is_active && prev_active_sweep.is_some_and(|(_, active_en)| active_en == elev_num);
            let fill = tl_colors::sweep_fill(elev_angle, matches_target);
            let border = if is_prev_active {
                tl_colors::PREV_ACTIVE_SWEEP
            } else {
                tl_colors::sweep_border(elev_angle, is_active)
            };
            painter.rect_filled(block, 1.0, fill);
            if width > 3.0 {
                let stroke_width = if is_active {
                    2.0
                } else if is_prev_active {
                    1.5
                } else {
                    0.5
                };
                let stroke_kind = if is_active || is_prev_active {
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

            // Expected-chunk subdivision ticks: faint lines at regular
            // intervals showing where chunks are expected to fall.
            let expected_chunks = live_state.expected_chunks_for_current_sweep();
            if let Some(exp_n) = expected_chunks {
                if exp_n >= 2 {
                    let tick_color = Color32::from_rgba_unmultiplied(100, 160, 220, 60);
                    for tick_i in 1..exp_n {
                        let tick_frac = tick_i as f32 / exp_n as f32;
                        let tick_x = block.min.x + block.width() * tick_frac;
                        if tick_x > block.min.x + 2.0 && tick_x < block.max.x - 2.0 {
                            painter.line_segment(
                                [
                                    Pos2::new(tick_x, block.min.y + 1.0),
                                    Pos2::new(tick_x, block.max.y - 1.0),
                                ],
                                Stroke::new(0.5, tick_color),
                            );
                        }
                    }
                }
            }

            // Draw downloaded chunks that belong to this elevation, with
            // clear separators between each chunk boundary.  Chunks are
            // rendered shorter than the sweep block so they visually nest
            // inside it, making the two layers easy to distinguish.
            let chunk_inset = 3.5_f32;
            let chunk_top = block.min.y + chunk_inset;
            let chunk_bot = block.max.y - chunk_inset;
            let mut prev_chunk_end_x: Option<f32> = None;
            for &(span_elev, span_start, span_end, _) in &live_state.chunk_elev_spans {
                if span_elev != elev_num {
                    continue;
                }
                let cx0 = ts_to_x(span_start).max(sweep_rect.left());
                let cx1 = ts_to_x(span_end).min(sweep_rect.right());
                if cx1 > cx0 {
                    let chunk_rect =
                        Rect::from_min_max(Pos2::new(cx0, chunk_top), Pos2::new(cx1, chunk_bot));
                    painter.rect_filled(
                        chunk_rect,
                        1.0,
                        Color32::from_rgba_unmultiplied(80, 170, 230, 70),
                    );
                    painter.rect_stroke(
                        chunk_rect,
                        1.0,
                        Stroke::new(0.5, Color32::from_rgba_unmultiplied(100, 180, 255, 90)),
                        StrokeKind::Inside,
                    );

                    // Separator tick at each chunk boundary
                    if let Some(prev_x) = prev_chunk_end_x {
                        // Draw separator at the boundary between previous and current chunk
                        let sep_x = (prev_x + cx0) / 2.0;
                        painter.line_segment(
                            [Pos2::new(sep_x, chunk_top), Pos2::new(sep_x, chunk_bot)],
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

            // -- Next-chunk placeholder block --
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

            // Chunk count for labeling (e.g., "2/6")
            let received_chunk_count: u32 = live_state
                .chunk_elev_spans
                .iter()
                .filter(|&&(e, _, _, _)| e == elev_num)
                .count() as u32
                + if in_progress_radials > 0 { 1 } else { 0 };
            let chunk_label = expected_chunks
                .map(|exp| format!("{}/{}", received_chunk_count, exp))
                .unwrap_or_else(|| format!("{}c", received_chunk_count));

            // Radial progress label in the filled (collected) portion
            if countdown.is_none() && width > 30.0 {
                // Show chunk count + radial progress while actively receiving
                let label = if width > 70.0 {
                    format!(
                        "{} \u{00B7} {}/{}",
                        chunk_label, total_radials, expected_radials
                    )
                } else if width > 45.0 {
                    chunk_label.clone()
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
                // When waiting, show chunk count in the collected portion
                let collected_center_x = (block.min.x + edge_x) / 2.0;
                let collected_width = edge_x - block.min.x;
                if collected_width > 25.0 {
                    let label = if collected_width > 50.0 {
                        chunk_label
                    } else {
                        format!("{}", total_radials)
                    };
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
                // -- Next-chunk placeholder on the first future sweep --
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
pub(super) fn render_saved_events(
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
