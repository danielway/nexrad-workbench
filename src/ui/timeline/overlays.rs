//! Overlay rendering: download ghosts, realtime progress, and saved events.

use super::strokes::{fill_diagonal_hatch, stroke_dashed_rect, DashedBorder, DashedEdges};
use super::DetailLevel;
use crate::state::{LiveOverlayContext, SavedEvents, TimelineView};
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
    view: &TimelineView<'_>,
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
        // Resolve the flash to its target cached scan's bounds via the view's
        // tighter completion-match tolerance.
        if let Some((scan_start_time, scan_end_time)) = view.completion_target(scan_start) {
            let x_start = ts_to_x(scan_start_time).max(rect.left());
            let x_end = ts_to_x(scan_end_time).min(rect.right());
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

        // Skip if real data already covers this timestamp.
        if view.is_covered_by_cached(scan_start) {
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
            fill_diagonal_hatch(
                painter,
                ghost_rect,
                8.0,
                0.0,
                Stroke::new(0.5, tl_colors::ghost_pending_fill()),
            );
        }
    };

    // Draw pending scans
    for &(s, e) in &progress.pending_scans {
        draw_ghost(s, e, false, false);
    }

    // Draw active scans
    for &(s, e) in &progress.active_scans {
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
///
///   Each non-complete sweep shows chunk subdivision where downloaded chunks
///   are clipped to the sweep's time range.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_realtime_progress(
    painter: &Painter,
    scan_rect: &Rect,
    sweep_rect: Option<&Rect>,
    model: &crate::nexrad::projection::ScanProjection,
    ctx: &LiveOverlayContext,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    _anim_time: f64,
    now_secs: f64,
    selected_elevation_number: Option<u8>,
    active_sweep: Option<(f64, u8)>,
    prev_active_sweep: Option<(f64, u8)>,
) {
    let ts_to_x = |ts: f64| -> f32 { scan_rect.left() + ((ts - view_start) * zoom) as f32 };
    let now = now_secs;

    let vol_start = model.volume_start;
    let vcp = model.vcp_number;
    let expected_end = model.volume_end;
    let expected_dur = expected_end - vol_start;
    let expected_count = model.sweeps.len();

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
        let remainder = Rect::from_min_max(
            Pos2::new(x_now, scan_block.min.y),
            Pos2::new(x_vol_end, scan_block.max.y),
        );
        // Right edge at 1px stroke, period 7.
        stroke_dashed_rect(
            painter,
            remainder,
            DashedBorder::rect(Stroke::new(1.0, dash_color), 0.0, 0.0, 4.0, 7.0).with_edges(
                DashedEdges {
                    top: false,
                    bottom: false,
                    left: false,
                    right: true,
                },
            ),
        );
        // Top and bottom at thinner 0.5 stroke, period 8.
        stroke_dashed_rect(
            painter,
            remainder,
            DashedBorder::uniform(Stroke::new(0.5, dash_color), 4.0, 8.0)
                .with_edges(DashedEdges::HORIZONTAL),
        );
    }

    // Unified label centered across the full scan block
    let full_width = x_vol_end - x_vol_start;
    if full_width > 40.0 {
        let received = model.completed_count();

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
                super::style::block_font(),
                tl_colors::block_label(),
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
                // Zero-width "rect" collapses to a single dashed vertical line.
                stroke_dashed_rect(
                    painter,
                    Rect::from_min_max(
                        Pos2::new(x, scan_rect.top()),
                        Pos2::new(x, scan_rect.bottom()),
                    ),
                    DashedBorder::rect(Stroke::new(1.0, boundary_color), 0.0, 0.0, 4.0, 7.0)
                        .with_edges(DashedEdges {
                            top: false,
                            bottom: false,
                            left: true,
                            right: false,
                        }),
                );
                painter.text(
                    Pos2::new(x + 3.0, scan_rect.top() + 2.0),
                    egui::Align2::LEFT_TOP,
                    "est.",
                    super::style::block_font(),
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

    let received = &ctx.elevations_received;
    let in_progress_elev = ctx.in_progress_elevation;
    let in_progress_radials = ctx.in_progress_radials;
    let countdown = ctx.countdown_secs;
    // When the next download target is in the next volume, its countdown
    // belongs to the next-volume ghost — not a current-volume future sweep.
    // Suppress the current-volume "first future" placeholder in that case so
    // the ghost doesn't render behind the playhead over the cached sweep.
    let next_in_next_volume = ctx.next_target_in_next_volume;

    for sweep_pos in &model.sweeps {
        let elev_num = sweep_pos.elevation_number;
        let sw_start = sweep_pos.collection_start_secs;
        let sw_end = sweep_pos.collection_end_secs;
        let is_complete = sweep_pos.is_complete();
        let is_downloading = sweep_pos.is_in_progress();
        let is_future = sweep_pos.is_future();

        let x_start = ts_to_x(sw_start).max(sweep_rect.left());
        let x_end = ts_to_x(sw_end).min(sweep_rect.right());
        if x_end - x_start < 1.0 || sw_end < view_start || sw_start > view_end {
            continue;
        }

        let elev_angle = sweep_pos.elevation_angle;
        let matches_target = selected_elevation_number.is_none_or(|num| elev_num == num);

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
            // -- Downloading: evenly-divided chunk slots --
            // The sweep is divided into N equal slots where N = chunks_expected.
            // Received chunks fill slots left-to-right; the next expected chunk
            // shows a countdown placeholder.

            let (chunks_received, chunks_expected_opt) = if sweep_pos.is_in_progress() {
                (
                    sweep_pos.chunks_received,
                    (sweep_pos.chunks_in_sweep > 0).then_some(sweep_pos.chunks_in_sweep as u32),
                )
            } else {
                (0, None)
            };
            let exp_n = chunks_expected_opt.unwrap_or(3).max(1);
            let chunk_width = block.width() / exp_n as f32;
            let chunk_inset = 3.0_f32;
            let chunk_top = block.min.y + chunk_inset;
            let chunk_bot = block.max.y - chunk_inset;

            // The last received chunk may still be accumulating radials (partial).
            // chunk_elev_spans already includes the current chunk, so don't add +1.
            let is_last_chunk_partial = in_progress_radials > 0 && chunks_received > 0;

            for slot in 0..exp_n {
                let slot_x0 = block.min.x + slot as f32 * chunk_width;
                let slot_x1 = block.min.x + (slot + 1) as f32 * chunk_width;
                let slot_rect = Rect::from_min_max(
                    Pos2::new(slot_x0, chunk_top),
                    Pos2::new(slot_x1, chunk_bot),
                );

                if is_last_chunk_partial && slot == chunks_received - 1 {
                    // ── Last received chunk, still accumulating (partial fill) ──
                    let radials_per_chunk = (360.0 / exp_n as f32).max(1.0);
                    let partial_frac =
                        (in_progress_radials as f32 / radials_per_chunk).clamp(0.0, 1.0);
                    if partial_frac > 0.0 {
                        let partial_rect = Rect::from_min_max(
                            slot_rect.min,
                            Pos2::new(
                                slot_rect.min.x + slot_rect.width() * partial_frac,
                                slot_rect.max.y,
                            ),
                        );
                        painter.rect_filled(
                            partial_rect,
                            1.0,
                            Color32::from_rgba_unmultiplied(80, 170, 230, 70),
                        );
                    }
                    // Dashed border for the partial slot (top + bottom only)
                    let border_color = Color32::from_rgba_unmultiplied(100, 180, 255, 70);
                    stroke_dashed_rect(
                        painter,
                        slot_rect,
                        DashedBorder::uniform(Stroke::new(0.5, border_color), 3.0, 6.0)
                            .with_edges(DashedEdges::HORIZONTAL),
                    );
                } else if slot < chunks_received {
                    // ── Received (complete) chunk slot ──
                    painter.rect_filled(
                        slot_rect,
                        1.0,
                        Color32::from_rgba_unmultiplied(80, 170, 230, 70),
                    );
                    painter.rect_stroke(
                        slot_rect,
                        1.0,
                        Stroke::new(0.5, Color32::from_rgba_unmultiplied(100, 180, 255, 90)),
                        StrokeKind::Inside,
                    );
                } else if slot == chunks_received && countdown.is_some() {
                    // ── Next-chunk placeholder with countdown ──
                    painter.rect_filled(slot_rect, 1.0, tl_colors::rt_next_chunk_fill());
                    let dot_color = tl_colors::rt_next_chunk_border();
                    stroke_dashed_rect(
                        painter,
                        slot_rect,
                        DashedBorder::uniform(Stroke::new(1.0, dot_color), 2.0, 4.0),
                    );
                    // Countdown label
                    if let Some(remaining) = countdown {
                        if slot_rect.width() > 16.0 {
                            painter.text(
                                slot_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                format!("{}s", remaining.ceil() as i32),
                                super::style::block_font(),
                                tl_colors::rt_next_chunk_label(),
                            );
                        }
                    }
                }
                // Future slots beyond the next-chunk placeholder are left empty
                // (the sweep's dashed border already indicates estimated bounds).
            }

            // Dashed border around the entire sweep block
            let border_color = Color32::from_rgba_unmultiplied(60, 140, 200, 100);
            stroke_dashed_rect(
                painter,
                block,
                DashedBorder::rect(Stroke::new(1.0, border_color), 4.0, 8.0, 3.0, 6.0),
            );
        } else if is_future {
            // Check if this is the first future sweep (next to receive data)
            // and we're waiting for a chunk with no downloading sweep active.
            let is_next_sweep = in_progress_elev.is_none()
                && countdown.is_some()
                && !next_in_next_volume
                && !received.iter().any(|&e| e > elev_num);

            // For the "next" sweep, also check it's the very first future one
            let is_first_future = is_next_sweep
                && (elev_num == 1 || received.last().is_some_and(|&last| last == elev_num - 1));

            if is_first_future {
                // -- Next-chunk placeholder on the first future sweep --
                // Sized to one estimated chunk slot. Standard sweeps are 3
                // chunks; super-res are 6. Without per-sweep projection we
                // default to 3 — the slot is a placeholder, not a precise
                // count.
                let future_exp_n: u32 = 3;
                let slot_width = block.width() / future_exp_n as f32;
                let nc_end_x = (block.min.x + slot_width.max(8.0)).min(block.max.x);
                let nc_rect = Rect::from_min_max(
                    Pos2::new(block.min.x, block.min.y),
                    Pos2::new(nc_end_x, block.max.y),
                );
                let nc_width = nc_rect.width();

                let nc_fill = tl_colors::rt_next_chunk_fill();
                let dot_color = tl_colors::rt_next_chunk_border();

                painter.rect_filled(nc_rect, 1.0, nc_fill);

                // Dotted border (2px on, 2px off)
                stroke_dashed_rect(
                    painter,
                    nc_rect,
                    DashedBorder::uniform(Stroke::new(1.0, dot_color), 2.0, 4.0),
                );

                // Countdown label
                if let Some(remaining) = countdown {
                    if nc_width > 16.0 {
                        painter.text(
                            nc_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("{}s", remaining.ceil() as i32),
                            super::style::block_font(),
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
                    stroke_dashed_rect(
                        painter,
                        rest_block,
                        DashedBorder::rect(Stroke::new(0.5, dash_color), 4.0, 8.0, 3.0, 6.0)
                            .with_edges(DashedEdges {
                                top: true,
                                bottom: true,
                                left: false,
                                right: true,
                            }),
                    );
                }
            } else {
                // -- Regular future: dashed outline to indicate estimated bounds --
                let dash_color = tl_colors::rt_pending_sweep_border();
                stroke_dashed_rect(
                    painter,
                    block,
                    DashedBorder::rect(Stroke::new(0.5, dash_color), 4.0, 8.0, 3.0, 6.0),
                );
            }
        }

        // Elevation label (for all states, when wide enough)
        if width > 25.0 && !is_downloading {
            let label = if width > 50.0 {
                format!("{:.1}\u{00B0}", elev_angle)
            } else {
                format!("{:.1}", elev_angle)
            };
            let label_color = if is_complete {
                tl_colors::block_label()
            } else {
                tl_colors::block_label_weak()
            };
            painter.text(
                block.center(),
                egui::Align2::CENTER_CENTER,
                label,
                super::style::block_font(),
                label_color,
            );
        }
    }

    // ===================================================================
    // NEXT-SCAN GHOST -- projected next volume, drawn faded
    // ===================================================================
    //
    // Present only when an elevation filter is active and the target has no
    // remaining matches in the current volume — so the user's next download
    // will land in the next volume. Rendered with a reduced-alpha treatment
    // so it reads as "projected" without competing with the live scan.
    if let Some(ghost) = model.next_scan_ghost.as_deref() {
        render_ghost_volume_overlay(
            painter,
            scan_rect,
            sweep_rect,
            ghost,
            view_start,
            view_end,
            zoom,
            selected_elevation_number,
            // The next-chunk countdown predicts the next-volume target, so
            // it rides along with the ghost's matching sweep rather than a
            // current-volume placeholder.
            if next_in_next_volume { countdown } else { None },
            ctx.next_target_elevation,
        );
    }
}

/// Render the projected next-volume ghost: a faded scan block and a row of
/// dashed-outlined future sweeps. The matching target sweep (if the user
/// has an elevation filter active) is rendered with the same target tint as
/// in the current scan, just at lower alpha.
#[allow(clippy::too_many_arguments)]
fn render_ghost_volume_overlay(
    painter: &Painter,
    scan_rect: &Rect,
    sweep_rect: &Rect,
    ghost: &crate::nexrad::projection::ScanProjection,
    view_start: f64,
    view_end: f64,
    zoom: f64,
    selected_elevation_number: Option<u8>,
    countdown: Option<f64>,
    next_target_elevation: Option<u8>,
) {
    let ts_to_x = |ts: f64| -> f32 { scan_rect.left() + ((ts - view_start) * zoom) as f32 };
    let vol_start = ghost.volume_start;
    let vol_end = ghost.volume_end;
    if vol_end <= vol_start || vol_end < view_start || vol_start > view_end {
        return;
    }
    let x_start = ts_to_x(vol_start).max(scan_rect.left());
    let x_end = ts_to_x(vol_end).min(scan_rect.right());
    if x_end - x_start < 2.0 {
        return;
    }

    // Scan-track ghost block: faded VCP color with a dashed outline so it
    // visibly reads as "projected, not live."
    let (vr, vg, vb) = tl_colors::vcp_base_rgb(ghost.vcp_number);
    let block = Rect::from_min_max(
        Pos2::new(x_start, scan_rect.top() + 2.0),
        Pos2::new(x_end, scan_rect.bottom() - 2.0),
    );
    painter.rect_filled(block, 2.0, Color32::from_rgba_unmultiplied(vr, vg, vb, 28));
    let dash_color = Color32::from_rgba_unmultiplied(vr, vg, vb, 70);
    stroke_dashed_rect(
        painter,
        block,
        DashedBorder::rect(Stroke::new(1.0, dash_color), 4.0, 8.0, 4.0, 8.0),
    );

    if (x_end - x_start) > 60.0 {
        painter.text(
            block.center(),
            egui::Align2::CENTER_CENTER,
            format!("next VCP {}", ghost.vcp_number),
            super::style::block_font(),
            tl_colors::block_label_weak(),
        );
    }

    // Sweep-track ghost row: dashed outline per future sweep, with the
    // user-selected target highlighted.
    for sweep_pos in &ghost.sweeps {
        let elev_num = sweep_pos.elevation_number;
        let sw_start = sweep_pos.collection_start_secs;
        let sw_end = sweep_pos.collection_end_secs;
        let x_a = ts_to_x(sw_start).max(sweep_rect.left());
        let x_b = ts_to_x(sw_end).min(sweep_rect.right());
        if x_b - x_a < 1.0 || sw_end < view_start || sw_start > view_end {
            continue;
        }
        let block = Rect::from_min_max(
            Pos2::new(x_a, sweep_rect.top() + 2.0),
            Pos2::new(x_b, sweep_rect.bottom() - 2.0),
        );
        let elev_angle = sweep_pos.elevation_angle;
        let matches_target = selected_elevation_number.is_none_or(|num| elev_num == num);

        // Faded fill + dashed outline. When the user's target lives in this
        // sweep, tint it with the same target color used in the current
        // scan, just at lower alpha so it still reads as "ghost."
        if matches_target && selected_elevation_number.is_some() {
            let fill = tl_colors::sweep_fill(elev_angle, true);
            let mut fill = fill;
            fill = Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), fill.a() / 2);
            painter.rect_filled(block, 1.0, fill);
        }
        let dash_color = tl_colors::rt_pending_sweep_border();
        let dash_color = Color32::from_rgba_unmultiplied(
            dash_color.r(),
            dash_color.g(),
            dash_color.b(),
            dash_color.a() / 2,
        );
        stroke_dashed_rect(
            painter,
            block,
            DashedBorder::rect(Stroke::new(0.5, dash_color), 4.0, 8.0, 3.0, 6.0),
        );

        // Next-chunk countdown placeholder on the ghost's target sweep. The
        // next download lands here (next volume), so the "Xs" countdown rides
        // with this sweep instead of a current-volume placeholder. Sized to
        // one estimated chunk slot, mirroring the in-volume next-chunk style.
        if let (Some(remaining), Some(target_elev)) = (countdown, next_target_elevation) {
            if elev_num == target_elev {
                let future_exp_n: u32 = 3;
                let slot_width = block.width() / future_exp_n as f32;
                let nc_end_x = (block.min.x + slot_width.max(8.0)).min(block.max.x);
                let nc_rect = Rect::from_min_max(
                    Pos2::new(block.min.x, block.min.y),
                    Pos2::new(nc_end_x, block.max.y),
                );
                painter.rect_filled(nc_rect, 1.0, tl_colors::rt_next_chunk_fill());
                stroke_dashed_rect(
                    painter,
                    nc_rect,
                    DashedBorder::uniform(
                        Stroke::new(1.0, tl_colors::rt_next_chunk_border()),
                        2.0,
                        4.0,
                    ),
                );
                if nc_rect.width() > 16.0 {
                    painter.text(
                        nc_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("{}s", remaining.ceil() as i32),
                        super::style::block_font(),
                        tl_colors::rt_next_chunk_label(),
                    );
                }
            }
        }

        // Elevation label (faded)
        let width = x_b - x_a;
        if width > 25.0 {
            let label = if width > 50.0 {
                format!("{:.1}\u{00B0}", elev_angle)
            } else {
                format!("{:.1}", elev_angle)
            };
            painter.text(
                block.center(),
                egui::Align2::CENTER_CENTER,
                label,
                super::style::block_font(),
                tl_colors::block_label_weak(),
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
