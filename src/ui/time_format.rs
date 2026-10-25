//! Shared render-time clock formatting (spec §5 top readouts, §11.4 local/UTC).
//!
//! The displayed-frame timestamp is the product's PRIMARY readout, and the
//! local/UTC choice must flip live the instant the user taps it — so time is
//! formatted *at render time* from raw Unix seconds, never baked into a stored
//! `String` at decode time. Every readout that shows a clock (top-bar primary,
//! canvas overlay, transport, inspector, timeline ruler, mPING detail) funnels
//! through these helpers so a single preference flip reformats them all in one
//! frame.
//!
//! Every clock is 24-hour ("19:41:07") in the chosen zone. Local time goes
//! through the browser, so it owns DST and we never carry a tz database; UTC
//! goes through chrono so it stays deterministic under test. The top bar names
//! the zone once ([`zone_label`]) and every other clock inherits it.

/// A timestamp broken into calendar + clock components in one zone.
///
/// The zone branch (`js_sys::Date` for local, chrono for UTC) used to be
/// re-implemented at every site that wanted a 24-hour or ISO-ish readout —
/// canvas overlays, the alerts modal, the saved-events list. Extracting the
/// split leaves those sites as a single `format!` over these fields.
pub(crate) struct TimeParts {
    pub year: i32,
    /// 1-based, unlike `Date::get_month`.
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

/// Split `ts` (Unix seconds, sub-second ok) into components, in the user's
/// local zone or UTC. Local goes through the browser so DST is the browser's
/// problem; UTC goes through chrono so it stays deterministic under test.
pub(crate) fn parts(ts: f64, use_local: bool) -> TimeParts {
    if use_local {
        let d = js_sys::Date::new_0();
        d.set_time(ts * 1000.0);
        TimeParts {
            year: d.get_full_year() as i32,
            month: d.get_month() + 1,
            day: d.get_date(),
            hour: d.get_hours(),
            minute: d.get_minutes(),
            second: d.get_seconds(),
            millis: d.get_milliseconds(),
        }
    } else {
        use chrono::{Datelike, TimeZone, Timelike, Utc};
        let secs = ts.floor() as i64;
        let millis = ((ts - ts.floor()) * 1000.0).round() as u32;
        // A timestamp outside chrono's range can't be rendered; fall back to
        // the epoch rather than blanking a readout.
        let dt = Utc
            .timestamp_opt(secs, 0)
            .single()
            .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());
        TimeParts {
            year: dt.year(),
            month: dt.month(),
            day: dt.day(),
            hour: dt.hour(),
            minute: dt.minute(),
            second: dt.second(),
            millis,
        }
    }
}

/// The session view-model's [`crate::core::session::Clock`]: 24-hour clocks
/// in the user's chosen zone, so core-owned words (the selection chip, the
/// canvas caption) print times without knowing the zone.
pub(crate) struct ZoneClock {
    pub use_local: bool,
}

impl crate::core::session::Clock for ZoneClock {
    fn hms(&self, ts: f64) -> String {
        let p = parts(ts, self.use_local);
        format!("{:02}:{:02}:{:02}", p.hour, p.minute, p.second)
    }

    fn hm(&self, ts: f64) -> String {
        let p = parts(ts, self.use_local);
        format!("{:02}:{:02}", p.hour, p.minute)
    }

    fn day(&self, ts: f64) -> (i32, u32, u32) {
        let p = parts(ts, self.use_local);
        (p.year, p.month, p.day)
    }
}

/// The calendar sheet's [`crate::core::calendar::Zone`]: the chosen zone's
/// offset east of UTC at a moment, read from the browser (0 in UTC).
impl crate::core::calendar::Zone for ZoneClock {
    fn offset_secs(&self, ts: f64) -> i64 {
        if !self.use_local {
            return 0;
        }
        // getTimezoneOffset() is minutes positive-west.
        let d = js_sys::Date::new_0();
        d.set_time(ts * 1000.0);
        -(d.get_timezone_offset() as i64) * 60
    }
}

/// The chosen zone's abbreviation at `ts`: "UTC", or the browser's short
/// local name ("CDT", "GMT+2"). The top bar shows it once, after the frame
/// time (one-instrument §4); every other clock inherits it.
pub(crate) fn zone_label(ts: f64, use_local: bool) -> String {
    use wasm_bindgen::JsValue;

    if !use_local {
        return "UTC".to_string();
    }
    let date = js_sys::Date::new(&JsValue::from_f64(ts * 1000.0));
    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &opts,
        &JsValue::from_str("timeZoneName"),
        &JsValue::from_str("short"),
    );
    let s: String = js_sys::Date::to_locale_time_string_with_options(&date, "en-US", &opts).into();
    // "11:21:57 AM CDT" → "CDT".
    s.rsplit(' ')
        .next()
        .filter(|z| !z.is_empty() && !z.contains(':'))
        .unwrap_or("local")
        .to_string()
}

impl crate::core::time_field::ZoneCalendar for ZoneClock {
    fn to_unix(&self, date: chrono::NaiveDate, time: chrono::NaiveTime) -> Option<f64> {
        use chrono::{Datelike, Timelike};
        if !self.use_local {
            return crate::core::time_field::UtcCalendar.to_unix(date, time);
        }
        // The browser owns the local zone and its DST rules.
        let d = js_sys::Date::new_with_year_month_day_hr_min_sec(
            date.year() as u32,
            date.month0() as i32,
            date.day() as i32,
            time.hour() as i32,
            time.minute() as i32,
            time.second() as i32,
        );
        let ms = d.get_time();
        (!ms.is_nan()).then_some(ms / 1000.0)
    }
}

/// Canvas stamp for the national mosaic product valid time.
///
/// `ts` is the MRMS product valid time (unix seconds) from the WMS `time`
/// dimension, not the client fetch clock. Returns `None` when there is no
/// image yet so the caller can skip painting. Like every other clock it is
/// 24-hour in the chosen zone, and the top bar names the zone once.
pub(crate) fn format_mosaic_stamp(ts: Option<f64>, use_local: bool) -> Option<String> {
    use crate::core::session::Clock as _;
    let clock = ZoneClock { use_local }.hms(ts?);
    Some(format!("Mosaic \u{00B7} {clock}"))
}

#[cfg(test)]
mod parts_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Node runs with TZ=UTC, so the local and UTC branches agree there; these
    // pin the UTC branch's arithmetic (the deterministic half).
    #[wasm_bindgen_test]
    fn utc_parts_split_a_known_instant() {
        // 2023-11-14T22:13:20.000Z
        let p = parts(1_700_000_000.0, false);
        assert_eq!((p.year, p.month, p.day), (2023, 11, 14));
        assert_eq!((p.hour, p.minute, p.second), (22, 13, 20));
        assert_eq!(p.millis, 0);
    }

    #[wasm_bindgen_test]
    fn utc_parts_carry_sub_second_millis() {
        let p = parts(1_700_000_000.25, false);
        assert_eq!(p.second, 20);
        assert_eq!(p.millis, 250);
    }

    #[wasm_bindgen_test]
    fn month_is_one_based() {
        // Epoch is January — 1, not the JS Date convention of 0.
        assert_eq!(parts(0.0, false).month, 1);
    }

    #[wasm_bindgen_test]
    fn pre_epoch_timestamps_still_split() {
        // Negative seconds are valid; the readout must not blank.
        let p = parts(-1.0, false);
        assert_eq!(p.year, 1969);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // 2026-06-12 19:41:07 UTC.
    const TS: f64 = 1_780_688_467.0;

    #[wasm_bindgen_test]
    fn mosaic_stamp_none_when_no_image() {
        assert_eq!(format_mosaic_stamp(None, false), None);
    }

    #[wasm_bindgen_test]
    fn mosaic_stamp_is_a_24_hour_clock() {
        assert_eq!(
            format_mosaic_stamp(Some(TS), false).as_deref(),
            Some("Mosaic \u{00B7} 19:41:07")
        );
    }
}
