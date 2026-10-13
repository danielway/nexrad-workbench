//! The overview lane above the timeline (round-4 design): a sliding window,
//! 48 hours wide by default, with a box marking what the main strip shows.
//!
//! The overview is a map of the view, never a control of playback. Dragging
//! the box moves the view, dragging its sides zooms the view, dragging the
//! rest of the lane slides the overview itself, and a click centers the view
//! there. While its right edge is on now it slides forward with the clock.
//! There are no edge traps: the overview can slide anywhere in the archive
//! era, and the box can leave it (an edge chip brings it back).
//!
//! Pure decisions only. The egui shell (`ui::timeline::overview`) paints the
//! lane and turns pointer facts into calls on [`Overview`] and on
//! [`PlaybackState::show_view`].
//!
//! [`PlaybackState::show_view`]: crate::core::PlaybackState::show_view

/// How much time the overview shows unless the view needs more.
pub(crate) const OVERVIEW_DEFAULT_SPAN_SECS: f64 = 48.0 * 3600.0;

/// The view box never fills more of the overview than this, so there is
/// always context on either side of it. A wider view widens the overview.
const MAX_VIEW_FRAC: f64 = 0.6;

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
            self.span = OVERVIEW_DEFAULT_SPAN_SECS.max(view_span / MAX_VIEW_FRAC);
        }
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
    fn a_wide_view_widens_the_overview() {
        let mut ov = Overview::default();
        let view = 60.0 * H;
        ov.reconcile(NOW - view, view, NOW, ERA);
        assert!(approx(ov.span, view / MAX_VIEW_FRAC));
        // A narrow view puts it back to 48 h.
        ov.reconcile(NOW - H, H, NOW, ERA);
        assert!(approx(ov.span, OVERVIEW_DEFAULT_SPAN_SECS));
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
