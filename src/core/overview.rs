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

/// Label times (Unix seconds) in `[start, end]` for a day / month / year
/// step, on midnights of the chosen zone: `tz_offset_secs` is the zone's
/// offset east of UTC (0 for UTC), so the overview labels days the way the
/// strip does. Empty for [`LabelStep::Clock`], which the strip's tick ladder
/// handles.
pub(crate) fn label_times(start: f64, end: f64, step: LabelStep, tz_offset_secs: i64) -> Vec<f64> {
    let off = tz_offset_secs as f64;
    utc_label_times(start + off, end + off, step)
        .into_iter()
        .map(|t| t - off)
        .collect()
}

/// [`label_times`] on UTC midnights.
fn utc_label_times(start: f64, end: f64, step: LabelStep) -> Vec<f64> {
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

// ──────────────────────────────────────────────────────────────────────────
// Overview marks (one-instrument §5 "Overview", answers "Storm activity
// stays"). Two channels, two glyphs: bar height is storm activity, a thin
// underline is what is held on device. Saved events are amber pennants on
// the underline, a known outage is a neutral tick, now is the red dot, and
// the playhead a white tick when it isn't at now. The lane is never empty:
// a bar whose days aren't known yet draws a pending stub while they list.
// ──────────────────────────────────────────────────────────────────────────

/// Held spans closer than this (px) merge into one underline.
const HELD_MERGE_PX: f64 = 1.0;

/// Outage spans closer than this (px) merge into one tick.
const OUTAGE_MERGE_PX: f64 = 3.0;

/// A pennant is found by hover within this many px of its flag.
pub(crate) const PENNANT_HIT_PX: f64 = 5.0;

/// A saved event, as the overview is given it (current site only).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverviewEvent<'a> {
    pub start: f64,
    pub end: f64,
    pub name: &'a str,
}

/// A saved event's pennant: an amber flag at its start and an amber
/// underline across it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pennant {
    pub start: f64,
    pub end: f64,
    pub name: String,
}

/// The now dot, at the strength of the red that means now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NowDot {
    pub ts: f64,
    /// Full-strength red while streaming, the idle red otherwise.
    pub streaming: bool,
}

/// What the overview needs to place its marks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OverviewMarkInputs<'a> {
    /// The overview window (Unix seconds).
    pub start: f64,
    pub end: f64,
    pub px_per_sec: f64,
    /// Time spans held on this device, in time order.
    pub held: &'a [(f64, f64)],
    pub events: &'a [OverviewEvent<'a>],
    /// Starts of known outage days ([`crate::core::storm_activity::ActivityBook::outage_days`]).
    pub outage_days: &'a [i64],
    pub now: f64,
    pub streaming: bool,
    pub playhead: f64,
    /// The playhead rides now (pinned while streaming), so the now dot
    /// stands for it.
    pub playhead_at_now: bool,
    /// The selected range (the drag while drawing).
    pub range: Option<(f64, f64)>,
    /// The range's end rides now.
    pub range_pinned: bool,
}

/// Every mark on the overview lane, in window coordinates (Unix seconds).
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct OverviewMarks {
    /// The held underline: on-device spans, merged below a pixel apart.
    pub held: Vec<(f64, f64)>,
    pub pennants: Vec<Pennant>,
    /// Known outages, merged when a few pixels apart.
    pub outages: Vec<(f64, f64)>,
    pub now_dot: Option<NowDot>,
    /// The playhead's white tick, when it isn't at now.
    pub playhead_tick: Option<f64>,
    /// The amber range line along the lane's top.
    pub range: Option<(f64, f64)>,
}

impl OverviewMarks {
    /// Place the marks.
    pub(crate) fn build(i: &OverviewMarkInputs<'_>) -> Self {
        let in_window = |t: f64| t >= i.start && t <= i.end;
        let overlaps = |s: f64, e: f64| e >= i.start && s <= i.end;
        let px = i.px_per_sec.max(f64::MIN_POSITIVE);
        let held = merge_spans(
            i.held
                .iter()
                .copied()
                .filter(|&(s, e)| overlaps(s, e) && e > s)
                .map(|(s, e)| (s.max(i.start), e.min(i.end))),
            HELD_MERGE_PX / px,
        );
        let mut pennants: Vec<Pennant> = i
            .events
            .iter()
            .filter(|e| overlaps(e.start, e.end))
            .map(|e| Pennant {
                start: e.start,
                end: e.end.max(e.start),
                name: e.name.to_string(),
            })
            .collect();
        pennants.sort_by(|a, b| a.start.total_cmp(&b.start));
        let outages = merge_spans(
            i.outage_days
                .iter()
                .map(|&d| (d as f64, d as f64 + DAY_SECS))
                .filter(|&(s, e)| overlaps(s, e)),
            OUTAGE_MERGE_PX / px,
        );
        let range = i.range.map(|(s, e)| {
            let e = if i.range_pinned { i.now.max(s) } else { e };
            (s, e)
        });
        Self {
            held,
            pennants,
            outages,
            now_dot: in_window(i.now).then_some(NowDot {
                ts: i.now,
                streaming: i.streaming,
            }),
            playhead_tick: (!i.playhead_at_now && in_window(i.playhead)).then_some(i.playhead),
            range: range.filter(|&(s, e)| overlaps(s, e)),
        }
    }

    /// The pennant whose flag is within `tolerance_secs` of `ts` (the
    /// nearest), for its hover name.
    pub(crate) fn pennant_at(&self, ts: f64, tolerance_secs: f64) -> Option<&Pennant> {
        self.pennants
            .iter()
            .filter(|p| (p.start - ts).abs() <= tolerance_secs)
            .min_by(|a, b| (a.start - ts).abs().total_cmp(&(b.start - ts).abs()))
    }
}

/// Merge time-ordered-or-not spans whose gap is under `gap`.
fn merge_spans(spans: impl Iterator<Item = (f64, f64)>, gap: f64) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = spans.collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s - last.1 < gap => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// How a zoomed-out bar draws its activity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BarShape {
    /// As tall as this fraction of the bar area.
    Height(f32),
    /// Its days are still being listed: a faint stub, so the lane is never
    /// empty while the background listing fills it in.
    Pending,
    /// Nothing to draw (an outage, whose tick stands for it).
    Empty,
}

/// The shape of a bar for its days' activity on the view's `scale`.
pub(crate) fn bar_shape(
    activity: crate::core::storm_activity::BarActivity,
    scale: &crate::core::storm_activity::ViewScale,
) -> BarShape {
    use crate::core::storm_activity::BarActivity;
    match activity {
        BarActivity::Level(level) => BarShape::Height(scale.height_frac(level)),
        BarActivity::Unknown => BarShape::Pending,
        BarActivity::Outage => BarShape::Empty,
    }
}

// ── Hover words, in the chosen zone ────────────────────────────────────

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `Oct 3, 2026` in the clock's zone.
pub(crate) fn date_words(clock: &dyn crate::core::session::Clock, ts: f64) -> String {
    let (y, m, d) = clock.day(ts);
    format!("{} {d}, {y}", MONTHS[(m.clamp(1, 12) - 1) as usize])
}

/// The lane's hover text when it isn't over a bar or a volume.
pub(crate) const LANE_HINT: &str =
    "Click to look here. Drag to slide the overview, scroll to zoom it.";

/// A volume's hover text, zoomed in: when it started and how much echo.
pub(crate) fn scan_tip(
    clock: &dyn crate::core::session::Clock,
    scan: &crate::core::storm_activity::ScanActivity,
) -> String {
    format!(
        "{}, {}\n{} (volume {:.1} MB)\n{}",
        date_words(clock, scan.start),
        clock.hm(scan.start),
        crate::core::storm_activity::activity_word(scan.level),
        scan.bytes as f64 / 1e6,
        LANE_HINT
    )
}

/// What a zoomed-out bar's hover text is about.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BarTipInputs {
    /// The bar's span (Unix seconds).
    pub start: f64,
    pub end: f64,
    pub activity: crate::core::storm_activity::SpanActivity,
    /// A one-day bar's mean volume size, MB, when known.
    pub day_mean_mb: Option<f64>,
    /// Seconds of the bar held on this device.
    pub cached_secs: f64,
    /// The day a click goes to.
    pub go_to: f64,
}

/// A bar's hover text: its date, storm activity, what's downloaded, and the
/// day a click goes to.
pub(crate) fn bar_tip(clock: &dyn crate::core::session::Clock, b: &BarTipInputs) -> String {
    use crate::core::storm_activity::{activity_word, BarActivity};
    let one_day = b.end - b.start <= DAY_SECS;
    let when = if one_day {
        date_words(clock, b.start)
    } else if b.end - b.start <= 7.0 * DAY_SECS {
        format!("Week of {}", date_words(clock, b.start))
    } else {
        let (y, m, _) = clock.day(b.start);
        format!("{} {y}", MONTHS[(m.clamp(1, 12) - 1) as usize])
    };
    let weather = match b.activity.bar() {
        BarActivity::Level(level) if one_day => match b.day_mean_mb {
            Some(mb) => format!("{} (volumes average {mb:.1} MB)", activity_word(level)),
            None => activity_word(level).to_string(),
        },
        BarActivity::Level(level) => {
            let peak = b
                .activity
                .peak_day
                .map_or(String::new(), |d| date_words(clock, d as f64));
            let days = ((b.end - b.start) / DAY_SECS).round() as u32;
            let known = if b.activity.known_days < days {
                format!(", {} of {days} days checked", b.activity.known_days)
            } else {
                String::new()
            };
            format!("Most echo: {peak} ({}){known}", activity_word(level))
        }
        BarActivity::Outage => "No radar data recorded".to_string(),
        BarActivity::Unknown => "Checking echo amounts\u{2026}".to_string(),
    };
    let held = if b.cached_secs >= 3600.0 {
        format!("{:.1} h on this device", b.cached_secs / 3600.0)
    } else if b.cached_secs > 0.0 {
        format!("{:.0} min on this device", (b.cached_secs / 60.0).max(1.0))
    } else {
        "None downloaded".to_string()
    };
    format!(
        "{when}\n{weather}\n{held}\nClick to go to {}.",
        date_words(clock, b.go_to)
    )
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
        let months = label_times(t(2025, 11, 15), t(2026, 3, 2), LabelStep::Months(1), 0);
        assert_eq!(
            months,
            vec![t(2025, 12, 1), t(2026, 1, 1), t(2026, 2, 1), t(2026, 3, 1)]
        );
        let years = label_times(t(1991, 6, 5), t(2001, 1, 2), LabelStep::Years(5), 0);
        assert_eq!(years, vec![t(1995, 1, 1), t(2000, 1, 1)]);
        let weeks = label_times(t(2026, 10, 1), t(2026, 10, 20), LabelStep::Days(7), 0);
        assert!(!weeks.is_empty());
        for w in weeks {
            let d = Utc.timestamp_opt(w as i64, 0).unwrap();
            assert_eq!(d.weekday(), chrono::Weekday::Mon);
        }
        assert!(label_times(t(2026, 1, 1), t(2026, 1, 2), LabelStep::Clock, 0).is_empty());
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

    // ── Marks ──────────────────────────────────────────────────────────

    fn inputs<'a>(
        held: &'a [(f64, f64)],
        events: &'a [OverviewEvent<'a>],
        outage_days: &'a [i64],
    ) -> OverviewMarkInputs<'a> {
        OverviewMarkInputs {
            start: 0.0,
            end: 100_000.0,
            // 1000 px across: 100 s per px.
            px_per_sec: 0.01,
            held,
            events,
            outage_days,
            now: 90_000.0,
            streaming: false,
            playhead: 50_000.0,
            playhead_at_now: false,
            range: None,
            range_pinned: false,
        }
    }

    #[wasm_bindgen_test]
    fn held_underline_follows_the_cached_spans_not_the_whole_day() {
        // One cached minute stays one minute wide; spans under a pixel apart
        // merge; spans outside the window drop; edges clip.
        let held = [
            (-500.0, 200.0),
            (1000.0, 1060.0),
            (1100.0, 1300.0),
            (40_000.0, 40_060.0),
            (200_000.0, 300_000.0),
        ];
        let m = OverviewMarks::build(&inputs(&held, &[], &[]));
        assert_eq!(
            m.held,
            vec![(0.0, 200.0), (1000.0, 1300.0), (40_000.0, 40_060.0)]
        );
    }

    #[wasm_bindgen_test]
    fn saved_events_become_pennants_in_time_order() {
        let events = [
            OverviewEvent {
                start: 60_000.0,
                end: 61_000.0,
                name: "Derecho",
            },
            OverviewEvent {
                start: 10_000.0,
                end: 12_000.0,
                name: "Hail",
            },
            OverviewEvent {
                start: 500_000.0,
                end: 501_000.0,
                name: "Out of view",
            },
        ];
        let m = OverviewMarks::build(&inputs(&[], &events, &[]));
        let names: Vec<&str> = m.pennants.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Hail", "Derecho"]);
        // Hover finds the nearest flag within tolerance.
        assert_eq!(m.pennant_at(10_300.0, 500.0).unwrap().name, "Hail");
        assert!(m.pennant_at(30_000.0, 500.0).is_none());
    }

    #[wasm_bindgen_test]
    fn outage_days_merge_into_ticks() {
        let day = DAY_SECS as i64;
        let ov = OverviewMarkInputs {
            end: 30.0 * DAY_SECS,
            px_per_sec: 1000.0 / (30.0 * DAY_SECS),
            now: 29.0 * DAY_SECS,
            ..inputs(&[], &[], &[])
        };
        let days = [2 * day, 3 * day, 10 * day];
        let m = OverviewMarks::build(&OverviewMarkInputs {
            outage_days: &days,
            ..ov
        });
        assert_eq!(
            m.outages,
            vec![
                (2.0 * DAY_SECS, 4.0 * DAY_SECS),
                (10.0 * DAY_SECS, 11.0 * DAY_SECS)
            ]
        );
    }

    #[wasm_bindgen_test]
    fn now_dot_strength_and_playhead_tick() {
        let base = inputs(&[], &[], &[]);
        let m = OverviewMarks::build(&base);
        assert_eq!(
            m.now_dot,
            Some(NowDot {
                ts: 90_000.0,
                streaming: false
            })
        );
        assert_eq!(m.playhead_tick, Some(50_000.0));
        // Riding now: the dot stands for the playhead, at full strength.
        let m = OverviewMarks::build(&OverviewMarkInputs {
            streaming: true,
            playhead_at_now: true,
            playhead: 90_000.0,
            ..base
        });
        assert!(m.now_dot.unwrap().streaming);
        assert_eq!(m.playhead_tick, None);
        // Now out of the window: no dot.
        let m = OverviewMarks::build(&OverviewMarkInputs {
            now: 200_000.0,
            ..base
        });
        assert_eq!(m.now_dot, None);
    }

    #[wasm_bindgen_test]
    fn a_pinned_range_line_runs_to_now() {
        let base = inputs(&[], &[], &[]);
        let m = OverviewMarks::build(&OverviewMarkInputs {
            range: Some((80_000.0, 85_000.0)),
            range_pinned: true,
            ..base
        });
        assert_eq!(m.range, Some((80_000.0, 90_000.0)));
        let m = OverviewMarks::build(&OverviewMarkInputs {
            range: Some((80_000.0, 85_000.0)),
            ..base
        });
        assert_eq!(m.range, Some((80_000.0, 85_000.0)));
    }

    #[wasm_bindgen_test]
    fn bars_are_never_blank_while_their_days_list() {
        use crate::core::storm_activity::{BarActivity, ViewScale};
        let scale = ViewScale::default();
        assert_eq!(bar_shape(BarActivity::Unknown, &scale), BarShape::Pending);
        assert_eq!(bar_shape(BarActivity::Outage, &scale), BarShape::Empty);
        match bar_shape(BarActivity::Level(1.0), &scale) {
            BarShape::Height(h) => assert!((h - 1.0).abs() < 1e-6),
            other => panic!("{other:?}"),
        }
    }

    /// A UTC clock for hover words.
    struct Utc;
    impl crate::core::session::Clock for Utc {
        fn hms(&self, ts: f64) -> String {
            let d = chrono::DateTime::from_timestamp(ts as i64, 0).unwrap();
            d.format("%H:%M:%S").to_string()
        }
        fn hm(&self, ts: f64) -> String {
            let d = chrono::DateTime::from_timestamp(ts as i64, 0).unwrap();
            d.format("%H:%M").to_string()
        }
        fn day(&self, ts: f64) -> (i32, u32, u32) {
            use chrono::Datelike;
            let d = chrono::DateTime::from_timestamp(ts as i64, 0).unwrap();
            (d.year(), d.month(), d.day())
        }
    }

    #[wasm_bindgen_test]
    fn hover_words_use_the_clock_and_no_zone_suffix() {
        use crate::core::storm_activity::{ScanActivity, SpanActivity};
        // 2026-10-03 00:00 UTC.
        let oct3 = 1_790_985_600.0;
        assert_eq!(date_words(&Utc, oct3), "Oct 3, 2026");
        let scan = ScanActivity {
            start: oct3 + 3600.0 + 120.0,
            end: oct3 + 4000.0,
            bytes: 12_300_000,
            level: 0.7,
        };
        assert!(
            scan_tip(&Utc, &scan).starts_with("Oct 3, 2026, 01:02\nLots of echo (volume 12.3 MB)")
        );
        let tip = bar_tip(
            &Utc,
            &BarTipInputs {
                start: oct3,
                end: oct3 + DAY_SECS,
                activity: SpanActivity::default(),
                day_mean_mb: None,
                cached_secs: 60.0,
                go_to: oct3,
            },
        );
        assert_eq!(
            tip,
            "Oct 3, 2026\nChecking echo amounts\u{2026}\n1 min on this device\nClick to go to Oct 3, 2026."
        );
        assert!(!tip.contains("UTC"));
    }

    #[wasm_bindgen_test]
    fn labels_follow_the_chosen_zone() {
        use chrono::{TimeZone, Utc as ChronoUtc};
        let t = |y, m, d, h| {
            ChronoUtc
                .with_ymd_and_hms(y, m, d, h, 0, 0)
                .unwrap()
                .timestamp() as f64
        };
        // Five hours west of UTC: local midnight is 05:00 UTC.
        let off = -5 * 3600;
        let days = label_times(
            t(2026, 10, 1, 0),
            t(2026, 10, 5, 0),
            LabelStep::Days(2),
            off,
        );
        assert!(!days.is_empty());
        for d in &days {
            let local = (*d as i64 + off).rem_euclid(DAY_SECS as i64);
            assert_eq!(local, 0);
        }
        let months = label_times(
            t(2026, 9, 15, 0),
            t(2026, 11, 15, 0),
            LabelStep::Months(1),
            off,
        );
        assert_eq!(months, vec![t(2026, 10, 1, 5), t(2026, 11, 1, 5)]);
    }
}
