//! The time field: the one clock you can type into (§4 "one clock"). The
//! transport's playhead clock is one; the event dialog's start and end will be
//! the same field.
//!
//! Pure formatting and parsing; the zone is the shell's (a browser fact), so
//! both go through [`ZoneCalendar`]. The egui widget is `ui::time_field`.
//!
//! - Shown as `11:24:05` today, `Oct 3 11:24:05` on another day this year,
//!   and `Oct 3 2019 11:24:05` in another year, in the chosen zone.
//! - Accepts what it shows, with or without seconds or a date, plus ISO dates
//!   (`2026-10-03 11:24`), RFC 3339 with an offset, 12-hour times
//!   (`2:41 PM`), epoch seconds or milliseconds, and a trailing `Z` / `UTC` to
//!   mean UTC whatever the chosen zone. A bare time or a date without a year
//!   is read on the day / in the year the field showed.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

use crate::core::session::Clock;
use crate::core::timeline_interaction::NEXRAD_ARCHIVE_START_SECS;

/// Calendar arithmetic in the user's chosen zone, implemented by the shell.
pub(crate) trait ZoneCalendar: Clock {
    /// The Unix seconds of a wall-clock date and time in this zone, or `None`
    /// when it does not exist (a DST gap) or is out of range.
    fn to_unix(&self, date: NaiveDate, time: NaiveTime) -> Option<f64>;
}

/// UTC, for the explicit `Z` / `UTC` suffix and for tests.
pub(crate) struct UtcCalendar;

impl Clock for UtcCalendar {
    fn hms(&self, ts: f64) -> String {
        utc(ts).format("%H:%M:%S").to_string()
    }

    fn hm(&self, ts: f64) -> String {
        utc(ts).format("%H:%M").to_string()
    }

    fn day(&self, ts: f64) -> (i32, u32, u32) {
        let d = utc(ts);
        (d.year(), d.month(), d.day())
    }
}

impl ZoneCalendar for UtcCalendar {
    fn to_unix(&self, date: NaiveDate, time: NaiveTime) -> Option<f64> {
        Some(Utc.from_utc_datetime(&date.and_time(time)).timestamp() as f64)
    }
}

fn utc(ts: f64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(ts.floor() as i64, 0)
        .single()
        .unwrap_or_default()
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The field's text for `ts`: the clock, with the date before it only when
/// the day is not today (and the year only when it is not this year).
pub(crate) fn format_time_field(ts: f64, now: f64, clock: &dyn Clock) -> String {
    let (y, m, d) = clock.day(ts);
    let (ty, tm, td) = clock.day(now);
    let hms = clock.hms(ts);
    let month = MONTHS[(m.clamp(1, 12) - 1) as usize];
    if (y, m, d) == (ty, tm, td) {
        hms
    } else if y == ty {
        format!("{month} {d} {hms}")
    } else {
        format!("{month} {d} {y} {hms}")
    }
}

/// Why a typed time was refused. The field shows [`Self::message`] inline.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TimeFieldError {
    /// Not a time the field can read.
    Unreadable(String),
    /// After now: there is nothing there yet.
    Future,
    /// Before the NEXRAD archive begins (June 1991).
    BeforeArchive,
}

impl TimeFieldError {
    pub(crate) fn message(&self) -> String {
        match self {
            TimeFieldError::Unreadable(text) => format!("Couldn't read \"{text}\" as a time"),
            TimeFieldError::Future => "That time hasn't happened yet".to_string(),
            TimeFieldError::BeforeArchive => "The archive starts in June 1991".to_string(),
        }
    }
}

/// Read a typed or pasted time. `base` is the time the field showed (a bare
/// time is read on its day, a date without a year in its year); `now` bounds
/// it. A trailing `Z` or `UTC` reads it in UTC, else in `zone`.
pub(crate) fn parse_time_field(
    text: &str,
    base: f64,
    now: f64,
    zone: &dyn ZoneCalendar,
) -> Result<f64, TimeFieldError> {
    let trimmed = text.trim();
    let unreadable = || TimeFieldError::Unreadable(trimmed.to_string());
    if trimmed.is_empty() {
        return Err(unreadable());
    }
    let ts = parse_any(trimmed, base, zone).ok_or_else(unreadable)?;
    if ts > now + 1.0 {
        return Err(TimeFieldError::Future);
    }
    if ts < NEXRAD_ARCHIVE_START_SECS {
        return Err(TimeFieldError::BeforeArchive);
    }
    Ok(ts)
}

fn parse_any(t: &str, base: f64, zone: &dyn ZoneCalendar) -> Option<f64> {
    // Absolute: carries its own offset.
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
        return Some(dt.timestamp() as f64);
    }
    // Bare epoch seconds (10 digits) or milliseconds (13). A shorter run of
    // digits is not a time.
    if (10..=13).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit()) {
        let n: f64 = t.parse().ok()?;
        return Some(if t.len() > 10 { n / 1000.0 } else { n });
    }
    // A trailing Z / UTC / GMT means UTC.
    let upper = t.to_ascii_uppercase();
    let (t, zone): (&str, &dyn ZoneCalendar) = match ["UTC", "GMT", "Z"]
        .iter()
        .find(|suffix| upper.ends_with(*suffix) && t.len() > suffix.len())
    {
        Some(suffix) => (t[..t.len() - suffix.len()].trim_end(), &UtcCalendar),
        None => (t, zone),
    };
    let (date, time) = parse_wall(t, base, zone)?;
    zone.to_unix(date, time)
}

/// A wall-clock date and time from the text, filling a missing date (or year)
/// from `base` in `zone`.
fn parse_wall(t: &str, base: f64, zone: &dyn ZoneCalendar) -> Option<(NaiveDate, NaiveTime)> {
    const FULL: [&str; 6] = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
        "%b %d %Y %H:%M:%S",
        "%b %d %Y %H:%M",
    ];
    for fmt in FULL {
        if let Ok(dt) = NaiveDateTime::parse_from_str(t, fmt) {
            return Some((dt.date(), dt.time()));
        }
    }
    for fmt in ["%Y-%m-%d", "%b %d %Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(t, fmt) {
            return Some((d, NaiveTime::MIN));
        }
    }
    let (by, bm, bd) = zone.day(base);
    // A month and day without a year: the year the field showed.
    let with_year = format!("{by} {t}");
    for fmt in ["%Y %b %d %H:%M:%S", "%Y %b %d %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(&with_year, fmt) {
            return Some((dt.date(), dt.time()));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(&with_year, "%Y %b %d") {
        return Some((d, NaiveTime::MIN));
    }
    // A bare time: the day the field showed.
    let base_date = NaiveDate::from_ymd_opt(by, bm, bd)?;
    for fmt in [
        "%H:%M:%S",
        "%H:%M",
        "%I:%M:%S %p",
        "%I:%M %p",
        "%I:%M:%S%p",
        "%I:%M%p",
    ] {
        if let Ok(time) = NaiveTime::parse_from_str(t, fmt) {
            return Some((base_date, time));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// 2026-10-15T11:24:05Z.
    const NOW: f64 = 1_792_063_445.0;

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> f64 {
        Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap().timestamp() as f64
    }

    fn parse(text: &str) -> Result<f64, TimeFieldError> {
        parse_time_field(text, NOW - 600.0, NOW, &UtcCalendar)
    }

    #[wasm_bindgen_test]
    fn now_is_what_the_fixture_says() {
        assert_eq!(at(2026, 10, 15, 11, 24, 5), NOW);
    }

    #[wasm_bindgen_test]
    fn shows_the_date_only_off_today() {
        let c = UtcCalendar;
        assert_eq!(format_time_field(NOW - 60.0, NOW, &c), "11:23:05");
        assert_eq!(
            format_time_field(at(2026, 10, 3, 9, 5, 0), NOW, &c),
            "Oct 3 09:05:00"
        );
        assert_eq!(
            format_time_field(at(2019, 5, 20, 23, 0, 1), NOW, &c),
            "May 20 2019 23:00:01"
        );
    }

    #[wasm_bindgen_test]
    fn what_it_shows_reads_back() {
        let c = UtcCalendar;
        for ts in [
            NOW - 60.0,
            at(2026, 10, 3, 9, 5, 0),
            at(2019, 5, 20, 23, 0, 1),
        ] {
            let shown = format_time_field(ts, NOW, &c);
            assert_eq!(parse_time_field(&shown, ts, NOW, &c), Ok(ts), "{shown}");
        }
    }

    #[wasm_bindgen_test]
    fn a_bare_time_is_read_on_the_day_shown() {
        assert_eq!(parse("10:00"), Ok(at(2026, 10, 15, 10, 0, 0)));
        assert_eq!(parse("10:00:30"), Ok(at(2026, 10, 15, 10, 0, 30)));
        assert_eq!(parse("9:15 AM"), Ok(at(2026, 10, 15, 9, 15, 0)));
        // The day the field showed, not today.
        let base = at(2026, 10, 3, 12, 0, 0);
        assert_eq!(
            parse_time_field("08:30", base, NOW, &UtcCalendar),
            Ok(at(2026, 10, 3, 8, 30, 0))
        );
    }

    #[wasm_bindgen_test]
    fn dates_with_and_without_a_year() {
        assert_eq!(parse("Oct 3 11:24"), Ok(at(2026, 10, 3, 11, 24, 0)));
        assert_eq!(parse("oct 3 11:24:05"), Ok(at(2026, 10, 3, 11, 24, 5)));
        assert_eq!(parse("Oct 3"), Ok(at(2026, 10, 3, 0, 0, 0)));
        assert_eq!(parse("May 20 2013 20:00"), Ok(at(2013, 5, 20, 20, 0, 0)));
        assert_eq!(parse("2013-05-20 20:00:00"), Ok(at(2013, 5, 20, 20, 0, 0)));
        assert_eq!(parse("2013-05-20T20:00"), Ok(at(2013, 5, 20, 20, 0, 0)));
        assert_eq!(parse("2013-05-20"), Ok(at(2013, 5, 20, 0, 0, 0)));
    }

    #[wasm_bindgen_test]
    fn absolute_forms_ignore_the_zone() {
        assert_eq!(
            parse("2013-05-20T20:00:00-05:00"),
            Ok(at(2013, 5, 21, 1, 0, 0))
        );
        assert_eq!(parse("1369080000"), Ok(at(2013, 5, 20, 20, 0, 0)));
        assert_eq!(parse("1369080000000"), Ok(at(2013, 5, 20, 20, 0, 0)));
    }

    /// A zone five hours behind UTC, to show the suffix wins over it.
    struct Minus5;
    impl Clock for Minus5 {
        fn hms(&self, ts: f64) -> String {
            UtcCalendar.hms(ts - 18_000.0)
        }
        fn hm(&self, ts: f64) -> String {
            UtcCalendar.hm(ts - 18_000.0)
        }
        fn day(&self, ts: f64) -> (i32, u32, u32) {
            UtcCalendar.day(ts - 18_000.0)
        }
    }
    impl ZoneCalendar for Minus5 {
        fn to_unix(&self, date: NaiveDate, time: NaiveTime) -> Option<f64> {
            UtcCalendar.to_unix(date, time).map(|t| t + 18_000.0)
        }
    }

    #[wasm_bindgen_test]
    fn the_chosen_zone_reads_wall_time_unless_a_suffix_says_utc() {
        let base = NOW - 600.0;
        assert_eq!(
            parse_time_field("06:00", base, NOW, &Minus5),
            Ok(at(2026, 10, 15, 11, 0, 0))
        );
        assert_eq!(
            parse_time_field("06:00Z", base, NOW, &Minus5),
            Ok(at(2026, 10, 15, 6, 0, 0))
        );
        assert_eq!(
            parse_time_field("06:00 UTC", base, NOW, &Minus5),
            Ok(at(2026, 10, 15, 6, 0, 0))
        );
    }

    #[wasm_bindgen_test]
    fn refusals_say_why() {
        assert_eq!(
            parse("tomorrow"),
            Err(TimeFieldError::Unreadable("tomorrow".to_string()))
        );
        assert!(matches!(parse(""), Err(TimeFieldError::Unreadable(_))));
        assert!(matches!(parse("25:00"), Err(TimeFieldError::Unreadable(_))));
        assert_eq!(parse("23:00"), Err(TimeFieldError::Future));
        assert_eq!(parse("1990-01-01"), Err(TimeFieldError::BeforeArchive));
        assert!(TimeFieldError::Future.message().contains("hasn't happened"));
    }
}
