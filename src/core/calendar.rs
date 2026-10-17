//! The phone's calendar sheet, "Go to a day" (phone timeline §2): the
//! phone's overview.
//!
//! The desktop overview lane shows where the weather was; a phone has no
//! room for a second lane, so it gets a calendar: a month at a time, every
//! day shaded by its storm activity from the nightly index, a thin underline
//! under the days held on device, today outlined in the now color, and the
//! days after today dimmed and inert. Tapping the month name opens a grid of
//! years; a year opens its twelve months; a month opens its grid.
//!
//! **What it reads.** The grid costs no S3 request: day levels come from
//! the [`ActivityBook`] (the site's published index, read once per session
//! by the storm-activity shell, plus any day already listed), and the
//! on-device underline from the held spans of the scan index. A day the book
//! does not know (newer than the index, or a gap in it) is `unknown`, not
//! quiet. The index is keyed by UTC day; a day in the chosen zone reads the
//! index day of the same date, the UTC day that holds its local midday for
//! any zone within ±12 h.
//!
//! **Landing.** A tapped day ([`Intent::GoToDay`]) puts the strip at Day
//! zoom and pauses on the day's stormiest volume ([`land_on_day`]). Its
//! volumes come from the one per-day listing Day zoom already makes; until
//! that arrives the playhead waits at the day's local midday, and when it
//! arrives it moves to the stormiest volume once, only if the playhead has
//! not been touched since ([`Landing`], [`landing_after`],
//! [`settle_landing`]).
//!
//! Everything here is pure. The zone is handed in as a [`Zone`] (a browser
//! fact the shell reads); `ui::mobile::go_to_day` draws what [`calendar_vm`]
//! returns and emits the intents.

use crate::core::phone::{sheet_rect, Insets, SheetRect};
use crate::core::storm_activity::{ActivityBook, ScanActivity, ViewScale};
use crate::core::{Intent, RadarTimeline};
use crate::data::SiteId;
use chrono::{Datelike, NaiveDate};

const DAY_SECS: i64 = 86_400;

/// The first month the calendar reaches: the archive's first, June 1991.
pub(crate) const FIRST_MONTH: YearMonth = YearMonth {
    year: 1991,
    month: 6,
};

/// The sheet's title.
pub(crate) const TITLE: &str = "Go to a day";

/// The weekday row, Monday first.
pub(crate) const WEEKDAYS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];

/// The legend under the grid: the shade, the underline, the outline.
pub(crate) const LEGEND: [&str; 3] = ["storm activity", "on device", "today"];

/// The portrait sheet's height: header, month row, weekdays, six weeks of
/// 40 px rows and the legend, with the frame's margins.
pub(crate) const CALENDAR_SHEET_H: f32 = 384.0;

/// A horizontal drag at least this long (points) on the grid moves a month.
pub(crate) const SWIPE_MIN_PX: f32 = 40.0;

/// A landed playhead that has moved further than this (seconds) was
/// touched, so the stormiest volume no longer takes it.
pub(crate) const TOUCH_TOLERANCE_SECS: f64 = 1.0;

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

// ───────────────────────────────────────────────────────────────────────────
// Months, dates and the zone
// ───────────────────────────────────────────────────────────────────────────

/// A calendar month.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct YearMonth {
    pub year: i32,
    /// 1..=12.
    pub month: u32,
}

impl YearMonth {
    pub(crate) fn new(year: i32, month: u32) -> Self {
        Self {
            year,
            month: month.clamp(1, 12),
        }
    }

    /// The month holding `date`.
    pub(crate) fn of(date: NaiveDate) -> Self {
        Self::new(date.year(), date.month())
    }

    fn index(self) -> i64 {
        self.year as i64 * 12 + (self.month as i64 - 1)
    }

    fn from_index(i: i64) -> Self {
        Self::new(i.div_euclid(12) as i32, (i.rem_euclid(12) + 1) as u32)
    }

    /// `months` later (earlier when negative).
    pub(crate) fn offset(self, months: i64) -> Self {
        Self::from_index(self.index() + months)
    }

    /// Its first day.
    pub(crate) fn first_day(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, 1).unwrap_or_default()
    }

    /// Its last day.
    pub(crate) fn last_day(self) -> NaiveDate {
        self.offset(1)
            .first_day()
            .pred_opt()
            .unwrap_or_else(|| self.first_day())
    }

    /// "October".
    pub(crate) fn name(self) -> &'static str {
        MONTH_NAMES[(self.month - 1) as usize]
    }

    /// "Oct".
    pub(crate) fn short_name(self) -> &'static str {
        &self.name()[..3]
    }

    /// "October 2026".
    pub(crate) fn title(self) -> String {
        format!("{} {}", self.name(), self.year)
    }
}

/// The chosen zone, as the shell reads it: its offset east of UTC at a
/// moment (seconds; 0 for UTC). Local time may change offset across the
/// year, so it is asked per moment.
pub(crate) trait Zone {
    fn offset_secs(&self, ts: f64) -> i64;
}

/// A zone with one offset all year (UTC, or a test's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FixedZone(pub i64);

impl Zone for FixedZone {
    fn offset_secs(&self, _ts: f64) -> i64 {
        self.0
    }
}

fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default()
}

fn epoch_days(date: NaiveDate) -> i64 {
    (date - epoch()).num_days()
}

fn date_of_epoch_days(days: i64) -> NaiveDate {
    epoch()
        .checked_add_signed(chrono::Duration::days(days))
        .unwrap_or_default()
}

/// The date `ts` falls on in the zone.
pub(crate) fn local_date(ts: f64, zone: &dyn Zone) -> NaiveDate {
    let local = ts.floor() as i64 + zone.offset_secs(ts);
    date_of_epoch_days(local.div_euclid(DAY_SECS))
}

/// The local midnight that starts `date` (Unix seconds). The offset is
/// read at the midnight itself, so a day that changes offset still starts
/// where the zone says.
pub(crate) fn local_midnight(date: NaiveDate, zone: &dyn Zone) -> f64 {
    let utc = epoch_days(date) * DAY_SECS;
    let guess = utc - zone.offset_secs(utc as f64);
    (utc - zone.offset_secs(guess as f64)) as f64
}

/// One day in the chosen zone, with its span in Unix seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CalendarDay {
    pub date: NaiveDate,
    /// Its local midnight.
    pub start: f64,
    /// The next local midnight.
    pub end: f64,
}

impl CalendarDay {
    /// `date` in the zone.
    pub(crate) fn of(date: NaiveDate, zone: &dyn Zone) -> Self {
        let next = date.succ_opt().unwrap_or(date);
        Self {
            date,
            start: local_midnight(date, zone),
            end: local_midnight(next, zone),
        }
    }

    /// Its local midday (the middle of its span, so a day that changes
    /// offset still has one).
    pub(crate) fn midday(&self) -> f64 {
        (self.start + self.end) / 2.0
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The sheet's state and navigation
// ───────────────────────────────────────────────────────────────────────────

/// What the sheet shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CalendarMode {
    /// One month's days.
    #[default]
    Month,
    /// One year's twelve months.
    Year,
    /// Every year the site's index reaches.
    Years,
}

/// The calendar sheet, open (on `Chrome`). Mutated only by intents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CalendarSheet {
    /// The month on screen (its year in the Year mode); `None` until the
    /// user moves, which shows the playhead's month.
    pub month: Option<YearMonth>,
    pub mode: CalendarMode,
}

/// Where a tap in the sheet goes. Targets are absolute, read off the
/// view-model, so the reducer needs no clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CalendarNav {
    /// A month's grid (the arrows, a swipe, a month in the year view).
    Month(YearMonth),
    /// A year's months (a year in the years grid, the year view's arrows).
    Year(i32),
    /// The grid of years (a tap on the month name or the year).
    Years,
}

/// A month inside the calendar's reach, `[FIRST_MONTH, current]`.
pub(crate) fn clamp_month(month: YearMonth, current: YearMonth) -> YearMonth {
    month.clamp(FIRST_MONTH, current.max(FIRST_MONTH))
}

/// The month before `month`, if the calendar reaches it.
pub(crate) fn prev_month(month: YearMonth) -> Option<YearMonth> {
    let prev = month.offset(-1);
    (prev >= FIRST_MONTH).then_some(prev)
}

/// The month after `month`, if it is not past `current`.
pub(crate) fn next_month(month: YearMonth, current: YearMonth) -> Option<YearMonth> {
    let next = month.offset(1);
    (next <= current).then_some(next)
}

/// The latest month it is anywhere on Earth (UTC+14) at `now`: a bound
/// for [`reduce_nav`] that needs no zone. The view-model clamps again to
/// today's month in the chosen zone.
pub(crate) fn latest_month(now: f64) -> YearMonth {
    YearMonth::of(local_date(now, &FixedZone(14 * 3600)))
}

/// Apply a navigation tap. `current` is today's month (at most
/// [`latest_month`]).
pub(crate) fn reduce_nav(sheet: &mut CalendarSheet, nav: CalendarNav, current: YearMonth) {
    match nav {
        CalendarNav::Month(month) => {
            sheet.month = Some(clamp_month(month, current));
            sheet.mode = CalendarMode::Month;
        }
        CalendarNav::Year(year) => {
            let keep = sheet.month.map_or(1, |m| m.month);
            sheet.month = Some(clamp_month(YearMonth::new(year, keep), current));
            sheet.mode = CalendarMode::Year;
        }
        CalendarNav::Years => sheet.mode = CalendarMode::Years,
    }
}

/// The years the sheet offers: from the site's first indexed year (the
/// archive's, without an index) through `current_year`.
pub(crate) fn year_range(book: &ActivityBook, site: &SiteId, current_year: i32) -> (i32, i32) {
    let first = book
        .index_range(site)
        .map(|(first, _)| date_of_epoch_days(first.div_euclid(DAY_SECS)).year())
        .unwrap_or(FIRST_MONTH.year)
        .clamp(FIRST_MONTH.year, current_year.max(FIRST_MONTH.year));
    (first, current_year.max(first))
}

/// A horizontal swipe on the grid: `+1` (a month on) for a swipe left, `-1`
/// for a swipe right, `None` for anything shorter than [`SWIPE_MIN_PX`] or
/// more vertical than horizontal.
pub(crate) fn swipe_months(dx: f32, dy: f32) -> Option<i64> {
    if dx.abs() < SWIPE_MIN_PX || dx.abs() < 2.0 * dy.abs() {
        return None;
    }
    Some(if dx < 0.0 { 1 } else { -1 })
}

/// Where the sheet sits: the phone sheet's place (bottom in portrait, right
/// in landscape), but in portrait only as tall as the month needs.
pub(crate) fn calendar_sheet_rect(
    width: f32,
    height: f32,
    landscape: bool,
    insets: Insets,
) -> SheetRect {
    let r = sheet_rect(width, height, landscape, insets);
    if landscape {
        return r;
    }
    let h = CALENDAR_SHEET_H.min(r.h);
    SheetRect {
        y: r.y + r.h - h,
        h,
        ..r
    }
}

// ───────────────────────────────────────────────────────────────────────────
// View-models
// ───────────────────────────────────────────────────────────────────────────

/// What the sheet reads, gathered by the shell.
#[derive(Clone, Copy)]
pub(crate) struct CalendarFacts<'a> {
    pub book: &'a ActivityBook,
    pub site: &'a SiteId,
    /// Time spans held on this device (the scan index's coverage).
    pub held: &'a [(f64, f64)],
    pub now: f64,
}

impl CalendarFacts<'_> {
    fn holds(&self, start: f64, end: f64) -> bool {
        self.held.iter().any(|&(s, e)| s < end && e > start)
    }

    /// Known index days in `[first, last]` (dates) by their UTC midnight.
    fn levels(&self, first: NaiveDate, last: NaiveDate) -> Vec<(NaiveDate, Option<f32>)> {
        self.book
            .levels(
                self.site,
                epoch_days(first) * DAY_SECS,
                epoch_days(last) * DAY_SECS,
            )
            .into_iter()
            .map(|(day, level)| (date_of_epoch_days(day.div_euclid(DAY_SECS)), level))
            .collect()
    }
}

/// One day in the month grid.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DayCell {
    pub day: CalendarDay,
    /// "17".
    pub label: String,
    /// The shade, `0..=1`, on the month's scale ([`ViewScale`]): a day
    /// with data at least the scale's floor, an outage or an unknown day 0.
    pub level: f32,
    /// The book has no entry for the day (newer than the index, or a gap):
    /// drawn as unknown, not as quiet. Never set for future days.
    pub unknown: bool,
    /// A known day the radar recorded nothing.
    pub outage: bool,
    /// Some scan of the day is held on this device.
    pub on_device: bool,
    pub is_today: bool,
    /// After today: dimmed and inert.
    pub is_future: bool,
    /// In the month on screen (the weeks' leading and trailing days are
    /// not).
    pub in_month: bool,
}

impl DayCell {
    /// Whether a tap goes to it.
    pub(crate) fn tappable(&self) -> bool {
        self.in_month && !self.is_future
    }
}

/// One month's grid.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MonthVm {
    pub month: YearMonth,
    /// "October 2026".
    pub title: String,
    /// The arrows' targets; `None` at the calendar's ends.
    pub prev: Option<YearMonth>,
    pub next: Option<YearMonth>,
    /// Monday-first weeks covering the month (four to six).
    pub weeks: Vec<[DayCell; 7]>,
}

/// The month grid for `month` in the zone.
pub(crate) fn month_vm(month: YearMonth, facts: &CalendarFacts<'_>, zone: &dyn Zone) -> MonthVm {
    let today = local_date(facts.now, zone);
    let current = YearMonth::of(today);
    let first = month.first_day();
    let last = month.last_day();
    let grid_start = first
        .checked_sub_signed(chrono::Duration::days(
            first.weekday().num_days_from_monday() as i64,
        ))
        .unwrap_or(first);
    let grid_end = last
        .checked_add_signed(chrono::Duration::days(
            6 - last.weekday().num_days_from_monday() as i64,
        ))
        .unwrap_or(last);
    let known: std::collections::HashMap<NaiveDate, Option<f32>> =
        facts.levels(grid_start, grid_end).into_iter().collect();
    let scale = ViewScale::from_levels(
        known
            .iter()
            .filter(|(d, _)| YearMonth::of(**d) == month && **d <= today)
            .filter_map(|(_, level)| *level),
    );

    let mut weeks = Vec::new();
    let mut date = grid_start;
    while date <= grid_end {
        let week: [DayCell; 7] = std::array::from_fn(|i| {
            let d = date + chrono::Duration::days(i as i64);
            let is_future = d > today;
            let entry = known.get(&d).copied();
            let day = CalendarDay::of(d, zone);
            DayCell {
                day,
                label: d.day().to_string(),
                level: match entry {
                    Some(Some(level)) if !is_future => scale.height_frac(level),
                    _ => 0.0,
                },
                unknown: entry.is_none() && !is_future,
                outage: matches!(entry, Some(None)),
                on_device: facts.holds(day.start, day.end),
                is_today: d == today,
                is_future,
                in_month: YearMonth::of(d) == month,
            }
        });
        weeks.push(week);
        date += chrono::Duration::days(7);
    }
    MonthVm {
        month,
        title: month.title(),
        prev: prev_month(month),
        next: next_month(month, current),
        weeks,
    }
}

/// One month in the year view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MonthCell {
    pub month: YearMonth,
    /// "Oct".
    pub label: &'static str,
    /// Its stormiest known day's level, absolute.
    pub peak: Option<f32>,
    /// The shade, `0..=1`, on the year's scale.
    pub level: f32,
    /// Some scan of the month is held on this device.
    pub on_device: bool,
    /// Today's month.
    pub is_current: bool,
    /// Inside the calendar's reach (after the archive began, not after
    /// today's month).
    pub enabled: bool,
}

/// One year's twelve months.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct YearVm {
    pub year: i32,
    /// "2026".
    pub title: String,
    pub prev: Option<i32>,
    pub next: Option<i32>,
    pub months: Vec<MonthCell>,
}

/// The year view for `year`.
pub(crate) fn year_vm(year: i32, facts: &CalendarFacts<'_>, zone: &dyn Zone) -> YearVm {
    let today = local_date(facts.now, zone);
    let current = YearMonth::of(today);
    let (first_year, last_year) = year_range(facts.book, facts.site, current.year);
    let jan = YearMonth::new(year, 1);
    let levels = facts.levels(jan.first_day(), YearMonth::new(year, 12).last_day());
    let peaks: Vec<Option<f32>> = (1..=12)
        .map(|m| {
            levels
                .iter()
                .filter(|(d, _)| d.month() == m && *d <= today)
                .filter_map(|(_, l)| *l)
                .max_by(f32::total_cmp)
        })
        .collect();
    let scale = ViewScale::from_levels(peaks.iter().flatten().copied());
    let months = (1..=12u32)
        .map(|m| {
            let month = YearMonth::new(year, m);
            let peak = peaks[(m - 1) as usize];
            let start = local_midnight(month.first_day(), zone);
            let end = local_midnight(month.offset(1).first_day(), zone);
            MonthCell {
                month,
                label: month.short_name(),
                peak,
                level: peak.map_or(0.0, |p| scale.height_frac(p)),
                on_device: facts.holds(start, end),
                is_current: month == current,
                enabled: month >= FIRST_MONTH && month <= current,
            }
        })
        .collect();
    YearVm {
        year,
        title: year.to_string(),
        prev: (year > first_year).then_some(year - 1),
        next: (year < last_year).then_some(year + 1),
        months,
    }
}

/// One year in the years grid.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct YearCell {
    pub year: i32,
    /// "2026".
    pub label: String,
    /// Its stormiest known day's level, absolute.
    pub peak: Option<f32>,
    /// The shade, `0..=1`, on the scale of the years shown.
    pub level: f32,
    pub on_device: bool,
    pub is_current: bool,
}

/// The years grid.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct YearsVm {
    /// "1991 – 2026".
    pub title: String,
    pub years: Vec<YearCell>,
}

/// The grid of every year the site's index reaches ([`year_range`]).
pub(crate) fn years_vm(facts: &CalendarFacts<'_>, zone: &dyn Zone) -> YearsVm {
    let today = local_date(facts.now, zone);
    let (first, last) = year_range(facts.book, facts.site, today.year());
    let levels = facts.levels(
        YearMonth::new(first, 1).first_day(),
        YearMonth::new(last, 12).last_day(),
    );
    let peaks: Vec<Option<f32>> = (first..=last)
        .map(|y| {
            levels
                .iter()
                .filter(|(d, _)| d.year() == y && *d <= today)
                .filter_map(|(_, l)| *l)
                .max_by(f32::total_cmp)
        })
        .collect();
    let scale = ViewScale::from_levels(peaks.iter().flatten().copied());
    let years = (first..=last)
        .zip(peaks)
        .map(|(year, peak)| {
            let start = local_midnight(YearMonth::new(year, 1).first_day(), zone);
            let end = local_midnight(YearMonth::new(year + 1, 1).first_day(), zone);
            YearCell {
                year,
                label: year.to_string(),
                peak,
                level: peak.map_or(0.0, |p| scale.height_frac(p)),
                on_device: facts.holds(start, end),
                is_current: year == today.year(),
            }
        })
        .collect();
    YearsVm {
        title: format!("{first} \u{2013} {last}"),
        years,
    }
}

/// The sheet's body.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CalendarBody {
    Month(MonthVm),
    Year(YearVm),
    Years(YearsVm),
}

/// The whole sheet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CalendarVm {
    pub title: &'static str,
    /// The site ID, at the title's right.
    pub site: String,
    pub body: CalendarBody,
    pub legend: [&'static str; 3],
}

/// The sheet for its state. A sheet that has not moved shows the
/// playhead's month (clamped to the calendar's reach).
pub(crate) fn calendar_vm(
    sheet: &CalendarSheet,
    playhead: f64,
    facts: &CalendarFacts<'_>,
    zone: &dyn Zone,
) -> CalendarVm {
    let current = YearMonth::of(local_date(facts.now, zone));
    let month = clamp_month(
        sheet
            .month
            .unwrap_or_else(|| YearMonth::of(local_date(playhead, zone))),
        current,
    );
    let body = match sheet.mode {
        CalendarMode::Month => CalendarBody::Month(month_vm(month, facts, zone)),
        CalendarMode::Year => CalendarBody::Year(year_vm(month.year, facts, zone)),
        CalendarMode::Years => CalendarBody::Years(years_vm(facts, zone)),
    };
    CalendarVm {
        title: TITLE,
        site: facts.site.0.clone(),
        body,
        legend: LEGEND,
    }
}

/// The month a swipe on `vm`'s grid goes to.
pub(crate) fn swipe_nav(vm: &MonthVm, dx: f32, dy: f32) -> Option<CalendarNav> {
    match swipe_months(dx, dy)? {
        1 => vm.next.map(CalendarNav::Month),
        _ => vm.prev.map(CalendarNav::Month),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Landing on a day
// ───────────────────────────────────────────────────────────────────────────

/// The day's stormiest volume: the largest file (the earliest of equals).
pub(crate) fn stormiest(listing: &[ScanActivity]) -> Option<&ScanActivity> {
    listing
        .iter()
        .reduce(|best, v| if v.bytes > best.bytes { v } else { best })
}

/// Where the playhead goes for a volume starting at `start`, as the strip's
/// snapping lands it: the end of its first frame when the volume is held
/// (so that frame shows whole), else its start.
pub(crate) fn volume_landing(start: f64, held: &RadarTimeline) -> f64 {
    held.find_scan_at_timestamp(start + 0.5)
        .filter(|scan| (scan.key_timestamp - start).abs() < 60.0)
        .and_then(|scan| {
            scan.sweeps
                .iter()
                .min_by(|a, b| a.start_time.total_cmp(&b.start_time))
                .map(|first| first.end_time)
                .filter(|end| *end > start)
        })
        .unwrap_or(start)
}

/// A landing that may still move to the day's stormiest volume, once.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Landing {
    pub site: SiteId,
    pub day: CalendarDay,
    /// Where the playhead was put.
    pub landed_at: f64,
}

/// Where [`Intent::GoToDay`] puts the playhead, and whether it waits for the
/// listing. With the day's volumes known (`listing`), its stormiest volume
/// ([`stormiest`], [`volume_landing`]); without them, local midday (never
/// past now) and a [`Landing`] that [`settle_landing`] finishes. A day with
/// no volumes stays at midday.
pub(crate) fn land_on_day(
    site: &SiteId,
    day: CalendarDay,
    listing: Option<&[ScanActivity]>,
    held: &RadarTimeline,
    now: f64,
) -> (f64, Option<Landing>) {
    let midday = day.midday().min(now);
    match listing {
        Some(volumes) => (
            stormiest(volumes).map_or(midday, |v| volume_landing(v.start, held).min(now)),
            None,
        ),
        None => (
            midday,
            Some(Landing {
                site: site.clone(),
                day,
                landed_at: midday,
            }),
        ),
    }
}

/// A pending landing after an intent: anything that moves the playhead or
/// the selection (a seek, a step, play, going live, a range, another site,
/// another day) means the user has taken over, so it is dropped.
pub(crate) fn landing_after(landing: Option<Landing>, intent: &Intent) -> Option<Landing> {
    match intent {
        Intent::Seek { .. }
        | Intent::Step { .. }
        | Intent::TogglePlayPause
        | Intent::GoLive
        | Intent::ReturnToLive
        | Intent::StartLive
        | Intent::StopLive(_)
        | Intent::StopLooping
        | Intent::ClearLoop
        | Intent::SelectRange { .. }
        | Intent::ApplyLoopPreset(_)
        | Intent::SetLoopPoint(_)
        | Intent::LoopFromScan(_)
        | Intent::SelectSite { .. }
        | Intent::GoToDay(_) => None,
        _ => landing,
    }
}

/// One frame's facts for a pending landing.
#[derive(Clone, Copy)]
pub(crate) struct LandingFacts<'a> {
    /// The site on screen.
    pub site: &'a SiteId,
    /// The landing day's volumes, once listed
    /// ([`ActivityBook::listing`]).
    pub listing: Option<&'a [ScanActivity]>,
    pub held: &'a RadarTimeline,
    pub playhead: f64,
    pub playing: bool,
    pub now: f64,
}

/// Finish a pending landing: `(still pending, re-seek to)`. It waits while
/// the listing is out; when the listing arrives it re-seeks to the stormiest
/// volume once and is done. It is dropped without a seek when the playhead
/// was touched (moved, or playing) or the site changed.
pub(crate) fn settle_landing(
    landing: Landing,
    f: &LandingFacts<'_>,
) -> (Option<Landing>, Option<f64>) {
    if &landing.site != f.site
        || f.playing
        || (f.playhead - landing.landed_at).abs() > TOUCH_TOLERANCE_SECS
    {
        return (None, None);
    }
    let Some(volumes) = f.listing else {
        return (Some(landing), None);
    };
    let target = stormiest(volumes)
        .map(|v| volume_landing(v.start, f.held).min(f.now))
        .filter(|ts| (ts - landing.landed_at).abs() > TOUCH_TOLERANCE_SECS);
    (None, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Scan, Sweep};
    use crate::data::DayActivity;
    use wasm_bindgen_test::wasm_bindgen_test;

    const UTC: FixedZone = FixedZone(0);
    /// US Central daylight time.
    const CDT: FixedZone = FixedZone(-5 * 3600);

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn utc_ts(y: i32, m: u32, d: u32, h: u32) -> f64 {
        (epoch_days(date(y, m, d)) * DAY_SECS + h as i64 * 3600) as f64
    }

    fn site() -> SiteId {
        SiteId::new("KTLX")
    }

    /// 2026-10-17 15:00 UTC (10:00 CDT), a Saturday.
    fn now() -> f64 {
        utc_ts(2026, 10, 17, 15)
    }

    fn day_record(d: NaiveDate, mean_bytes: u64, volumes: u32) -> DayActivity {
        let day_start = epoch_days(d) * DAY_SECS;
        DayActivity {
            site: site(),
            day_start,
            volumes,
            mean_bytes,
            format: "V06".into(),
            fetched_at_ms: (day_start + 2 * DAY_SECS) * 1000,
            has_scans: false,
            scan_secs: Vec::new(),
            scan_bytes: Vec::new(),
        }
    }

    fn facts<'a>(
        book: &'a ActivityBook,
        s: &'a SiteId,
        held: &'a [(f64, f64)],
    ) -> CalendarFacts<'a> {
        CalendarFacts {
            book,
            site: s,
            held,
            now: now(),
        }
    }

    fn cell(vm: &MonthVm, d: NaiveDate) -> &DayCell {
        vm.weeks
            .iter()
            .flatten()
            .find(|c| c.day.date == d)
            .expect("day in grid")
    }

    // ── months and dates ──────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn months_step_across_years_and_name_themselves() {
        let jan = YearMonth::new(2026, 1);
        assert_eq!(jan.offset(-1), YearMonth::new(2025, 12));
        assert_eq!(YearMonth::new(2025, 12).offset(1), jan);
        assert_eq!(jan.offset(14), YearMonth::new(2027, 3));
        assert_eq!(YearMonth::new(2024, 2).last_day(), date(2024, 2, 29));
        assert_eq!(YearMonth::new(2026, 10).title(), "October 2026");
        assert_eq!(YearMonth::new(2026, 9).short_name(), "Sep");
    }

    #[wasm_bindgen_test]
    fn days_are_local_midnight_to_midnight() {
        let d = CalendarDay::of(date(2026, 10, 17), &CDT);
        assert_eq!(d.start, utc_ts(2026, 10, 17, 5));
        assert_eq!(d.end, utc_ts(2026, 10, 18, 5));
        assert_eq!(d.midday(), utc_ts(2026, 10, 17, 17));
        assert_eq!(
            local_date(utc_ts(2026, 10, 17, 3), &CDT),
            date(2026, 10, 16)
        );
        assert_eq!(
            local_date(utc_ts(2026, 10, 17, 3), &UTC),
            date(2026, 10, 17)
        );
    }

    // ── the month grid ────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn the_grid_is_monday_first_and_covers_the_month_edges() {
        let book = ActivityBook::default();
        let s = site();
        // October 2026 starts on a Thursday and ends on a Saturday.
        let vm = month_vm(YearMonth::new(2026, 10), &facts(&book, &s, &[]), &UTC);
        assert_eq!(vm.weeks.len(), 5);
        assert_eq!(vm.weeks[0][0].day.date, date(2026, 9, 28));
        assert_eq!(vm.weeks[0][0].day.date.weekday(), chrono::Weekday::Mon);
        assert!(!vm.weeks[0][2].in_month);
        assert!(vm.weeks[0][3].in_month);
        assert_eq!(vm.weeks[0][3].label, "1");
        assert_eq!(vm.weeks[4][6].day.date, date(2026, 11, 1));
        assert!(!vm.weeks[4][6].in_month);
        assert_eq!(vm.title, "October 2026");
        assert_eq!(WEEKDAYS[0], "M");

        // February 2021 starts on a Monday and has exactly four weeks.
        let feb = month_vm(YearMonth::new(2021, 2), &facts(&book, &s, &[]), &UTC);
        assert_eq!(feb.weeks.len(), 4);
        assert!(feb.weeks.iter().flatten().all(|c| c.in_month));
        // A month starting on a Sunday with 31 days takes six.
        let aug = month_vm(YearMonth::new(2021, 8), &facts(&book, &s, &[]), &UTC);
        assert_eq!(aug.weeks.len(), 6);
    }

    #[wasm_bindgen_test]
    fn today_is_marked_and_later_days_are_future_and_inert() {
        let book = ActivityBook::default();
        let s = site();
        let vm = month_vm(YearMonth::new(2026, 10), &facts(&book, &s, &[]), &UTC);
        let today = cell(&vm, date(2026, 10, 17));
        assert!(today.is_today && !today.is_future && today.tappable());
        let tomorrow = cell(&vm, date(2026, 10, 18));
        assert!(tomorrow.is_future && !tomorrow.tappable());
        assert!(!tomorrow.unknown, "the future is not unknown, just not yet");
        assert!(
            !cell(&vm, date(2026, 9, 30)).tappable(),
            "outside the month"
        );
        assert_eq!(vm.weeks.iter().flatten().filter(|c| c.is_today).count(), 1);
    }

    #[wasm_bindgen_test]
    fn today_follows_the_zone() {
        let book = ActivityBook::default();
        let s = site();
        // 03:00 UTC on the 17th is still the 16th in Central time.
        let f = CalendarFacts {
            now: utc_ts(2026, 10, 17, 3),
            ..facts(&book, &s, &[])
        };
        let vm = month_vm(YearMonth::new(2026, 10), &f, &CDT);
        assert!(cell(&vm, date(2026, 10, 16)).is_today);
        assert!(cell(&vm, date(2026, 10, 17)).is_future);
    }

    #[wasm_bindgen_test]
    fn unknown_days_are_not_quiet_and_outages_are_known() {
        let mut book = ActivityBook::default();
        book.insert_index([
            day_record(date(2026, 10, 1), 3_300_000, 280),
            day_record(date(2026, 10, 2), 0, 0),
        ]);
        let s = site();
        let vm = month_vm(YearMonth::new(2026, 10), &facts(&book, &s, &[]), &UTC);
        let quiet = cell(&vm, date(2026, 10, 1));
        assert!(!quiet.unknown && !quiet.outage);
        assert!(quiet.level > 0.0, "a day with data still reads as data");
        let outage = cell(&vm, date(2026, 10, 2));
        assert!(outage.outage && !outage.unknown);
        assert_eq!(outage.level, 0.0);
        // Past the index's last day: unknown, level 0.
        let newer = cell(&vm, date(2026, 10, 16));
        assert!(newer.unknown && !newer.outage);
        assert_eq!(newer.level, 0.0);
    }

    #[wasm_bindgen_test]
    fn one_storm_day_reads_strong_on_the_months_scale() {
        let mut book = ActivityBook::default();
        let mut days: Vec<DayActivity> = (1..=15)
            .map(|d| day_record(date(2026, 10, d), 3_300_000, 280))
            .collect();
        days.push(day_record(date(2026, 10, 9), 9_000_000, 280));
        book.insert_index(days);
        let s = site();
        let vm = month_vm(YearMonth::new(2026, 10), &facts(&book, &s, &[]), &UTC);
        let storm = cell(&vm, date(2026, 10, 9)).level;
        let quiet = cell(&vm, date(2026, 10, 8)).level;
        assert!(
            (storm - 1.0).abs() < 1e-6,
            "the month's stormiest fills: {storm}"
        );
        assert!(quiet < 0.2, "a typical day sits near the floor: {quiet}");
        // A moderate absolute level would not fill on its own.
        assert!(crate::core::storm_activity::activity_level(9_000_000, 3_300_000.0) < 0.6);
    }

    #[wasm_bindgen_test]
    fn held_scans_underline_their_local_days() {
        let book = ActivityBook::default();
        let s = site();
        // 03:00–04:00 UTC on the 15th: the 14th in Central time.
        let held = [(utc_ts(2026, 10, 15, 3), utc_ts(2026, 10, 15, 4))];
        let utc = month_vm(YearMonth::new(2026, 10), &facts(&book, &s, &held), &UTC);
        assert!(cell(&utc, date(2026, 10, 15)).on_device);
        assert!(!cell(&utc, date(2026, 10, 14)).on_device);
        let cdt = month_vm(YearMonth::new(2026, 10), &facts(&book, &s, &held), &CDT);
        assert!(cell(&cdt, date(2026, 10, 14)).on_device);
        assert!(!cell(&cdt, date(2026, 10, 15)).on_device);
    }

    // ── navigation ────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn navigation_is_clamped_to_the_archive_and_this_month() {
        let current = YearMonth::new(2026, 10);
        assert_eq!(prev_month(FIRST_MONTH), None);
        assert_eq!(prev_month(YearMonth::new(1991, 7)), Some(FIRST_MONTH));
        assert_eq!(next_month(current, current), None);
        assert_eq!(next_month(YearMonth::new(2026, 9), current), Some(current));
        assert_eq!(clamp_month(YearMonth::new(1980, 1), current), FIRST_MONTH);
        assert_eq!(clamp_month(YearMonth::new(2030, 1), current), current);

        let book = ActivityBook::default();
        let s = site();
        let vm = month_vm(current, &facts(&book, &s, &[]), &UTC);
        assert_eq!(vm.next, None);
        assert_eq!(vm.prev, Some(YearMonth::new(2026, 9)));
    }

    #[wasm_bindgen_test]
    fn intents_move_the_sheet_between_month_year_and_years() {
        let current = YearMonth::new(2026, 10);
        let mut sheet = CalendarSheet::default();
        reduce_nav(&mut sheet, CalendarNav::Years, current);
        assert_eq!(sheet.mode, CalendarMode::Years);
        reduce_nav(&mut sheet, CalendarNav::Year(2013), current);
        assert_eq!(sheet.mode, CalendarMode::Year);
        assert_eq!(sheet.month.map(|m| m.year), Some(2013));
        reduce_nav(
            &mut sheet,
            CalendarNav::Month(YearMonth::new(2013, 5)),
            current,
        );
        assert_eq!(sheet.mode, CalendarMode::Month);
        assert_eq!(sheet.month, Some(YearMonth::new(2013, 5)));
        // Out of reach is clamped.
        reduce_nav(
            &mut sheet,
            CalendarNav::Month(YearMonth::new(2027, 1)),
            current,
        );
        assert_eq!(sheet.month, Some(current));
        // A year keeps the month, clamped into reach.
        reduce_nav(&mut sheet, CalendarNav::Year(1991), current);
        assert_eq!(sheet.month, Some(YearMonth::new(1991, 10)));
        sheet.month = Some(YearMonth::new(2013, 3));
        reduce_nav(&mut sheet, CalendarNav::Year(1991), current);
        assert_eq!(sheet.month, Some(FIRST_MONTH));
    }

    #[wasm_bindgen_test]
    fn an_unmoved_sheet_shows_the_playheads_month() {
        let book = ActivityBook::default();
        let s = site();
        let vm = calendar_vm(
            &CalendarSheet::default(),
            utc_ts(2013, 5, 20, 22),
            &facts(&book, &s, &[]),
            &UTC,
        );
        assert_eq!(vm.title, "Go to a day");
        assert_eq!(vm.site, "KTLX");
        let CalendarBody::Month(m) = vm.body else {
            panic!("month body")
        };
        assert_eq!(m.month, YearMonth::new(2013, 5));
    }

    #[wasm_bindgen_test]
    fn a_horizontal_swipe_moves_a_month() {
        assert_eq!(swipe_months(-60.0, 5.0), Some(1));
        assert_eq!(swipe_months(60.0, -5.0), Some(-1));
        assert_eq!(swipe_months(20.0, 0.0), None, "too short");
        assert_eq!(swipe_months(60.0, 50.0), None, "mostly vertical");
        let book = ActivityBook::default();
        let s = site();
        let vm = month_vm(YearMonth::new(2026, 10), &facts(&book, &s, &[]), &UTC);
        assert_eq!(swipe_nav(&vm, -60.0, 0.0), None, "nothing after this month");
        assert_eq!(
            swipe_nav(&vm, 60.0, 0.0),
            Some(CalendarNav::Month(YearMonth::new(2026, 9)))
        );
    }

    #[wasm_bindgen_test]
    fn the_portrait_sheet_is_as_tall_as_a_month_and_landscape_is_the_phone_sheet() {
        let r = calendar_sheet_rect(390.0, 844.0, false, (0.0, 0.0, 34.0, 0.0));
        assert_eq!(r.h, CALENDAR_SHEET_H);
        assert_eq!(r.y + r.h, 844.0 - 34.0);
        assert_eq!(r.w, 390.0);
        let l = calendar_sheet_rect(844.0, 390.0, true, (0.0, 0.0, 0.0, 0.0));
        assert_eq!(l, sheet_rect(844.0, 390.0, true, (0.0, 0.0, 0.0, 0.0)));
    }

    // ── the year views ────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn the_year_grid_has_twelve_months_with_their_peaks() {
        let mut book = ActivityBook::default();
        book.insert_index([
            day_record(date(2025, 12, 31), 3_300_000, 280),
            day_record(date(2026, 3, 1), 3_300_000, 280),
            day_record(date(2026, 5, 20), 12_000_000, 280),
            day_record(date(2026, 5, 21), 4_000_000, 280),
        ]);
        let s = site();
        let held = [(utc_ts(2026, 7, 4, 12), utc_ts(2026, 7, 4, 13))];
        let vm = year_vm(2026, &facts(&book, &s, &held), &UTC);
        assert_eq!(vm.months.len(), 12);
        assert_eq!(vm.title, "2026");
        assert_eq!(vm.months[0].label, "Jan");
        assert_eq!(vm.months[0].peak, None);
        assert_eq!(vm.months[0].level, 0.0);
        let may = &vm.months[4];
        assert!(may.peak.is_some() && may.peak > vm.months[2].peak);
        assert!(
            (may.level - 1.0).abs() < 1e-6,
            "the year's stormiest month fills"
        );
        assert!(vm.months[2].level < may.level);
        assert!(vm.months[6].on_device && !vm.months[5].on_device);
        assert!(vm.months[9].is_current && vm.months[9].enabled);
        assert!(!vm.months[10].enabled, "after this month");
        // The index starts in 2025, so the years reach back to it.
        assert_eq!(vm.next, None);
        assert_eq!(vm.prev, Some(2025));

        let first = year_vm(1991, &facts(&book, &s, &[]), &UTC);
        assert!(!first.months[4].enabled && first.months[5].enabled);
        assert_eq!(first.prev, None);
    }

    #[wasm_bindgen_test]
    fn the_year_list_is_clamped_to_the_index() {
        let mut book = ActivityBook::default();
        let s = site();
        assert_eq!(year_range(&book, &s, 2026), (1991, 2026));
        book.insert_index([
            day_record(date(1994, 3, 2), 500_000, 200),
            day_record(date(2026, 10, 14), 3_300_000, 280),
        ]);
        assert_eq!(year_range(&book, &s, 2026), (1994, 2026));
        let vm = years_vm(&facts(&book, &s, &[]), &UTC);
        assert_eq!(vm.years.first().map(|y| y.year), Some(1994));
        assert_eq!(vm.years.last().map(|y| y.year), Some(2026));
        assert_eq!(vm.title, "1994 \u{2013} 2026");
        assert!(vm.years.last().unwrap().is_current);
        assert!(vm.years.iter().filter(|y| y.peak.is_some()).count() == 2);
        // A year view of the index's first year has nothing before it.
        assert_eq!(year_vm(1994, &facts(&book, &s, &[]), &UTC).prev, None);
    }

    // ── landing ───────────────────────────────────────────────────────

    fn vol(start: f64, bytes: u32) -> ScanActivity {
        ScanActivity {
            start,
            end: start + 300.0,
            bytes,
            level: 0.0,
        }
    }

    fn held_scan(start: f64) -> RadarTimeline {
        let sweep = |s: f64, e: f64| Sweep {
            start_time: s,
            end_time: e,
            elevation: 0.5,
            elevation_number: 1,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: Vec::new(),
        };
        RadarTimeline {
            scans: vec![Scan {
                start_time: start,
                end_time: start + 290.0,
                key_timestamp: start,
                vcp: 212,
                vcp_pattern: None,
                sweeps: vec![
                    sweep(start + 20.0, start + 40.0),
                    sweep(start, start + 18.0),
                ],
                completeness: None,
                cached_sweep_count: None,
                planned_sweep_count: None,
            }],
        }
    }

    #[wasm_bindgen_test]
    fn the_stormiest_volume_is_the_largest_file() {
        let list = [vol(100.0, 3), vol(400.0, 9), vol(700.0, 9), vol(1000.0, 1)];
        assert_eq!(stormiest(&list).map(|v| v.start), Some(400.0));
        assert_eq!(stormiest(&[]), None);
    }

    #[wasm_bindgen_test]
    fn a_held_volume_lands_on_its_first_frames_end() {
        let t = 1_000_000.0;
        assert_eq!(volume_landing(t, &held_scan(t)), t + 18.0);
        assert_eq!(volume_landing(t + 3_000.0, &held_scan(t)), t + 3_000.0);
        assert_eq!(volume_landing(t, &RadarTimeline::default()), t);
    }

    #[wasm_bindgen_test]
    fn landing_waits_at_midday_until_the_listing_arrives() {
        let s = site();
        let day = CalendarDay::of(date(2026, 10, 9), &CDT);
        let held = RadarTimeline::default();
        let (ts, pending) = land_on_day(&s, day, None, &held, now());
        assert_eq!(ts, utc_ts(2026, 10, 9, 17), "local midday");
        let pending = pending.expect("waits for the listing");

        let storm = day.start + 6.0 * 3600.0;
        let list = [vol(day.start + 600.0, 3_000_000), vol(storm, 20_000_000)];
        let f = LandingFacts {
            site: &s,
            listing: None,
            held: &held,
            playhead: ts,
            playing: false,
            now: now(),
        };
        // Still out: keep waiting.
        assert_eq!(
            settle_landing(pending.clone(), &f),
            (Some(pending.clone()), None)
        );
        // Arrived: one re-seek to the stormiest volume, then done.
        let arrived = LandingFacts {
            listing: Some(&list),
            ..f
        };
        assert_eq!(settle_landing(pending, &arrived), (None, Some(storm)));
    }

    #[wasm_bindgen_test]
    fn a_listed_day_lands_on_its_stormiest_volume_at_once() {
        let s = site();
        let day = CalendarDay::of(date(2026, 10, 9), &UTC);
        let list = [vol(day.start + 600.0, 3), vol(day.start + 900.0, 8)];
        let (ts, pending) = land_on_day(&s, day, Some(&list), &held_scan(day.start + 900.0), now());
        assert_eq!(ts, day.start + 900.0 + 18.0);
        assert_eq!(pending, None);
        // A day with no volumes stays at midday.
        let (ts, pending) = land_on_day(&s, day, Some(&[]), &RadarTimeline::default(), now());
        assert_eq!((ts, pending), (day.midday(), None));
    }

    #[wasm_bindgen_test]
    fn today_never_lands_past_now() {
        let s = site();
        let day = CalendarDay::of(date(2026, 10, 17), &CDT);
        // 10:00 local: midday is still ahead.
        let (ts, _) = land_on_day(&s, day, None, &RadarTimeline::default(), now());
        assert_eq!(ts, now());
    }

    #[wasm_bindgen_test]
    fn no_re_seek_after_the_playhead_was_touched() {
        let s = site();
        let day = CalendarDay::of(date(2026, 10, 9), &UTC);
        let held = RadarTimeline::default();
        let (ts, pending) = land_on_day(&s, day, None, &held, now());
        let pending = pending.unwrap();
        let list = [vol(day.start + 600.0, 30)];
        let arrived = LandingFacts {
            site: &s,
            listing: Some(&list),
            held: &held,
            playhead: ts,
            playing: false,
            now: now(),
        };
        // Scrubbed (a gesture that bypasses intents): dropped, no seek.
        let scrubbed = LandingFacts {
            playhead: ts + 600.0,
            ..arrived
        };
        assert_eq!(settle_landing(pending.clone(), &scrubbed), (None, None));
        // Playing: dropped.
        let playing = LandingFacts {
            playing: true,
            ..arrived
        };
        assert_eq!(settle_landing(pending.clone(), &playing), (None, None));
        // Another site: dropped.
        let other = SiteId::new("KFWS");
        let moved = LandingFacts {
            site: &other,
            ..arrived
        };
        assert_eq!(settle_landing(pending.clone(), &moved), (None, None));
        // Any playhead intent drops it before it settles.
        use crate::core::transport::{SeekGesture, StepDir, StepUnit};
        for intent in [
            Intent::Seek {
                ts: 5.0,
                gesture: SeekGesture::Press,
            },
            Intent::Step {
                dir: StepDir::Forward,
                unit: StepUnit::Frame,
            },
            Intent::TogglePlayPause,
            Intent::GoLive,
            Intent::GoToDay(day),
        ] {
            assert_eq!(
                landing_after(Some(pending.clone()), &intent),
                None,
                "{intent:?}"
            );
        }
        // Opening a sheet or zooming the strip is not a touch.
        assert_eq!(
            landing_after(Some(pending.clone()), &Intent::OpenGoToDay),
            Some(pending.clone())
        );
        assert_eq!(
            landing_after(
                Some(pending.clone()),
                &Intent::SetPhoneStripZoom(crate::core::phone_strip::StripZoom::Volumes)
            ),
            Some(pending)
        );
    }
}
