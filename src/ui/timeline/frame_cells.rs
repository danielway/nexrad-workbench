//! Frames-first cell painter (spec §6.2 / §6.3).
//!
//! ONE cell-painting code path shared by every container — settled cached
//! scans, archive shadows, and the live in-progress volume all flow through
//! [`paint_container`]. The strip's primary unit is the frame cell (a sweep of
//! the selected product + tilt); the full volume structure renders only as a
//! faint neutral sub-texture inside each scan container.
//!
//! Visual grammar, distinguishable by fill + shape so it reads in grayscale
//! (accent budget: only the playhead, the live edge, and the active-frame ring
//! are colored — everything else is neutral):
//!   - Available  → hollow dashed outline
//!   - Cached     → solid fill
//!   - In flight  → live chunk slots (3/6, faithful) or a pulsing fill (archive)
//!   - Queued     → faint diagonal hatch
//!   - Projected  → dashed ghost; the nearest one carries a "0.5° in ~40s" countdown
//!   - Failed     → small red alert triangle; its hit-rect is returned for retry
//!   - Active     → yellow accent ring snapping cell-to-cell

use super::strokes::{fill_hatched_rect, stroke_dashed_rect, DashedBorder};
use super::{style, TimelineFrame};
use crate::core::{FrameCell, FrameCellState, ScanContainer};
use crate::ui::colors::acquisition as acq_colors;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, StrokeKind};

/// A failed frame cell's clickable tick, returned so the orchestrator can wire
/// a retry. `key_secs` is the container scan-start (the join key the retry
/// targets).
pub(super) struct FailedTick {
    pub rect: Rect,
    pub key_secs: f64,
}

/// Per-frame animation inputs threaded to the cell painter.
#[derive(Clone, Copy)]
pub(super) struct CellAnim {
    /// Pulse value 0..1 (sin phase); used for archive in-flight fill.
    pub pulse: f32,
    /// True when the host treats motion as reduced (see [`super::reduced_motion`]):
    /// pulses become a static partial fill instead of animating.
    pub reduced_motion: bool,
    /// Morph factor 0..1 (Macro→Micro). 1 = fully expanded frame cells; <1
    /// collapses cell height toward the track centre so the cells visibly fold
    /// into ticks during a tier transition (spec §6.4). The playhead + live
    /// edge stay position-stable because only cell HEIGHT animates, not x.
    pub morph: f32,
}

/// Paint one scan container and its frame cells onto the main track. Appends
/// any failed-cell ticks to `failed_ticks`. When `carries_countdown` is set,
/// the container owns the "next data" countdown: it lands on the in-flight cell
/// (next chunk) if one has chunk telemetry, otherwise on the first projected
/// cell ("0.5° in ~Ns"). `countdown_secs` is the remaining time.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_container(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    container: &ScanContainer,
    anim: CellAnim,
    countdown_secs: Option<f64>,
    carries_countdown: bool,
    failed_ticks: &mut Vec<FailedTick>,
) {
    let track = &frame.rects.scan;
    let dark = frame.dark;
    let x0 = frame.ts_to_x(container.start_secs).max(track.left());
    let x1 = frame.ts_to_x(container.end_secs).min(track.right());
    if x1 <= x0 {
        return;
    }

    let box_rect = Rect::from_min_max(
        Pos2::new(x0, track.top() + style::CONTAINER_INSET_Y),
        Pos2::new(x1, track.bottom() - style::CONTAINER_INSET_Y),
    );

    // 1. Container bounding box — a subtle neutral frame. Available containers
    //    (server-only) read as hollow (dashed); cached/live containers get a
    //    faint solid box.
    if box_rect.width() >= 2.0 {
        if container.is_available {
            stroke_dashed_rect(
                painter,
                box_rect,
                DashedBorder::uniform(
                    Stroke::new(1.0_f32, tl_colors::container_border(dark)),
                    4.0,
                    7.0,
                ),
            );
        } else {
            painter.rect_stroke(
                box_rect,
                2.0,
                Stroke::new(1.0_f32, tl_colors::container_border(dark)),
                StrokeKind::Inside,
            );
        }
    }

    // 2. Faint sub-texture: thin vertical sweep-boundary lines of the FULL
    //    volume (every sweep, not just matching tilts), neutral. Skipped for
    //    available containers (no known structure) and very narrow boxes.
    if box_rect.width() > 6.0 {
        let tex = tl_colors::sub_texture(dark);
        let top = box_rect.top() + 1.0;
        let bot = box_rect.bottom() - 1.0;
        for &(s, _e) in &container.sweep_spans {
            let x = frame.ts_to_x(s);
            if x > box_rect.left() + 0.5 && x < box_rect.right() - 0.5 {
                painter.line_segment(
                    [Pos2::new(x, top), Pos2::new(x, bot)],
                    Stroke::new(0.5_f32, tex),
                );
            }
        }
    }

    // 3. Frame cells, all through one painter. Cells inset inside the box;
    //    during a morph their height collapses toward the box centre (cells
    //    fold into ticks) by interpolating both edges to the centre as
    //    `morph → 0`.
    let full_top = box_rect.top() + style::CELL_INSET_Y;
    let full_bot = box_rect.bottom() - style::CELL_INSET_Y;
    let cy = box_rect.center().y;
    let m = anim.morph.clamp(0.0, 1.0);
    let cell_top = cy + (full_top - cy) * m;
    let cell_bot = cy + (full_bot - cy) * m;
    let mut first_projected_done = false;
    for cell in &container.cells {
        let cx0 = frame.ts_to_x(cell.start_secs).max(box_rect.left());
        let cx1 = frame.ts_to_x(cell.end_secs).min(box_rect.right());
        if cx1 - cx0 < 0.5 {
            continue;
        }
        let cell_rect = Rect::from_min_max(Pos2::new(cx0, cell_top), Pos2::new(cx1, cell_bot));
        let carry = carries_countdown && !first_projected_done;
        let consumed = paint_cell(
            painter,
            frame,
            cell,
            cell_rect,
            box_rect,
            anim,
            if carry { countdown_secs } else { None },
            failed_ticks,
            container.key_secs,
        );
        if consumed {
            first_projected_done = true;
        }
    }

    // 4. Faint VCP identity at the container's bottom-left when there is room
    //    and the morph hasn't collapsed the cells (structure label, not color).
    if container.vcp > 0 && box_rect.width() > 64.0 && m > 0.6 {
        painter.text(
            Pos2::new(box_rect.left() + 3.0, box_rect.bottom() - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("VCP {}", container.vcp),
            style::block_font(),
            tl_colors::block_label_weak(),
        );
    }
}

/// Paint a single frame cell. Returns true when this cell consumed the
/// container's countdown slot (i.e. it was the first projected cell that drew
/// the countdown), so the caller marks the countdown as placed.
#[allow(clippy::too_many_arguments)]
fn paint_cell(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    cell: &FrameCell,
    rect: Rect,
    box_rect: Rect,
    anim: CellAnim,
    countdown_secs: Option<f64>,
    failed_ticks: &mut Vec<FailedTick>,
    key_secs: f64,
) -> bool {
    let dark = frame.dark;
    let width = rect.width();
    let mut consumed_countdown = false;

    match cell.state {
        FrameCellState::Cached => {
            painter.rect_filled(rect, 1.0, tl_colors::cell_cached(dark));
        }
        FrameCellState::Available => {
            painter.rect_filled(rect, 1.0, tl_colors::cell_available_fill(dark));
            stroke_dashed_rect(
                painter,
                rect,
                DashedBorder::uniform(
                    Stroke::new(1.0_f32, tl_colors::cell_available_border(dark)),
                    4.0,
                    7.0,
                ),
            );
        }
        FrameCellState::InFlight => {
            paint_inflight(painter, frame, cell, rect, anim, countdown_secs);
            // A live in-flight cell with chunk telemetry draws the next-chunk
            // "Ns" in its filling slot; consume the container's countdown so a
            // later projected cell in the same container doesn't double it.
            if countdown_secs.is_some() && cell.chunks.is_some() {
                consumed_countdown = true;
            }
        }
        FrameCellState::Queued => {
            // Faint diagonal hatch — distinct from the dashed Available outline.
            fill_hatched_rect(
                painter,
                rect,
                Stroke::new(0.75_f32, tl_colors::cell_queued_hatch(dark)),
                4.0,
            );
        }
        FrameCellState::Projected => {
            painter.rect_filled(rect, 1.0, tl_colors::cell_projected_fill(dark));
            stroke_dashed_rect(
                painter,
                rect,
                DashedBorder::uniform(
                    Stroke::new(0.75_f32, tl_colors::cell_projected_border(dark)),
                    3.0,
                    6.0,
                ),
            );
            // Nearest ghost: countdown including tilt identity ("0.5° in ~40s").
            if let Some(remaining) = countdown_secs {
                consumed_countdown = true;
                if width > 22.0 {
                    let label = format!(
                        "{:.1}\u{00B0} in ~{}s",
                        cell.elevation_angle,
                        remaining.ceil() as i32
                    );
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        style::block_font(),
                        tl_colors::cell_countdown_label(dark),
                    );
                }
            }
        }
        FrameCellState::Failed => {
            // Faint hollow body so the cell still has presence, then a red
            // alert triangle tick (the only red besides the live edge).
            painter.rect_filled(rect, 1.0, tl_colors::cell_available_fill(dark));
            stroke_dashed_rect(
                painter,
                rect,
                DashedBorder::uniform(
                    Stroke::new(1.0_f32, tl_colors::cell_available_border(dark)),
                    3.0,
                    6.0,
                ),
            );
            let tick = failure_tick(painter, rect);
            failed_ticks.push(FailedTick {
                rect: tick,
                key_secs,
            });
        }
    }

    // Active-frame accent ring snaps to the on-GPU cell (spec §6.2 last row).
    if cell.is_active {
        painter.rect_stroke(
            rect,
            1.0,
            Stroke::new(2.0_f32, tl_colors::ACTIVE_SWEEP),
            StrokeKind::Outside,
        );
    } else if cell.is_prev_active {
        painter.rect_stroke(
            rect,
            1.0,
            Stroke::new(1.5_f32, tl_colors::PREV_ACTIVE_SWEEP),
            StrokeKind::Outside,
        );
    }

    // Elevation/identity label when wide enough and not an in-flight slot grid.
    let _ = box_rect;
    if width > 28.0
        && !matches!(
            cell.state,
            FrameCellState::InFlight | FrameCellState::Projected
        )
    {
        // When the angle is unknown (assumed Available cell), fall back to the
        // elevation number so the cell still names its tilt.
        let label = if cell.elevation_angle <= 0.0 && cell.elevation_number > 0 {
            format!("E{}", cell.elevation_number)
        } else if width > 52.0 {
            format!("{:.1}\u{00B0}", cell.elevation_angle)
        } else {
            format!("{:.1}", cell.elevation_angle)
        };
        let color = match cell.state {
            FrameCellState::Cached => tl_colors::block_label(),
            _ => tl_colors::block_label_weak(),
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            style::block_font(),
            color,
        );
    }

    consumed_countdown
}

/// Paint an in-flight cell: faithful chunk slots when live telemetry is
/// present (3/6, alignment #4), else a pulsing fill for archive downloads
/// (with the reduced-motion fallback to a static partial fill).
fn paint_inflight(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    cell: &FrameCell,
    rect: Rect,
    anim: CellAnim,
    countdown_secs: Option<f64>,
) {
    let dark = frame.dark;
    match &cell.chunks {
        Some(chunks) if chunks.chunks_expected > 0 => {
            // Live chunk slots: N equal slots, received fill left→right, the
            // next slot shows the countdown placeholder.
            let n = chunks.chunks_expected.max(1);
            let slot_w = rect.width() / n as f32;
            let received = chunks.chunks_received.min(n);
            for slot in 0..n {
                let sx0 = rect.min.x + slot as f32 * slot_w;
                let sx1 = rect.min.x + (slot + 1) as f32 * slot_w;
                let slot_rect =
                    Rect::from_min_max(Pos2::new(sx0, rect.min.y), Pos2::new(sx1, rect.max.y));
                if slot < received {
                    painter.rect_filled(slot_rect, 1.0, tl_colors::cell_inflight(dark));
                } else if slot == received {
                    // Partial fill for the currently-accumulating chunk, plus
                    // the next-chunk countdown. `partial_radials` counts the
                    // whole in-progress sweep, so subtract the radials already
                    // accounted for by completed chunks to get this chunk's
                    // progress (otherwise the slot saturates after one chunk's
                    // worth of radials).
                    let radials_per_chunk = (360.0 / n as f32).max(1.0);
                    let in_this_chunk = (chunks.partial_radials as f32
                        - received as f32 * radials_per_chunk)
                        .max(0.0);
                    let frac = (in_this_chunk / radials_per_chunk).clamp(0.0, 1.0);
                    if frac > 0.0 {
                        let partial = Rect::from_min_max(
                            slot_rect.min,
                            Pos2::new(slot_rect.min.x + slot_rect.width() * frac, slot_rect.max.y),
                        );
                        painter.rect_filled(partial, 1.0, tl_colors::cell_inflight(dark));
                    }
                    if let Some(remaining) = countdown_secs {
                        if slot_rect.width() > 16.0 {
                            painter.text(
                                slot_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                format!("{}s", remaining.ceil() as i32),
                                style::block_font(),
                                tl_colors::cell_countdown_label(dark),
                            );
                        }
                    }
                }
            }
            stroke_dashed_rect(
                painter,
                rect,
                DashedBorder::uniform(
                    Stroke::new(1.0_f32, tl_colors::cell_inflight_border(dark)),
                    4.0,
                    8.0,
                ),
            );
        }
        _ => {
            // Archive download (no chunk telemetry): pulsing fill inside the
            // frame cell (alignment #4). Reduced motion → static partial fill.
            let base = tl_colors::cell_inflight(dark);
            if anim.reduced_motion {
                // Static partial fill at ~50% height so it reads as "working"
                // without motion.
                let h = rect.height() * 0.5;
                let partial = Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - h), rect.max);
                painter.rect_filled(partial, 1.0, base);
            } else {
                let a = (90.0 + 60.0 * anim.pulse) as u8;
                painter.rect_filled(
                    rect,
                    1.0,
                    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a),
                );
            }
            painter.rect_stroke(
                rect,
                1.0,
                Stroke::new(1.0_f32, tl_colors::cell_inflight_border(dark)),
                StrokeKind::Inside,
            );
        }
    }
}

/// Draw a small red alert triangle in the top-right of a failed cell, and
/// return its (slightly padded) hit rect.
fn failure_tick(painter: &Painter, rect: Rect) -> Rect {
    let size = (rect.height() * 0.5).clamp(6.0, 10.0);
    let pad = 1.5;
    let right = rect.right() - pad;
    let top = rect.top() + pad;
    let pts = vec![
        Pos2::new(right - size, top),
        Pos2::new(right, top),
        Pos2::new(right - size / 2.0, top + size),
    ];
    painter.add(egui::Shape::convex_polygon(
        pts,
        acq_colors::FAILED,
        Stroke::new(0.5_f32, Color32::from_rgb(120, 30, 30)),
    ));
    // "!" mark for redundancy with shape (grayscale legibility).
    if size >= 8.0 {
        painter.text(
            Pos2::new(right - size / 2.0, top + size * 0.32),
            egui::Align2::CENTER_CENTER,
            "!",
            egui::FontId::proportional(size * 0.8),
            Color32::WHITE,
        );
    }
    Rect::from_min_max(
        Pos2::new(right - size - pad, rect.top()),
        Pos2::new(rect.right(), top + size + pad),
    )
}
