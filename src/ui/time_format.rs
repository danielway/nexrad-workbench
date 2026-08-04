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
//! Format choices:
//! - Local: friendly 12-hour "2:41:07 PM CDT" via the browser's
//!   `Date.toLocaleTimeString` with `timeZoneName: 'short'`. The browser owns
//!   DST and the abbreviation, so we never carry a tz database.
//! - UTC: the same 12-hour shape "2:41:07 PM UTC" via chrono (deterministic,
//!   testable in node where the local zone is itself UTC).
//!
//! The `Compaction` ladder lets a narrow readout drop seconds, then the zone
//! suffix, rather than dropping the time entirely — the time is primary.

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

/// How aggressively to compact a primary clock readout. Higher tiers shed
/// detail (seconds, then the zone suffix) so the time survives at narrow widths
/// instead of being demoted out of the bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Compaction {
    /// Full: hours:minutes:seconds + zone, e.g. "2:41:07 PM CDT".
    Full,
    /// Drop seconds, keep the zone: "2:41 PM CDT".
    NoSeconds,
    /// Drop seconds and the zone: "2:41 PM".
    Bare,
}

/// Format `ts` (Unix seconds, sub-second ok) as a friendly 12-hour clock,
/// honoring `use_local` and the requested `compaction`.
///
/// Local renders through the browser (`toLocaleTimeString`) so the zone
/// abbreviation reflects the user's actual locale and DST. UTC renders through
/// chrono with a fixed "UTC" suffix so it is deterministic (and unit-testable).
pub(crate) fn format_clock_12h(ts: f64, use_local: bool, compaction: Compaction) -> String {
    if use_local {
        format_local_12h(ts, compaction)
    } else {
        format_utc_12h(ts, compaction)
    }
}

/// Browser-formatted local 12-hour clock. Falls back to the UTC formatter if
/// the locale call fails for any reason (it shouldn't in a browser, but the
/// readout must never blank).
fn format_local_12h(ts: f64, compaction: Compaction) -> String {
    use wasm_bindgen::JsValue;

    let date = js_sys::Date::new(&JsValue::from_f64(ts * 1000.0));
    let opts = js_sys::Object::new();
    let set = |k: &str, v: &JsValue| {
        let _ = js_sys::Reflect::set(&opts, &JsValue::from_str(k), v);
    };
    set("hour", &JsValue::from_str("numeric"));
    set("minute", &JsValue::from_str("2-digit"));
    if compaction == Compaction::Full {
        set("second", &JsValue::from_str("2-digit"));
    }
    set("hour12", &JsValue::from_bool(true));
    if compaction != Compaction::Bare {
        // "short" yields the locale abbreviation, e.g. "CDT" / "PST".
        set("timeZoneName", &JsValue::from_str("short"));
    }

    let s: String = js_sys::Date::to_locale_time_string_with_options(&date, "en-US", &opts).into();
    if s.is_empty() {
        // Locale formatting unavailable — fall back so the readout still shows
        // a real time rather than an empty string.
        return format_utc_12h(ts, compaction);
    }
    s
}

/// chrono-based UTC 12-hour clock. Deterministic; the basis for the unit tests.
fn format_utc_12h(ts: f64, compaction: Compaction) -> String {
    use chrono::{TimeZone, Timelike, Utc};

    let secs = ts.floor() as i64;
    let dt = match Utc.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt,
        _ => return format!("{ts:.0}"),
    };
    let hour24 = dt.hour();
    let (hour12, meridiem) = to_12h(hour24);
    let body = match compaction {
        Compaction::Full => format!(
            "{}:{:02}:{:02} {}",
            hour12,
            dt.minute(),
            dt.second(),
            meridiem
        ),
        _ => format!("{}:{:02} {}", hour12, dt.minute(), meridiem),
    };
    if compaction == Compaction::Bare {
        body
    } else {
        format!("{body} UTC")
    }
}

/// Convert a 0..=23 hour to a (1..=12, "AM"/"PM") pair.
fn to_12h(hour24: u32) -> (u32, &'static str) {
    let meridiem = if hour24 < 12 { "AM" } else { "PM" };
    let hour12 = match hour24 % 12 {
        0 => 12,
        h => h,
    };
    (hour12, meridiem)
}

/// Plain-language data-age phrasing for the live edge (spec §5/§11.3):
/// "updated just now", "updated 1m ago", "updated 2h ago". `age_secs` is
/// wall-clock now minus the displayed frame's collection time. Negative ages
/// (clock skew / a frame stamped slightly in the future) read as "just now".
pub(crate) fn format_updated_ago(age_secs: f64) -> String {
    if age_secs < 45.0 {
        "updated just now".to_string()
    } else if age_secs < 3600.0 {
        let m = (age_secs / 60.0).round().max(1.0) as u32;
        format!("updated {m}m ago")
    } else if age_secs < 86_400.0 {
        let h = (age_secs / 3600.0).floor() as u32;
        format!("updated {h}h ago")
    } else {
        let d = (age_secs / 86_400.0).floor() as u32;
        format!("updated {d}d ago")
    }
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
    fn utc_full_is_12hour_with_seconds_and_zone() {
        assert_eq!(
            format_clock_12h(TS, false, Compaction::Full),
            "7:41:07 PM UTC"
        );
    }

    #[wasm_bindgen_test]
    fn utc_no_seconds_drops_seconds_keeps_zone() {
        assert_eq!(
            format_clock_12h(TS, false, Compaction::NoSeconds),
            "7:41 PM UTC"
        );
    }

    #[wasm_bindgen_test]
    fn utc_bare_drops_seconds_and_zone() {
        assert_eq!(format_clock_12h(TS, false, Compaction::Bare), "7:41 PM");
    }

    #[wasm_bindgen_test]
    fn noon_and_midnight_render_as_12() {
        // 2026-06-12 12:00:00 UTC → noon.
        let noon = 1_780_660_800.0;
        assert_eq!(format_clock_12h(noon, false, Compaction::Bare), "12:00 PM");
        // 2026-06-12 00:00:00 UTC → midnight.
        let midnight = 1_780_617_600.0;
        assert_eq!(
            format_clock_12h(midnight, false, Compaction::Bare),
            "12:00 AM"
        );
    }

    #[wasm_bindgen_test]
    fn local_format_is_nonempty_and_12hour_shaped() {
        // The browser path must yield a non-empty 12-hour string (AM/PM). The
        // exact zone abbreviation depends on the host locale (UTC in node), so
        // we only assert the shape, not the zone text.
        let s = format_clock_12h(TS, true, Compaction::Full);
        assert!(!s.is_empty());
        assert!(s.contains("AM") || s.contains("PM"), "got {s}");
    }

    #[wasm_bindgen_test]
    fn updated_ago_buckets() {
        assert_eq!(format_updated_ago(0.0), "updated just now");
        assert_eq!(format_updated_ago(20.0), "updated just now");
        // Just under the 45s threshold still reads as "just now".
        assert_eq!(format_updated_ago(44.0), "updated just now");
        assert_eq!(format_updated_ago(60.0), "updated 1m ago");
        assert_eq!(format_updated_ago(90.0), "updated 2m ago");
        assert_eq!(format_updated_ago(3600.0), "updated 1h ago");
        assert_eq!(format_updated_ago(7200.0), "updated 2h ago");
        assert_eq!(format_updated_ago(90_000.0), "updated 1d ago");
    }

    #[wasm_bindgen_test]
    fn updated_ago_negative_reads_as_just_now() {
        assert_eq!(format_updated_ago(-5.0), "updated just now");
    }
}
