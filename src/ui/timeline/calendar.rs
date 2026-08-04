//! Archive-tier calendar coverage heatmap (spec §6.4 Archive).
//!
//! When the timeline is zoomed out past the Archive-enter span the linear strip
//! is replaced by this **calendar-style coverage heatmap** (GitHub-contributions
//! grammar). The strip never renders stretched across multi-day spans — the
//! "year-wide strip zoom" the spec deprecates (§15 cut #4) is gone because the
//! linear renderer is simply never the active one at Archive spans; the calendar
//! is.
//!
//! Layout choice (height budget): a **single horizontal lane of cells** inside
//! the same main-track rect the linear strip uses, so the panel height stays
//! exactly `TIMELINE_TOTAL_H` (no reflow, honoring the constant-height
//! contract). Cells map to x through the frame's shared `ts_to_x` — both edges,
//! since buckets are variable width once months are in play — so the
//! playhead/now overlays the orchestrator paints afterward stay spatially
//! consistent with the same zoom scalar.
//!
//! **Cell size follows the zoom ladder** (day → week → month → quarter), which
//! is what lets the lane span the whole NEXRAD era while staying legible. The
//! label ladder follows it: month names carry the structure while cells are days
//! or weeks, but once a cell IS a month a month label on every cell is noise, so
//! years take over. A collision guard thins labels further rather than
//! hand-tuning an interval per zoom. The calendar owns the tick lane at this
//! tier — the linear tick configs are skipped entirely.
//!
//! Tone (two visual dimensions per the spec, within the accent budget — no new
//! hues, red still reserved for live/failure):
//! - **availability** (server/listing coverage) → a *lighter wash* whose alpha
//!   scales with the day's `availability_frac`.
//! - **cache** (downloaded locally) → a *solid intensity* inner fill whose alpha
//!   scales with `cache_frac`.
//!
//! Both use the neutral steel cell tones. A saved-event bucket gets a small
//! neutral bookmark tick (shape, not color). Reduced-motion has nothing to
//! animate here (the calendar is static) — the morph into Archive is an instant
//! swap.
//!
//! At the widest zooms most buckets are structurally empty (the listing pump is
//! capped at a 4-day span, so nothing new ever loads out there). Three measures
//! keep that reading as *bounded* rather than *broken*: a keyline washing the
//! addressable `[era start, now]` range behind the cells, a 1px floor on the
//! cache bar so a small-but-real fraction still shows, and gap/border collapsing
//! on narrow cells so the coverage tones aren't crowded out by chrome.

use super::TimelineFrame;
use crate::core::BucketGranularity;
use crate::state::TimeBucket;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind};

/// Horizontal gap (px) between adjacent day cells, so the lane reads as a row of
/// discrete cells rather than one continuous bar.
const CELL_GAP_PX: f32 = 1.0;
/// Vertical inset of the day-cell lane inside the main track.
const LANE_INSET_Y: f32 = 5.0;
/// Minimum on-screen width (px) for a cell to draw its inner cache fill /
/// bookmark glyph; below this the cell is a thin tick and only the wash shows.
const MIN_DETAIL_W: f32 = 4.0;
/// Minimum horizontal spacing (px) between period labels in the tick lane. At
/// the widest zooms year separators land only ~33px apart, so this thins them
/// to every other year rather than letting them overlap into soup.
const LABEL_MIN_GAP_PX: f32 = 40.0;
/// Minimum painted height (px) of the cache bar whenever any data is cached.
/// A 1%-cached week is 0.3px of a 32px lane and would vanish entirely — and at
/// coarse rungs almost every real bucket is a small fraction, so without this
/// floor the wide view looks empty even where data exists.
const MIN_CACHE_BAR_H: f32 = 1.0;

/// One hit-tested day cell, returned so the interaction layer can tap-to-zoom
/// and the tooltip layer can describe the hovered day.
#[derive(Clone, Copy)]
pub(super) struct DayCellHit {
    /// Screen rect of the cell (for hit-test + hover).
    pub rect: Rect,
    /// The bucket this cell renders (UTC day start + tones + events flag).
    pub bucket: TimeBucket,
}

/// Render the Archive calendar heatmap into the main-track rect and return the
/// per-cell hit list (for tap-to-zoom + hover tooltips). The buckets are the
/// pure aggregation already computed for the visible span; this function only
/// paints + hit-tests. Month labels are drawn in the tick lane.
pub(super) fn render_calendar(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    buckets: &[TimeBucket],
    granularity: BucketGranularity,
) -> Vec<DayCellHit> {
    let track = &frame.rects.scan;
    let lane_top = track.top() + LANE_INSET_Y;
    let lane_bot = track.bottom() - LANE_INSET_Y;
    let dark = frame.dark;

    // The addressable range, drawn under the cells. At the widest zooms most
    // buckets are structurally empty (nothing new loads out there — the listing
    // pump is capped at a 4-day span), so without a keyline the lane reads as
    // broken rather than as bounded.
    paint_era_keyline(painter, frame, dark);

    let mut hits: Vec<DayCellHit> = Vec::with_capacity(buckets.len());

    // Label collision guard: the last x a label was emitted at. Beats
    // hand-tuning a label interval per zoom — at 36 years the year separators
    // land ~33px apart and this drops to every other year on its own.
    let mut last_label_x = f32::NEG_INFINITY;
    let mut prev_period: Option<(i32, u32)> = None;

    for bucket in buckets {
        // Both edges through the shared mapping: buckets are variable width
        // once months are in play, so the end can't be derived from the start.
        let x0 = frame.ts_to_x(bucket.start);
        let x1 = frame.ts_to_x(bucket.end);
        // Fully off-screen cells are skipped (no hit, no paint).
        if x1 < track.left() || x0 > track.right() {
            continue;
        }
        // Gap and border both collapse on narrow cells — at ~3px a bordered,
        // gapped cell is almost entirely border and the coverage tones vanish.
        let raw_w = x1 - x0;
        let gap = if raw_w >= MIN_DETAIL_W * 2.0 {
            CELL_GAP_PX
        } else {
            0.0
        };
        let cell_left = (x0 + gap).max(track.left());
        let cell_right = (x1 - gap).min(track.right());
        if cell_right <= cell_left {
            continue;
        }
        let cell_rect = Rect::from_min_max(
            Pos2::new(cell_left, lane_top),
            Pos2::new(cell_right, lane_bot),
        );

        paint_day_cell(painter, cell_rect, bucket, dark, raw_w);

        // Period separator + label. Which period counts depends on the rung:
        // month boundaries are meaningful at Day/Week, but at Month/Quarter
        // they land on every cell, so years carry the structure instead.
        let comps = super::DateTimeComponents::from_timestamp(bucket.start as i64, false);
        let period = match granularity {
            BucketGranularity::Day | BucketGranularity::Week => (comps.year, comps.month),
            BucketGranularity::Month | BucketGranularity::Quarter => (comps.year, 1),
        };
        if prev_period != Some(period) {
            let far_enough = x0 - last_label_x >= LABEL_MIN_GAP_PX;
            if paint_period_marker(painter, frame, x0, &comps, granularity, far_enough, dark) {
                last_label_x = x0;
            }
            prev_period = Some(period);
        }

        hits.push(DayCellHit {
            rect: cell_rect,
            bucket: *bucket,
        });
    }

    hits
}

/// Wash the addressable archive range `[era start, now]` behind the cells.
///
/// Zoomed fully out, the great majority of buckets have no data and nothing
/// will ever load there. A lane of uniformly empty cells reads as a failure; the
/// same lane with a visible "this is the range that exists" band reads as
/// bounded, which is the truth.
fn paint_era_keyline(painter: &Painter, frame: &TimelineFrame<'_>, dark: bool) {
    let track = &frame.rects.scan;
    let x0 = frame
        .ts_to_x(crate::core::NEXRAD_ARCHIVE_START_SECS)
        .max(track.left());
    let x1 = frame.ts_to_x(frame.now_secs).min(track.right());
    if x1 <= x0 {
        return;
    }
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(x0, track.top() + LANE_INSET_Y - 2.0),
            Pos2::new(x1, track.bottom() - LANE_INSET_Y + 2.0),
        ),
        1.0,
        tl_colors::sub_texture(dark),
    );
}

/// Paint one day cell: a lighter availability wash, a solid cache-intensity
/// inner fill, a faint border, and a bookmark glyph when the day has events.
fn paint_day_cell(painter: &Painter, rect: Rect, bucket: &TimeBucket, dark: bool, raw_w: f32) {
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
        // Floored so a small-but-real fraction is visible. Coarse rungs make
        // almost every genuine bucket a small fraction of its span, so without
        // the floor the wide view reads as empty where it isn't.
        let fill_h = (rect.height() * frac)
            .max(MIN_CACHE_BAR_H)
            .min(rect.height());
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

    // Faint cell border so empty (unknown) cells still read as cells in the
    // grid — but only while the cell is wide enough to have an interior. On a
    // ~3px cell a 1px inset border on both sides IS the cell, which hides the
    // coverage tones the lane exists to show.
    if raw_w >= MIN_DETAIL_W {
        painter.rect_stroke(
            rect,
            1.0,
            Stroke::new(1.0_f32, tl_colors::container_border(dark)),
            StrokeKind::Inside,
        );
    }

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

/// Draw a period separator through the lane and its label in the tick lane.
///
/// The label ladder follows the rung: month names carry the structure while
/// cells are days or weeks, but once a cell *is* a month or a quarter a month
/// label on every cell is noise, so years take over. The calendar owns its own
/// labels — the linear tick ladder no longer carries month/year configs, and is
/// skipped entirely at this tier.
///
/// Returns whether a label was actually emitted (the separator is always drawn;
/// the label is subject to the collision guard).
fn paint_period_marker(
    painter: &Painter,
    frame: &TimelineFrame<'_>,
    x: f32,
    comps: &super::DateTimeComponents,
    granularity: BucketGranularity,
    label_allowed: bool,
    dark: bool,
) -> bool {
    let track = &frame.rects.scan;
    let tick = &frame.rects.tick;
    if x < track.left() || x > track.right() {
        return false;
    }
    // Separator through the cell lane.
    painter.line_segment(
        [
            Pos2::new(x, track.top() + 1.0),
            Pos2::new(x, track.bottom() - 1.0),
        ],
        Stroke::new(1.0_f32, tl_colors::tick_major(dark)),
    );
    if !label_allowed {
        return false;
    }
    let label = match granularity {
        BucketGranularity::Day | BucketGranularity::Week => {
            format!("{} {}", comps.month_abbrev(), comps.year)
        }
        BucketGranularity::Month | BucketGranularity::Quarter => comps.year.to_string(),
    };
    painter.text(
        Pos2::new((x + 3.0).min(track.right() - 2.0), tick.center().y),
        egui::Align2::LEFT_CENTER,
        &label,
        FontId::monospace(9.0),
        tl_colors::tick_label(dark),
    );
    true
}

/// Human-readable coverage duration for a calendar tooltip.
///
/// Deliberately absolute rather than a percentage: at a quarter rung six hours
/// of data is 0.3% and rounds to "0%", which tells the user there is nothing
/// there when there is.
pub(super) fn format_coverage(secs: f64) -> String {
    if secs <= 0.0 {
        return "none".to_string();
    }
    let hours = secs / 3600.0;
    if hours < 1.0 {
        format!("{:.0} min", (secs / 60.0).max(1.0))
    } else if hours < 48.0 {
        format!("{hours:.1} h")
    } else {
        format!("{:.1} days", hours / 24.0)
    }
}

/// Render the hover tooltip for a calendar cell: the period plus a coverage
/// summary. Reuses the always-open tooltip idiom the rest of the timeline uses.
/// `hover_pos` anchors the popup.
pub(super) fn render_day_tooltip(
    ui: &mut egui::Ui,
    bucket: &TimeBucket,
    hover_pos: Pos2,
    use_local: bool,
    granularity: BucketGranularity,
) {
    use eframe::egui::{RichText, Vec2};
    let comps = super::DateTimeComponents::from_timestamp(bucket.start as i64, use_local);
    // The heading names what the cell actually is, so a quarter cell doesn't
    // claim to be its first day.
    let date = match granularity {
        BucketGranularity::Day => {
            format!("{} {:02}, {}", comps.month_abbrev(), comps.day, comps.year)
        }
        BucketGranularity::Week => format!(
            "Week of {} {:02}, {}",
            comps.month_abbrev(),
            comps.day,
            comps.year
        ),
        BucketGranularity::Month => format!("{} {}", comps.month_abbrev(), comps.year),
        BucketGranularity::Quarter => {
            format!("Q{} {}", (comps.month - 1) / 3 + 1, comps.year)
        }
    };
    let period = match granularity {
        BucketGranularity::Day => "this day",
        BucketGranularity::Week => "this week",
        BucketGranularity::Month => "this month",
        BucketGranularity::Quarter => "this quarter",
    };

    egui::Tooltip::always_open(
        ui.ctx().clone(),
        egui::LayerId::new(egui::Order::Tooltip, ui.id()),
        ui.id().with("cal_tooltip"),
        Rect::from_center_size(hover_pos, Vec2::splat(20.0)),
    )
    .show(|ui: &mut egui::Ui| {
        ui.label(RichText::new(date).strong().size(12.0));
        if bucket.available_secs <= 0.0 {
            // "No data" would imply the archive is empty here; the truth is
            // that no listing has been loaded for this span — the listing pump
            // is capped at a 4-day window, so wide views never populate.
            ui.label(
                RichText::new(format!("No listing loaded for {period} — zoom in"))
                    .size(11.0)
                    .weak(),
            );
        } else {
            // Absolutes, not percentages: at coarse rungs a real few hours of
            // data rounds to "0%", which reads as nothing at all.
            ui.label(
                RichText::new(format!(
                    "{} cached of {} available",
                    format_coverage(bucket.cached_secs),
                    format_coverage(bucket.available_secs)
                ))
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
