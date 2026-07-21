//! Macro / Archive main-track rendering (spec §6.4 / §9).
//!
//! In the Macro tier scans collapse to **uniform-width ticks** (not
//! proportional blocks — no sub-pixel silent drops). When tick density exceeds
//! ~1 per 3px they merge into the coverage-style fill. **Gap glyphs** mark
//! where the real spacing between consecutive scans far exceeds the median, so
//! equidistant playback doesn't deceive. Shadow (server-available) regions keep
//! a hollow/dashed treatment. `fade` (0..1) scales alpha during the Micro↔Macro
//! morph so this layer cross-fades with the frames-first layer.

use super::strokes::{stroke_dashed_rect, DashedBorder};
use super::TimelineFrame;
use crate::data::ScanCompleteness;
use crate::state::DownloadProgress;
use crate::ui::colors::acquisition as acq_colors;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{Color32, Painter, Pos2, Rect, Stroke, StrokeKind};

/// Uniform tick width (px) in the Macro tier.
const TICK_W: f32 = 2.0;
/// Below this center-to-center spacing (px) ticks are too dense to read
/// individually → merge into the coverage fill.
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

    // Collect the cached scan starts in range (uniform-tick candidates).
    let scans: Vec<(f64, f64, bool)> = view
        .settled_scans_in_range(view_start, view_end)
        .map(|(s, clamped_end)| {
            let available = s.completeness == Some(ScanCompleteness::Missing);
            (s.start_time, clamped_end, available)
        })
        .collect();

    // Decide density: merge to coverage fill when ticks would crowd.
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
        // Uniform ticks, one per cached scan — never dropped sub-pixel.
        let y0 = rect.top() + 6.0;
        let y1 = rect.bottom() - 6.0;
        for &(start, _end, available) in &scans {
            let x = ts_to_x(start);
            if x < rect.left() - TICK_W || x > rect.right() + TICK_W {
                continue;
            }
            let tick = Rect::from_min_max(
                Pos2::new(x - TICK_W / 2.0, y0),
                Pos2::new(x + TICK_W / 2.0, y1),
            );
            if available {
                painter.rect_stroke(
                    tick,
                    0.0,
                    Stroke::new(1.0_f32, faded(tl_colors::cell_available_border(dark), fade)),
                    StrokeKind::Inside,
                );
            } else {
                painter.rect_filled(tick, 0.0, faded(tl_colors::cell_cached(dark), fade));
            }
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
