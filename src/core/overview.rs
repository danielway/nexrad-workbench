//! The overview lane above the timeline (round-4 design): a sliding window,
//! 48 hours wide by default, with a box marking what the main strip shows.
//! It zooms from a few hours out to the whole archive era; past single scans
//! it draws coverage in day, week, month or quarter bars
//! ([`OverviewDetail`]).
//!
//! The overview is a map of the view, never a control of playback. Dragging
//! the box moves the view, dragging its sides zooms the view, dragging the
//! rest of the lane slides the overview itself, scrolling zooms it, and a
//! click looks there (or, zoomed out to bars, goes to that day). While its
//! right edge is on now it slides forward with the clock. There are no edge
//! traps: the overview can slide anywhere in the archive era, and the box
//! can leave it (an edge chip brings it back).
//!
//! Pure decisions only. The egui shell (`ui::timeline::overview`) paints the
//! lane and turns pointer facts into calls on [`Overview`] and on
//! [`PlaybackState::show_view`].
//!
//! [`PlaybackState::show_view`]: crate::core::PlaybackState::show_view

/// How much time the overview shows unless the view needs more.
pub(crate) const OVERVIEW_DEFAULT_SPAN_SECS: f64 = 48.0 * 3600.0;

use crate::core::BucketGranularity;

/// The narrowest the overview zooms in to.
pub(crate) const MIN_OVERVIEW_SPAN_SECS: f64 = 3.0 * 3600.0;

/// Zooming the overview in stops when the view box fills this much of it.
const MAX_VIEW_FRAC: f64 = 0.8;

/// When the view grows past this fraction of the overview (the strip zoomed
/// out), the overview widens so the box fills [`WIDEN_TO_FRAC`] of it.
const WIDEN_AT_FRAC: f64 = 0.9;
const WIDEN_TO_FRAC: f64 = 0.6;

/// Below this many px per bar the overview steps to the next coarser bar.
const MIN_BAR_PX: f64 = 2.5;

/// The overview's right edge may sit this fraction of its span past now, so
/// the live edge never lands on the lane's last pixel.
const FUTURE_SLACK_FRAC: f64 = 0.03;

/// When the view moves near an edge of the overview, the overview slides to
/// keep this fraction of its span between the box and the edge.
const CONTAIN_MARGIN_FRAC: f64 = 0.04;

/// How close (px) a press must land to a side of the view box to grab it.
pub(crate) const SIDE_GRAB_PX: f32 = 6.0;

/// Below this width (px) the box's sides can't be grabbed separately; a press
/// on or near it moves the whole view.
const MIN_BOX_FOR_SIDES_PX: f32 = 14.0;

/// The narrowest view a side drag can make.
pub(crate) const MIN_VIEW_SPAN_SECS: f64 = 6.0 * 60.0;

/// The overview window: where it starts, how wide it is, and whether it is
/// riding the live edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Overview {
    /// Left edge (Unix seconds).
    pub start: f64,
    /// Width (seconds).
    pub span: f64,
    /// The right edge is on now, so the overview slides with the clock.
    pub follows_now: bool,
    /// The main view at the last [`Overview::reconcile`]. When the view
    /// moves, the overview slides to keep it in sight; when only the overview
    /// moves, the view is left where the user put it.
    last_view: Option<(f64, f64)>,
}

impl Default for Overview {
    fn default() -> Self {
        Self {
            start: 0.0,
            span: OVERVIEW_DEFAULT_SPAN_SECS,
            follows_now: true,
            last_view: None,
        }
    }
}

/// What a press on the overview takes hold of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverviewGrab {
    /// The view box's left side (zooms the view, right edge fixed).
    ViewStart,
    /// The view box's right side (zooms the view, left edge fixed).
    ViewEnd,
    /// The view box itself (moves the view).
    View,
    /// The lane outside the box (slides the overview; a click looks there).
    Lane,
}

impl Overview {
    /// Right edge (Unix seconds).
    pub(crate) fn end(&self) -> f64 {
        self.start + self.span
    }

    /// Bring the overview up to date for this frame: size it to the view,
    /// slide it with now when it rides the live edge, keep a view that just
    /// moved in sight, and keep it inside the archive era and short of the
    /// future.
    pub(crate) fn reconcile(&mut self, view_start: f64, view_span: f64, now: f64, era_start: f64) {
        if view_span.is_finite() && view_span > 0.0 {
            if view_span > self.span * WIDEN_AT_FRAC {
                self.span = view_span / WIDEN_TO_FRAC;
            }
            self.span = self
                .span
                .max(MIN_OVERVIEW_SPAN_SECS)
                .max(view_span / MAX_VIEW_FRAC);
        }
        self.span = self.span.min(Self::max_span(now, era_start));
        if self.follows_now {
            self.start = self.max_end(now) - self.span;
        }
        let view = (view_start, view_span);
        if self.last_view != Some(view) && view_start.is_finite() && view_span > 0.0 {
            self.contain(view_start, view_start + view_span);
            self.last_view = Some(view);
        }
        self.clamp(now, era_start);
    }

    /// Slide the overview so it starts at `start` (a lane drag), within the
    /// same limits as [`Self::reconcile`]. Dropping the right edge on now
    /// makes it ride the live edge again.
    pub(crate) fn slide_to(&mut self, start: f64, now: f64, era_start: f64) {
        self.start = start;
        self.clamp(now, era_start);
    }

    /// Zoom the overview by `factor` (above 1 zooms out) keeping `anchor`
    /// under the pointer, from [`MIN_OVERVIEW_SPAN_SECS`] (and a little wider
    /// than the view) out to the whole archive era.
    pub(crate) fn zoom(
        &mut self,
        anchor: f64,
        factor: f64,
        view_span: f64,
        now: f64,
        era_start: f64,
    ) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let min = MIN_OVERVIEW_SPAN_SECS.max(view_span / MAX_VIEW_FRAC);
        let span = (self.span * factor).clamp(min, Self::max_span(now, era_start).max(min));
        self.start = anchor - (anchor - self.start) * span / self.span;
        self.span = span;
        self.clamp(now, era_start);
    }

    /// Frame a day (a click on a day, week, month or quarter bar): the
    /// default 48 h centered on it. The caller shows the day on the strip.
    pub(crate) fn go_to_day(&mut self, day_start: f64, now: f64, era_start: f64) {
        self.span = OVERVIEW_DEFAULT_SPAN_SECS;
        self.center_on(day_start + DAY_SECS / 2.0, now, era_start);
    }

    /// Center the overview on `ts` (the edge chip that brings an
    /// out-of-sight view box back).
    pub(crate) fn center_on(&mut self, ts: f64, now: f64, era_start: f64) {
        self.slide_to(ts - self.span / 2.0, now, era_start);
    }

    /// Which part of the lane a press at `x` grabs, given the view box's
    /// on-screen `(left, right)`.
    pub(crate) fn grab_at(box_px: (f32, f32), x: f32) -> OverviewGrab {
        let (left, right) = box_px;
        if right - left > MIN_BOX_FOR_SIDES_PX {
            if (x - left).abs() < SIDE_GRAB_PX {
                return OverviewGrab::ViewStart;
            }
            if (x - right).abs() < SIDE_GRAB_PX {
                return OverviewGrab::ViewEnd;
            }
            if x > left && x < right {
                return OverviewGrab::View;
            }
        } else if x > left - SIDE_GRAB_PX && x < right + SIDE_GRAB_PX {
            return OverviewGrab::View;
        }
        OverviewGrab::Lane
    }

    /// The widest the overview gets: from the start of the archive to just
    /// past now.
    fn max_span(now: f64, era_start: f64) -> f64 {
        ((now - era_start) / (1.0 - FUTURE_SLACK_FRAC)).max(MIN_OVERVIEW_SPAN_SECS)
    }

    fn max_end(&self, now: f64) -> f64 {
        now + self.span * FUTURE_SLACK_FRAC
    }

    fn contain(&mut self, view_start: f64, view_end: f64) {
        let margin = self.span * CONTAIN_MARGIN_FRAC;
        if view_start < self.start + margin {
            self.start = view_start - margin;
        } else if view_end > self.end() - margin {
            self.start = view_end + margin - self.span;
        }
    }

    fn clamp(&mut self, now: f64, era_start: f64) {
        self.start = self.start.min(self.max_end(now) - self.span).max(era_start);
        self.follows_now = self.end() >= now;
    }
}

const DAY_SECS: f64 = 86_400.0;

/// What the overview draws at a scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverviewDetail {
    /// Every scan, as on the strip (up to a few weeks across).
    Scans,
    /// Coverage summed into calendar bars.
    Bars(BucketGranularity),
}

/// The detail for `px_per_sec`: scans while an hour is at least
/// [`MIN_BAR_PX`] wide, then the finest bar that is.
pub(crate) fn detail_for(px_per_sec: f64) -> OverviewDetail {
    if 3600.0 * px_per_sec >= MIN_BAR_PX {
        return OverviewDetail::Scans;
    }
    let bar = [
        BucketGranularity::Day,
        BucketGranularity::Week,
        BucketGranularity::Month,
    ]
    .into_iter()
    .find(|g| g.nominal_secs() * px_per_sec >= MIN_BAR_PX)
    .unwrap_or(BucketGranularity::Quarter);
    OverviewDetail::Bars(bar)
}

/// How far apart the overview's time labels sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LabelStep {
    /// Clock time, at the strip's own tick spacing for this scale.
    Clock,
    /// Every `n` days (UTC midnights).
    Days(i64),
    /// Every `n` months (UTC month starts).
    Months(u32),
    /// Every `n` years (UTC January firsts).
    Years(i32),
}

/// The finest label step that keeps labels at least `min_px` apart.
pub(crate) fn label_step(px_per_sec: f64, min_px: f64) -> LabelStep {
    const MONTH: f64 = 2_629_746.0;
    const YEAR: f64 = 12.0 * MONTH;
    if DAY_SECS * px_per_sec >= min_px {
        return LabelStep::Clock;
    }
    if let Some(n) = [2, 7, 14]
        .into_iter()
        .find(|&n| n as f64 * DAY_SECS * px_per_sec >= min_px)
    {
        return LabelStep::Days(n);
    }
    if let Some(n) = [1, 3, 6]
        .into_iter()
        .find(|&n| n as f64 * MONTH * px_per_sec >= min_px)
    {
        return LabelStep::Months(n);
    }
    let n = [1, 2, 5, 10]
        .into_iter()
        .find(|&n| n as f64 * YEAR * px_per_sec >= min_px)
        .unwrap_or(20);
    LabelStep::Years(n)
}

/// Label times (Unix seconds, UTC-aligned) in `[start, end]` for a
/// day / month / year step. Empty for [`LabelStep::Clock`], which the strip's
/// tick ladder handles.
pub(crate) fn label_times(start: f64, end: f64, step: LabelStep) -> Vec<f64> {
    use chrono::{Datelike, NaiveDate};
    let ymd = |y: i32, m: u32| {
        NaiveDate::from_ymd_opt(y, m, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|d| d.and_utc().timestamp() as f64)
    };
    let Some(first) = chrono::DateTime::from_timestamp(start.floor() as i64, 0) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match step {
        LabelStep::Clock => {}
        LabelStep::Days(n) => {
            // Weekly labels land on Mondays; others on multiples of n days.
            let days = (start / DAY_SECS).floor() as i64;
            let offset = if n == 7 || n == 14 { 4 } else { 0 };
            let mut d = (days - offset).div_euclid(n) * n + offset;
            while (d as f64) * DAY_SECS <= end {
                if (d as f64) * DAY_SECS >= start {
                    out.push(d as f64 * DAY_SECS);
                }
                d += n;
            }
        }
        LabelStep::Months(n) => {
            let (mut y, mut m0) = (first.year(), first.month0() / n * n);
            while let Some(t) = ymd(y, m0 + 1) {
                if t > end {
                    break;
                }
                if t >= start {
                    out.push(t);
                }
                m0 += n;
                if m0 >= 12 {
                    m0 -= 12;
                    y += 1;
                }
            }
        }
        LabelStep::Years(n) => {
            let mut y = first.year().div_euclid(n) * n;
            while let Some(t) = ymd(y, 1) {
                if t > end {
                    break;
                }
                if t >= start {
                    out.push(t);
                }
                y += n;
            }
        }
    }
    out
}

/// The view a side drag of the box makes: the grabbed side follows the
/// pointer at `ts`, the other stays put, and the view never gets narrower
/// than [`MIN_VIEW_SPAN_SECS`]. `None` for a grab that isn't a side.
pub(crate) fn resized_view(
    grab: OverviewGrab,
    ts: f64,
    view_start: f64,
    view_end: f64,
) -> Option<(f64, f64)> {
    match grab {
        OverviewGrab::ViewStart => Some((ts.min(view_end - MIN_VIEW_SPAN_SECS), view_end)),
        OverviewGrab::ViewEnd => Some((view_start, ts.max(view_start + MIN_VIEW_SPAN_SECS))),
        OverviewGrab::View | OverviewGrab::Lane => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const ERA: f64 = 0.0;
    const NOW: f64 = 1_000_000.0;
    const H: f64 = 3600.0;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// An overview reconciled once against a view ending at now.
    fn live_overview() -> Overview {
        let mut ov = Overview::default();
        ov.reconcile(NOW - H, H, NOW, ERA);
        ov
    }

    #[wasm_bindgen_test]
    fn starts_48_hours_wide_on_the_live_edge() {
        let ov = live_overview();
        assert!(approx(ov.span, OVERVIEW_DEFAULT_SPAN_SECS));
        assert!(ov.follows_now);
        // The right edge sits just past now.
        assert!(ov.end() > NOW && ov.end() <= NOW + ov.span * FUTURE_SLACK_FRAC + 1e-6);
    }

    #[wasm_bindgen_test]
    fn rides_the_live_edge_as_time_passes() {
        let mut ov = live_overview();
        let end = ov.end();
        ov.reconcile(NOW - H, H, NOW + 600.0, ERA);
        assert!(approx(ov.end(), end + 600.0));
        assert!(ov.follows_now);
    }

    #[wasm_bindgen_test]
    fn a_wide_view_widens_the_overview_and_it_stays_wide() {
        let mut ov = Overview::default();
        let view = 45.0 * H;
        ov.reconcile(NOW - view, view, NOW, ERA);
        assert!(approx(ov.span, view / WIDEN_TO_FRAC));
        // Zooming the strip back in leaves the overview's zoom alone.
        ov.reconcile(NOW - H, H, NOW, ERA);
        assert!(approx(ov.span, view / WIDEN_TO_FRAC));
    }

    #[wasm_bindgen_test]
    fn zooms_from_hours_out_to_the_whole_era() {
        let era = NOW - 30.0 * 365.0 * 24.0 * H;
        let mut ov = Overview::default();
        ov.reconcile(NOW - H, H, NOW, era);
        // All the way out: the whole era, still riding the live edge.
        ov.zoom(NOW, 1e9, H, NOW, era);
        assert!(approx(ov.start, era));
        assert!(ov.follows_now);
        assert!(ov.end() >= NOW);
        // All the way in: no narrower than the floor or the view allows.
        ov.zoom(NOW, 1e-9, H, NOW, era);
        assert!(approx(ov.span, MIN_OVERVIEW_SPAN_SECS));
        ov.zoom(NOW, 1e-9, 2.8 * H, NOW, era);
        assert!(approx(ov.span, 2.8 * H / MAX_VIEW_FRAC));
    }

    #[wasm_bindgen_test]
    fn zoom_keeps_the_anchor_under_the_pointer() {
        let mut ov = live_overview();
        ov.slide_to(ov.start - 100.0 * H, NOW, ERA);
        let anchor = ov.start + ov.span * 0.25;
        ov.zoom(anchor, 2.0, H, NOW, ERA);
        assert!(approx((anchor - ov.start) / ov.span, 0.25));
    }

    #[wasm_bindgen_test]
    fn going_to_a_day_frames_it_in_48_hours() {
        let mut ov = live_overview();
        ov.zoom(NOW, 1000.0, H, NOW, ERA);
        let day = 86_400.0 * 5.0;
        ov.go_to_day(day, NOW, ERA);
        assert!(approx(ov.span, OVERVIEW_DEFAULT_SPAN_SECS));
        assert!(approx(ov.start + ov.span / 2.0, day + 43_200.0));
        assert!(!ov.follows_now);
    }

    #[wasm_bindgen_test]
    fn detail_steps_from_scans_to_bars() {
        let width = 1500.0;
        let at = |span: f64| detail_for(width / span);
        assert_eq!(at(48.0 * H), OverviewDetail::Scans);
        assert_eq!(at(20.0 * 24.0 * H), OverviewDetail::Scans);
        assert_eq!(
            at(60.0 * 24.0 * H),
            OverviewDetail::Bars(BucketGranularity::Day)
        );
        assert_eq!(
            at(3.0 * 365.0 * 24.0 * H),
            OverviewDetail::Bars(BucketGranularity::Week)
        );
        assert_eq!(
            at(30.0 * 365.0 * 24.0 * H),
            OverviewDetail::Bars(BucketGranularity::Month)
        );
        // A narrow lane over the whole era falls back to quarters.
        assert_eq!(
            detail_for(300.0 / (35.0 * 365.0 * 24.0 * H)),
            OverviewDetail::Bars(BucketGranularity::Quarter)
        );
    }

    #[wasm_bindgen_test]
    fn label_steps_coarsen_with_the_span() {
        let px = |span_days: f64| 1500.0 / (span_days * 86_400.0);
        assert_eq!(label_step(px(2.0), 60.0), LabelStep::Clock);
        assert_eq!(label_step(px(60.0), 60.0), LabelStep::Days(7));
        assert_eq!(label_step(px(365.0), 60.0), LabelStep::Months(1));
        assert_eq!(label_step(px(35.0 * 365.0), 60.0), LabelStep::Years(2));
    }

    #[wasm_bindgen_test]
    fn labels_land_on_calendar_boundaries() {
        use chrono::{Datelike, TimeZone, Utc};
        let t = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap().timestamp() as f64;
        let months = label_times(t(2025, 11, 15), t(2026, 3, 2), LabelStep::Months(1));
        assert_eq!(
            months,
            vec![t(2025, 12, 1), t(2026, 1, 1), t(2026, 2, 1), t(2026, 3, 1)]
        );
        let years = label_times(t(1991, 6, 5), t(2001, 1, 2), LabelStep::Years(5));
        assert_eq!(years, vec![t(1995, 1, 1), t(2000, 1, 1)]);
        let weeks = label_times(t(2026, 10, 1), t(2026, 10, 20), LabelStep::Days(7));
        assert!(!weeks.is_empty());
        for w in weeks {
            let d = Utc.timestamp_opt(w as i64, 0).unwrap();
            assert_eq!(d.weekday(), chrono::Weekday::Mon);
        }
        assert!(label_times(t(2026, 1, 1), t(2026, 1, 2), LabelStep::Clock).is_empty());
    }

    #[wasm_bindgen_test]
    fn sliding_the_lane_leaves_the_live_edge_and_the_view() {
        let mut ov = live_overview();
        ov.slide_to(ov.start - 10.0 * H, NOW, ERA);
        assert!(!ov.follows_now);
        let start = ov.start;
        // The view didn't move, so the next frame leaves the overview alone
        // even though the view box is now off its right edge.
        ov.reconcile(NOW - H, H, NOW + 60.0, ERA);
        assert!(approx(ov.start, start));
        assert!(!ov.follows_now);
    }

    #[wasm_bindgen_test]
    fn sliding_back_onto_now_rides_the_live_edge_again() {
        let mut ov = live_overview();
        ov.slide_to(ov.start - 10.0 * H, NOW, ERA);
        // Overshooting into the future stops at the live edge and re-pins.
        ov.slide_to(NOW, NOW, ERA);
        assert!(ov.follows_now);
        assert!(approx(ov.end(), NOW + ov.span * FUTURE_SLACK_FRAC));
    }

    #[wasm_bindgen_test]
    fn a_view_moved_past_an_edge_slides_the_overview_to_keep_it() {
        let mut ov = live_overview();
        // The user pans the main view three days back.
        let view_start = NOW - 72.0 * H;
        ov.reconcile(view_start, H, NOW, ERA);
        assert!(ov.start < view_start);
        assert!(view_start - ov.start >= ov.span * CONTAIN_MARGIN_FRAC - 1e-6);
        assert!(!ov.follows_now);
        // Panning it back to now re-pins the overview.
        ov.reconcile(NOW - H, H, NOW, ERA);
        assert!(ov.end() >= NOW);
        assert!(ov.follows_now);
    }

    #[wasm_bindgen_test]
    fn a_view_moved_inside_the_overview_does_not_slide_it() {
        let mut ov = live_overview();
        ov.slide_to(ov.start - 20.0 * H, NOW, ERA);
        let start = ov.start;
        // A view well inside the overview: nothing moves.
        let view_start = start + 10.0 * H;
        ov.reconcile(view_start, H, NOW, ERA);
        assert!(approx(ov.start, start));
    }

    #[wasm_bindgen_test]
    fn stops_at_the_start_of_the_archive() {
        let mut ov = live_overview();
        ov.slide_to(-1e9, NOW, ERA);
        assert!(approx(ov.start, ERA));
        ov.center_on(ERA + H, NOW, ERA);
        assert!(approx(ov.start, ERA));
    }

    #[wasm_bindgen_test]
    fn center_on_brings_a_lost_view_back() {
        let mut ov = live_overview();
        let ts = NOW - 200.0 * H;
        ov.center_on(ts, NOW, ERA);
        assert!(approx(ov.start + ov.span / 2.0, ts));
        assert!(!ov.follows_now);
    }

    #[wasm_bindgen_test]
    fn press_grabs_a_side_the_box_or_the_lane() {
        let wide = (100.0, 300.0);
        assert_eq!(Overview::grab_at(wide, 102.0), OverviewGrab::ViewStart);
        assert_eq!(Overview::grab_at(wide, 297.0), OverviewGrab::ViewEnd);
        assert_eq!(Overview::grab_at(wide, 200.0), OverviewGrab::View);
        assert_eq!(Overview::grab_at(wide, 50.0), OverviewGrab::Lane);
        assert_eq!(Overview::grab_at(wide, 320.0), OverviewGrab::Lane);
        // A thin box can only be moved, and is easier to hit.
        let thin = (100.0, 104.0);
        assert_eq!(Overview::grab_at(thin, 96.0), OverviewGrab::View);
        assert_eq!(Overview::grab_at(thin, 102.0), OverviewGrab::View);
        assert_eq!(Overview::grab_at(thin, 115.0), OverviewGrab::Lane);
    }

    #[wasm_bindgen_test]
    fn side_drags_resize_the_view_with_a_floor() {
        let (a, b) = (10_000.0, 20_000.0);
        assert_eq!(
            resized_view(OverviewGrab::ViewStart, 5_000.0, a, b),
            Some((5_000.0, b))
        );
        assert_eq!(
            resized_view(OverviewGrab::ViewEnd, 30_000.0, a, b),
            Some((a, 30_000.0))
        );
        // Dragged across the other side: stops at the minimum width.
        assert_eq!(
            resized_view(OverviewGrab::ViewStart, 25_000.0, a, b),
            Some((b - MIN_VIEW_SPAN_SECS, b))
        );
        assert_eq!(
            resized_view(OverviewGrab::ViewEnd, 0.0, a, b),
            Some((a, a + MIN_VIEW_SPAN_SECS))
        );
        assert_eq!(resized_view(OverviewGrab::View, 0.0, a, b), None);
    }
}
