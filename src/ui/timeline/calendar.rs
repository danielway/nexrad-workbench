//! Archive-tier calendar coverage heatmap (spec §6.4 Archive).
//!
//! When the timeline is zoomed out past the Archive-enter span the linear strip
//! is replaced by this **calendar-style coverage heatmap** (GitHub-contributions
//! grammar). The strip never renders stretched across multi-day spans — the
//! "year-wide strip zoom" the spec deprecates (§15 cut #4) is gone because the
//! linear renderer is simply never the active one at Archive spans; the calendar
//! is.
//!
//! Layout choice (height budget): a **single horizontal lane of UTC-day cells**
//! inside the same main-track rect the linear strip uses, so the panel height
//! stays exactly `TIMELINE_TOTAL_H` (no reflow, honoring the constant-height
//! contract). Month boundaries get a faint separator + a month label in the tick
//! lane (the calendar supplies its own labels; the linear tick configs only go
//! down to days now). Day cells map to x through the frame's shared `ts_to_x`,
//! so the playhead/now overlays the orchestrator paints afterward stay spatially
//! consistent with the same zoom scalar.
//!
//! Tone (two visual dimensions per the spec, within the accent budget — no new
//! hues, red still reserved for live/failure):
//! - **availability** (server/listing coverage) → a *lighter wash* whose alpha
//!   scales with the day's `availability_frac`.
//! - **cache** (downloaded locally) → a *solid intensity* inner fill whose alpha
//!   scales with `cache_frac`.
//!
//! Both use the neutral steel cell tones. A saved-event day gets a small neutral
//! bookmark tick (shape, not color). Reduced-motion has nothing to animate here
//! (the calendar is static) — the morph into Archive is an instant swap.

use super::TimelineFrame;
use crate::state::DayBucket;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind};

/// Horizontal gap (px) between adjacent day cells, so the lane reads as a row of
/// discrete cells rather than one continuous bar.
const CELL_GAP_PX: f32 = 1.0;
/// Vertical inset of the day-cell lane inside the main track.
const LANE_INSET_Y: f32 = 5.0;
/// Minimum on-screen width (px) for a day cell to draw its inner cache fill /
/// bookmark glyph; below this the cell is a thin tick and only the wash shows.
const MIN_DETAIL_W: f32 = 4.0;

/// One hit-tested day cell, returned so the interaction layer can tap-to-zoom
/// and the tooltip layer can describe the hovered day.
#[derive(Clone, Copy)]
pub(super) struct DayCellHit {
    /// Screen rect of the cell (for hit-test + hover).
    pub rect: Rect,
    /// The bucket this cell renders (UTC day start + tones + events flag).
    pub bucket: DayBucket,
}

/// Render the Archive calendar heatmap into the main-track rect and return the
/// per-cell hit list (for tap-to-zoom + hover tooltips). The buckets are the
/// pure aggregation already computed for the visible span; this function only
/// paints + hit-tests. Month labels are drawn in the tick lane.
pub(super) fn render_calendar(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    buckets: &[DayBucket],
) -> Vec<DayCellHit> {
    let track = &frame.rects.scan;
    let lane_top = track.top() + LANE_INSET_Y;
    let lane_bot = track.bottom() - LANE_INSET_Y;
    let dark = frame.dark;

    let day_secs = crate::state::DAY_SECS;
    let mut hits: Vec<DayCellHit> = Vec::with_capacity(buckets.len());

    let mut prev_month: Option<u32> = None;
    for bucket in buckets {
        let day_start = bucket.day_start;
        let day_end = day_start + day_secs;
        // Cell x-extent from the shared mapping, clamped to the track and inset
        // by the gap so cells read discretely.
        let x0 = frame.ts_to_x(day_start);
        let x1 = frame.ts_to_x(day_end);
        // Fully off-screen cells are skipped (no hit, no paint).
        if x1 < track.left() || x0 > track.right() {
            continue;
        }
        let cell_left = (x0 + CELL_GAP_PX).max(track.left());
        let cell_right = (x1 - CELL_GAP_PX).min(track.right());
        if cell_right <= cell_left {
            continue;
        }
        let cell_rect = Rect::from_min_max(
            Pos2::new(cell_left, lane_top),
            Pos2::new(cell_right, lane_bot),
        );

        paint_day_cell(painter, cell_rect, bucket, dark);

        // Month separator + label when this day starts a new month.
        let comps = super::DateTimeComponents::from_timestamp(day_start as i64, false);
        if prev_month != Some(comps.month) {
            paint_month_marker(painter, frame, x0, &comps, dark);
            prev_month = Some(comps.month);
        }

        hits.push(DayCellHit {
            rect: cell_rect,
            bucket: *bucket,
        });
    }

    hits
}

/// Paint one day cell: a lighter availability wash, a solid cache-intensity
/// inner fill, a faint border, and a bookmark glyph when the day has events.
fn paint_day_cell(painter: &Painter, rect: Rect, bucket: &DayBucket, dark: bool) {
    let w = rect.width();
    // Availability wash: lighter, alpha scaled by the fraction of the day that is
    // known to exist on the server. Even a fully-available day stays a wash so it
    // never competes with the solid cache fill.
    if bucket.availability_frac > 0.0 {
        let a = (40.0 + bucket.availability_frac * 70.0) as u8;
        let base = tl_colors::cell_available_fill(dark);
        painter.rect_filled(
            rect,
            1.0,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a),
        );
    }

    // Cache intensity: a solid inner fill whose height (and alpha) grows with the
    // cached fraction. A vertical "fill bar" from the bottom up reads the cache
    // quantity directly in grayscale (shape + intensity), not by hue.
    if bucket.cache_frac > 0.0 && w >= MIN_DETAIL_W {
        let frac = bucket.cache_frac.clamp(0.0, 1.0);
        let fill_h = rect.height() * frac;
        let inner = Rect::from_min_max(
            Pos2::new(rect.left(), rect.bottom() - fill_h),
            Pos2::new(rect.right(), rect.bottom()),
        );
        let a = (120.0 + frac * 110.0) as u8;
        let base = tl_colors::cell_cached(dark);
        painter.rect_filled(
            inner,
            1.0,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a),
        );
    } else if bucket.cache_frac > 0.0 {
        // Too narrow for a bar — fill the whole thin cell at cache intensity.
        let a = (120.0 + bucket.cache_frac.clamp(0.0, 1.0) * 110.0) as u8;
        let base = tl_colors::cell_cached(dark);
        painter.rect_filled(
            rect,
            1.0,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a),
        );
    }

    // Faint cell border so empty (unknown) days still read as cells in the grid.
    painter.rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0_f32, tl_colors::container_border(dark)),
        StrokeKind::Inside,
    );

    // Saved-event bookmark: a small neutral triangle at the top-left (shape, not
    // an accent hue — matches the linear strip's event marker grammar).
    if bucket.has_events && w >= MIN_DETAIL_W {
        let tip_x = rect.left();
        let pts = vec![
            Pos2::new(tip_x, rect.top()),
            Pos2::new(tip_x + 5.0, rect.top()),
            Pos2::new(tip_x + 2.5, rect.top() + 4.0),
        ];
        painter.add(egui::Shape::convex_polygon(
            pts,
            tl_colors::event_border(),
            Stroke::NONE,
        ));
    }
}

/// Draw a month-start separator line through the lane and a `Mon YYYY` label in
/// the tick lane above it. The calendar owns its own labels (the linear tick
/// ladder no longer carries month/year configs).
fn paint_month_marker(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    x: f32,
    comps: &super::DateTimeComponents,
    dark: bool,
) {
    let track = &frame.rects.scan;
    let tick = &frame.rects.tick;
    if x < track.left() || x > track.right() {
        return;
    }
    // Separator through the day lane.
    painter.line_segment(
        [
            Pos2::new(x, track.top() + 1.0),
            Pos2::new(x, track.bottom() - 1.0),
        ],
        Stroke::new(1.0_f32, tl_colors::tick_major(dark)),
    );
    // Month label in the tick lane.
    let label = format!("{} {}", comps.month_abbrev(), comps.year);
    painter.text(
        Pos2::new((x + 3.0).min(track.right() - 2.0), tick.center().y),
        egui::Align2::LEFT_CENTER,
        &label,
        FontId::monospace(9.0),
        tl_colors::tick_label(dark),
    );
}

/// Render the hover tooltip for a day cell: the date plus a "N cached /
/// available" coverage summary. Reuses the always-open tooltip idiom the rest of
/// the timeline uses. `hover_pos` anchors the popup.
pub(super) fn render_day_tooltip(
    ui: &mut egui::Ui,
    bucket: &DayBucket,
    hover_pos: Pos2,
    use_local: bool,
) {
    use eframe::egui::{RichText, Vec2};
    let comps = super::DateTimeComponents::from_timestamp(bucket.day_start as i64, use_local);
    let date = format!("{} {:02}, {}", comps.month_abbrev(), comps.day, comps.year);
    // Fractions → percent for a legible "how much of the day" readout.
    let avail_pct = (bucket.availability_frac * 100.0).round() as i32;
    let cache_pct = (bucket.cache_frac * 100.0).round() as i32;

    egui::Tooltip::always_open(
        ui.ctx().clone(),
        egui::LayerId::new(egui::Order::Tooltip, ui.id()),
        ui.id().with("cal_tooltip"),
        Rect::from_center_size(hover_pos, Vec2::splat(20.0)),
    )
    .show(|ui: &mut egui::Ui| {
        ui.label(RichText::new(date).strong().size(12.0));
        if avail_pct == 0 {
            ui.label(
                RichText::new("No data listed for this day")
                    .size(11.0)
                    .weak(),
            );
        } else {
            ui.label(
                RichText::new(format!("{cache_pct}% cached / {avail_pct}% available"))
                    .size(11.0)
                    .color(tl_colors::status_cached()),
            );
        }
        if bucket.has_events {
            ui.label(
                RichText::new("Saved event on this day")
                    .size(11.0)
                    .color(tl_colors::event_label()),
            );
        }
        ui.label(
            RichText::new("Tap to open this day")
                .size(10.0)
                .weak()
                .italics(),
        );
    });
}
