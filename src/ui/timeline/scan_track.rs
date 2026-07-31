//! Macro / Archive main-track rendering (spec §6.4 / §9).
//!
//! In the Macro tier each scan is a **block spanning the volume's real time**:
//! solid across the sweeps we hold, hollow across the remainder of the expected
//! volume. That one drawing answers three separate audit findings — a scan's
//! width now means its duration, a partially-downloaded volume shows how much
//! of itself is missing, and the painted extent equals the geometry the
//! range-selection logic tests against. Wide enough blocks also draw their
//! per-sweep boundaries, so a downloaded volume's intermediary elevations stop
//! vanishing at this tier.
//!
//! (It used to be a fixed 2px tick anchored at the scan start, with the end
//! discarded. A block too narrow to resolve still floors to that same 2px, so
//! nothing silently drops at the dense end.)
//!
//! When scan density exceeds ~1 per 3px the blocks merge into the coverage-style
//! fill. **Gap glyphs** mark where the real spacing between consecutive scans
//! far exceeds the median, so equidistant playback doesn't deceive. Shadow
//! (server-available) regions keep a hollow/dashed treatment. `fade` (0..1)
//! scales alpha during the Micro↔Macro morph so this layer cross-fades with the
//! frames-first layer.

use super::strokes::{stroke_dashed_rect, DashedBorder};
use super::TimelineFrame;
use crate::core::{
    block_extent, cell_gap_px, layout_block, should_draw_sweep_texture, Scan, MIN_BLOCK_W,
};
use crate::data::ScanCompleteness;
use crate::state::DownloadProgress;
use crate::ui::colors::acquisition as acq_colors;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{Color32, Painter, Pos2, Rect, Stroke};

/// Below this center-to-center spacing (px) scans are too dense to read
/// individually → merge into the coverage fill.
///
/// Because archive boundaries are contiguous, median block width equals median
/// start-to-start spacing — so this threshold doubles as the guarantee that an
/// unmerged block is at least ~3px wide.
const MERGE_SPACING_PX: f32 = 3.0;
/// A gap glyph is drawn when the real spacing between consecutive scans exceeds
/// `GAP_MEDIAN_MULT × median` (and at least `GAP_MIN_SECS`).
const GAP_MEDIAN_MULT: f64 = 2.5;
const GAP_MIN_SECS: f64 = 15.0 * 60.0;

/// Apply a morph fade (0..1) to a color's alpha.
fn faded(c: Color32, fade: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * fade) as u8)
}

/// Render the Macro / Archive main track.
pub(super) fn render_macro_track(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    progress: &DownloadProgress,
    fade: f32,
) {
    if fade <= 0.0 {
        return;
    }
    let rect = &frame.rects.scan;
    let view = &frame.view;
    let (view_start, view_end, dark) = (frame.view_start, frame.view_end, frame.dark);
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);

    // Collect the cached scans in range. The borrowed `&Scan` is what lets a
    // wide block read its own sweeps for the hairline texture — lazily, only
    // for the handful of blocks that pass the width gate, so the common case
    // costs nothing.
    let entries: Vec<(&Scan, f64, bool)> = view
        .settled_scans_in_range(view_start, view_end)
        .map(|(s, clamped_end)| {
            let available = s.completeness == Some(ScanCompleteness::Missing);
            (s, clamped_end, available)
        })
        .collect();
    let scans: Vec<(f64, f64, bool)> = entries
        .iter()
        .map(|(s, end, avail)| (s.start_time, *end, *avail))
        .collect();

    // Decide density: merge to coverage fill when blocks would crowd.
    let merge = should_merge(&scans, frame.zoom);

    if merge {
        // Coverage fill: contiguous cached ranges as one fill.
        for range in view.cache().time_ranges() {
            let x0 = ts_to_x(range.start).max(rect.left());
            let x1 = ts_to_x(range.end).min(rect.right());
            let x1 = if (x1 - x0) > 0.0 && (x1 - x0) < 8.0 {
                (x0 + 8.0).min(rect.right())
            } else {
                x1
            };
            if x1 > x0 {
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, rect.top() + 4.0),
                        Pos2::new(x1, rect.bottom() - 4.0),
                    ),
                    2.0,
                    faded(tl_colors::cached_fill(dark, false), fade),
                );
            }
        }
    } else {
        // One proportional block per cached scan.
        let y0 = rect.top() + 4.0;
        let y1 = rect.bottom() - 4.0;
        for &(scan, clamped_end, available) in &entries {
            draw_scan_block(painter, frame, scan, clamped_end, available, y0, y1, fade);
        }

        // Gap glyphs between consecutive scans whose real spacing is anomalous.
        draw_gap_glyphs(painter, frame, &scans, fade);
    }

    // Shadow (server-available) regions: hollow/dashed, merged like before.
    render_shadow_regions(painter, frame, fade);

    // In-flight acquisition at this tier: a faint combined region so the user
    // still sees something is downloading. Queued/per-cell detail is the Micro
    // tier's job (zoom in to see the hatch + chunk segmentation).
    render_macro_acquisition(painter, frame, progress, fade);
}

/// Paint one scan as a block spanning its real time extent.
///
/// Solid across the sweeps we actually hold, hollow/dashed across the rest of
/// the expected volume (reusing the Available grammar), with the volume's
/// per-sweep boundaries drawn inside once there is room. The right edge is
/// pulled in by [`cell_gap_px`] because archive boundaries are contiguous —
/// without that, a run of volumes paints as one solid bar and the scan cadence
/// is lost.
#[allow(clippy::too_many_arguments)]
fn draw_scan_block(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    scan: &Scan,
    clamped_end: f64,
    available: bool,
    y0: f32,
    y1: f32,
    fade: f32,
) {
    let rect = &frame.rects.scan;
    let dark = frame.dark;

    let extent = block_extent(
        scan.start_time,
        scan.end_time,
        scan.vcp_pattern
            .as_ref()
            .and_then(|v| v.estimated_volume_duration()),
        clamped_end,
        crate::FALLBACK_SCAN_DURATION_SECS as f64,
    );
    let Some(layout) = layout_block(&extent, |ts| frame.ts_to_x(ts), rect.left(), rect.right())
    else {
        return;
    };

    let gap = cell_gap_px(layout.width());
    let x0 = layout.x0.max(rect.left());
    let x1 = (layout.x1 - gap).min(rect.right());
    if x1 <= x0 {
        return;
    }
    let block = Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1));

    // Not downloaded at all — hollow, no interior structure to show.
    if available {
        painter.rect_filled(
            block,
            1.0,
            faded(tl_colors::cell_available_fill(dark), fade),
        );
        stroke_dashed_rect(
            painter,
            block,
            DashedBorder::uniform(
                Stroke::new(1.0_f32, faded(tl_colors::cell_available_border(dark), fade)),
                4.0,
                7.0,
            ),
        );
        return;
    }

    // Solid across what we hold. Floored to MIN_BLOCK_W so a scan whose data
    // span is sub-pixel still reads as present rather than as an empty outline.
    let data_x1 = layout.data_x1.clamp(x0, x1).max(x0 + MIN_BLOCK_W).min(x1);
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(data_x1, y1)),
        1.0,
        faded(tl_colors::cell_cached(dark), fade),
    );

    // Hollow tail across the part of the volume we don't have. This is the
    // "the range only represents the downloaded data" fix: the block's full
    // width is the volume, the solid part is what landed.
    if extent.is_partial() && x1 - data_x1 > 1.0 {
        let tail = Rect::from_min_max(Pos2::new(data_x1, y0), Pos2::new(x1, y1));
        painter.rect_filled(tail, 1.0, faded(tl_colors::cell_available_fill(dark), fade));
        stroke_dashed_rect(
            painter,
            tail,
            DashedBorder::uniform(
                Stroke::new(1.0_f32, faded(tl_colors::cell_available_border(dark), fade)),
                3.0,
                6.0,
            ),
        );
    }

    // Per-sweep boundaries inside the solid part — the intermediary elevations
    // that used to have nowhere to appear at this tier. Same grammar as the
    // Micro tier's sub-texture.
    let solid_w = data_x1 - x0;
    if should_draw_sweep_texture(solid_w, scan.sweeps.len()) {
        let tex = faded(tl_colors::sub_texture(dark), fade);
        for sweep in &scan.sweeps {
            let x = frame.ts_to_x(sweep.start_time);
            if x > x0 + 0.5 && x < data_x1 - 0.5 {
                painter.line_segment(
                    [Pos2::new(x, y0 + 1.0), Pos2::new(x, y1 - 1.0)],
                    Stroke::new(0.5_f32, tex),
                );
            }
        }
    }
}

/// Whether the visible cached scans are dense enough to merge into coverage.
fn should_merge(scans: &[(f64, f64, bool)], zoom: f64) -> bool {
    if scans.len() < 2 {
        return false;
    }
    // Median center-to-center pixel spacing.
    let mut deltas: Vec<f64> = scans
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).abs())
        .filter(|d| *d > 0.0)
        .collect();
    if deltas.is_empty() {
        return false;
    }
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = deltas[deltas.len() / 2];
    let spacing_px = (median * zoom) as f32;
    spacing_px < MERGE_SPACING_PX
}

/// Draw a small neutral break glyph between two consecutive ticks whose true
/// spacing exceeds the gap threshold.
fn draw_gap_glyphs(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    scans: &[(f64, f64, bool)],
    fade: f32,
) {
    if scans.len() < 2 {
        return;
    }
    let mut deltas: Vec<f64> = scans
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .filter(|d| *d > 0.0)
        .collect();
    if deltas.is_empty() {
        return;
    }
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = deltas[deltas.len() / 2];
    let threshold = (median * GAP_MEDIAN_MULT).max(GAP_MIN_SECS);
    let rect = &frame.rects.scan;
    let cy = rect.center().y;
    let color = faded(tl_colors::sub_texture(frame.dark), fade);
    let glyph =
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (color.a()).max(90));

    for w in scans.windows(2) {
        let gap = w[1].0 - w[0].0;
        if gap <= threshold {
            continue;
        }
        // Midpoint between the two scans.
        let mid_ts = (w[0].0 + w[1].0) / 2.0;
        let x = frame.ts_to_x(mid_ts);
        if x < rect.left() || x > rect.right() {
            continue;
        }
        // A small "//" break: two short slashes.
        for dx in [-2.0_f32, 2.0] {
            painter.line_segment(
                [
                    Pos2::new(x + dx - 1.5, cy + 4.0),
                    Pos2::new(x + dx + 1.5, cy - 4.0),
                ],
                Stroke::new(1.0_f32, glyph),
            );
        }
    }
}

/// Shadow (in-archive, not downloaded) regions: merged contiguous hollow
/// blocks, suppressed where cached data already covers them.
fn render_shadow_regions(painter: &Painter, frame: &TimelineFrame<'_>, fade: f32) {
    let rect = &frame.rects.scan;
    let view = &frame.view;
    let dark = frame.dark;
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);
    let view_start_i64 = frame.view_start as i64;
    let view_end_i64 = frame.view_end as i64;

    let visible: Vec<_> = view
        .shadow_boundaries()
        .iter()
        .filter(|b| !view.is_covered_by_cached(b.start))
        .filter(|b| b.end > view_start_i64 && b.start < view_end_i64)
        .collect();
    if visible.is_empty() {
        return;
    }

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
        let x0 = ts_to_x(start as f64).max(rect.left());
        let x1 = ts_to_x(end as f64).min(rect.right());
        let x1 = if (x1 - x0) > 0.0 && (x1 - x0) < 8.0 {
            (x0 + 8.0).min(rect.right())
        } else {
            x1
        };
        if x1 > x0 {
            let block = Rect::from_min_max(
                Pos2::new(x0, rect.top() + 4.0),
                Pos2::new(x1, rect.bottom() - 4.0),
            );
            painter.rect_filled(
                block,
                2.0,
                faded(tl_colors::cell_available_fill(dark), fade),
            );
            stroke_dashed_rect(
                painter,
                block,
                DashedBorder::uniform(
                    Stroke::new(1.0_f32, faded(tl_colors::cell_available_border(dark), fade)),
                    4.0,
                    7.0,
                ),
            );
        }
    }
}

/// A faint combined in-flight region at the Macro tier (the per-cell detail is
/// the Micro tier's; here we just acknowledge activity, motion not hue).
fn render_macro_acquisition(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    progress: &DownloadProgress,
    fade: f32,
) {
    let rect = &frame.rects.scan;
    let ts_to_x = |ts: f64| frame.ts_to_x(ts);
    let all: Vec<(i64, i64)> = progress
        .active_scans
        .iter()
        .chain(progress.in_flight_scans.iter())
        .copied()
        .collect();
    if all.is_empty() {
        return;
    }
    let min_ts = all.iter().map(|(s, _)| *s).min().unwrap() as f64;
    let max_ts = all.iter().map(|(_, e)| *e).max().unwrap() as f64;
    let x0 = ts_to_x(min_ts).max(rect.left());
    let x1 = ts_to_x(max_ts).min(rect.right());
    if x1 > x0 {
        let base = tl_colors::cell_inflight(frame.dark);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, rect.top() + 4.0),
                Pos2::new(x1, rect.bottom() - 4.0),
            ),
            2.0,
            faded(
                Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 70),
                fade,
            ),
        );
    }
    let _ = acq_colors::ACTIVE; // palette intentionally neutral now.
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---- faded() ---------------------------------------------------------

    #[wasm_bindgen_test]
    fn faded_full_opacity_is_identity_on_opaque_color() {
        // fade == 1.0 leaves a fully opaque color untouched.
        let c = Color32::from_rgb(100, 150, 200); // stored [100,150,200,255]
        let out = faded(c, 1.0);
        assert!(out.r() == 100);
        assert!(out.g() == 150);
        assert!(out.b() == 200);
        assert!(out.a() == 255);
    }

    #[wasm_bindgen_test]
    fn faded_zero_fade_is_fully_transparent() {
        // fade == 0.0 drives alpha to 0 → Color32::TRANSPARENT (all zeros).
        let c = Color32::from_rgb(100, 150, 200);
        let out = faded(c, 0.0);
        assert!(out.r() == 0);
        assert!(out.g() == 0);
        assert!(out.b() == 0);
        assert!(out.a() == 0);
    }

    #[wasm_bindgen_test]
    fn faded_scales_alpha_by_fade_factor() {
        // Input alpha 200, fade 0.5 → (200 * 0.5) = 100 (truncated to u8).
        // Use premultiplied ctor so the stored alpha byte is exactly 200.
        let c = Color32::from_rgba_premultiplied(10, 20, 30, 200);
        let out = faded(c, 0.5);
        assert!(out.a() == 100);
    }

    #[wasm_bindgen_test]
    fn faded_truncates_toward_zero() {
        // 255 * 0.5 = 127.5 → truncates to 127 (cast, not round).
        let c = Color32::from_rgb(1, 2, 3); // alpha 255
        let out = faded(c, 0.5);
        assert!(out.a() == 127);
    }

    // ---- should_merge() --------------------------------------------------

    #[wasm_bindgen_test]
    fn should_merge_false_when_fewer_than_two_scans() {
        let empty: Vec<(f64, f64, bool)> = Vec::new();
        assert!(!should_merge(&empty, 1.0));
        let one = vec![(0.0_f64, 0.0_f64, false)];
        assert!(!should_merge(&one, 1.0));
    }

    #[wasm_bindgen_test]
    fn should_merge_false_when_all_deltas_zero() {
        // Duplicate start times → all deltas filtered out → not mergeable.
        let scans = vec![(5.0, 5.0, false), (5.0, 5.0, false)];
        assert!(!should_merge(&scans, 100.0));
    }

    #[wasm_bindgen_test]
    fn should_merge_true_when_dense() {
        // Two scans 100s apart; zoom 0.01 → spacing_px = 1.0 < 3.0 → merge.
        let scans = vec![(0.0, 0.0, false), (100.0, 0.0, false)];
        assert!(should_merge(&scans, 0.01));
    }

    #[wasm_bindgen_test]
    fn should_merge_false_when_sparse() {
        // Same scans; zoom 0.05 → spacing_px = 5.0 (not < 3.0) → no merge.
        let scans = vec![(0.0, 0.0, false), (100.0, 0.0, false)];
        assert!(!should_merge(&scans, 0.05));
    }

    #[wasm_bindgen_test]
    fn should_merge_uses_upper_median_for_odd_delta_count() {
        // Starts [0,100,250] → deltas [100,150]; median index 2/2=1 → 150.
        // zoom 0.01 → spacing_px = 1.5 < 3.0 → merge.
        let scans = vec![(0.0, 0.0, false), (100.0, 0.0, false), (250.0, 0.0, false)];
        assert!(should_merge(&scans, 0.01));
        // zoom 0.025 → spacing_px = 3.75 (not < 3.0) → no merge.
        assert!(!should_merge(&scans, 0.025));
    }

    #[wasm_bindgen_test]
    fn should_merge_at_threshold_boundary_is_not_merged() {
        // spacing_px exactly 3.0 is NOT < 3.0 → no merge.
        // delta 100, zoom 0.03 → 3.0.
        let scans = vec![(0.0, 0.0, false), (100.0, 0.0, false)];
        assert!(!should_merge(&scans, 0.03));
    }
}
