//! Aggregator for the modal UI states that live alongside `AppState`.
//!
//! **Placement rule.** Transient UI state has exactly two homes, split on
//! one question: *is it "what is on screen" or "what has the user typed"?*
//!
//! - **What is on screen** — which panel/modal is open, which tab is active,
//!   what a modal was opened *for* — lives on [`crate::subsystem::Chrome`].
//!   Both the UI (a toggle) and the shell (an effect opening the site modal)
//!   write it, and the layout tree reads it to decide what to render.
//! - **What the user has typed** — search filters, form fields, text buffers
//!   two-way bound to egui widgets — lives here. Nothing outside the widget
//!   that owns it ever reads it.
//!
//! Anything that is neither is domain state and belongs on `AppState` or a
//! subsystem. (`DateTimePickerState` used to sit on `AppState` despite being
//! six text buffers; it moved here in 2026-07, leaving one axis instead of
//! three homes.)
//!
//! [`SiteModalState`], [`EventModalState`], [`MpingModalState`], and
//! [`DateTimePickerState`] each hold transient form state that should not
//! survive a reload. They live
//! outside `AppState` so they don't need `Default + Clone` (one of them
//! owns an `Rc<RefCell<Vec<LocationResult>>>` shared with async
//! callbacks), and so they're not serialized into the URL/preferences
//! roundtrip.
//!
//! Before this aggregator existed, the three states were threaded through
//! `WorkbenchApp` as separate fields and passed individually to render
//! functions. Collecting them into one struct keeps the placement rule
//! consistent and gives a single hook point for cross-cutting concerns
//! (e.g. resetting all modal forms on site change).
//!
//! Render functions still take `&mut SiteModalState` / `&mut
//! EventModalState` / `&mut MpingModalState` directly so each one
//! receives only what it touches — the aggregator exists for ownership
//! and threading, not for hiding the per-modal API.

use super::{EventModalState, MpingModalState, SiteModalState};

/// State for the datetime jump picker popup.
#[derive(Default)]
pub(crate) struct DateTimePickerState {
    /// Whether the picker popup is currently open.
    pub open: bool,
    /// Input values for the picker (as strings for text editing).
    pub year: String,
    pub month: String,
    pub day: String,
    pub hour: String,
    pub minute: String,
    pub second: String,
}

impl DateTimePickerState {
    /// Initialize the picker with a timestamp, respecting the timezone setting.
    pub(crate) fn init_from_timestamp(&mut self, ts: f64, use_local: bool) {
        let p = super::time_format::parts(ts, use_local);
        self.year = format!("{:04}", p.year);
        self.month = format!("{:02}", p.month);
        self.day = format!("{:02}", p.day);
        self.hour = format!("{:02}", p.hour);
        self.minute = format!("{:02}", p.minute);
        self.second = format!("{:02}", p.second);
        self.open = true;
    }

    /// Try to parse the current input values into a UTC timestamp (seconds).
    pub(crate) fn to_timestamp(&self, use_local: bool) -> Option<f64> {
        let year: i32 = self.year.parse().ok()?;
        let month: u32 = self.month.parse().ok()?;
        let day: u32 = self.day.parse().ok()?;
        let hour: u32 = self.hour.parse().ok()?;
        let minute: u32 = self.minute.parse().ok()?;
        let second: u32 = self.second.parse().ok()?;

        if use_local {
            // Construct a JS Date from local components and read back UTC millis
            let d = js_sys::Date::new_0();
            d.set_full_year(year as u32);
            d.set_month(month.checked_sub(1)?); // JS months are 0-based
            d.set_date(day);
            d.set_hours(hour);
            d.set_minutes(minute);
            d.set_seconds(second);
            d.set_milliseconds(0);
            let ts = d.get_time(); // UTC milliseconds
            if ts.is_nan() {
                return None;
            }
            Some(ts / 1000.0)
        } else {
            use chrono::{TimeZone, Utc};
            let dt = Utc.with_ymd_and_hms(year, month, day, hour, minute, second);
            match dt {
                chrono::LocalResult::Single(dt) => Some(dt.timestamp() as f64),
                _ => None,
            }
        }
    }

    /// Close the picker and reset state.
    pub(crate) fn close(&mut self) {
        self.open = false;
    }

    /// Fill every field from a pasted timestamp, returning whether it parsed.
    ///
    /// Accepts RFC-3339 / ISO-8601 with an offset (absolute — converted into
    /// the displayed timezone), naive ISO-8601 to second, minute, or day
    /// precision (taken verbatim in the displayed timezone, which is what the
    /// user sees and means), and bare epoch seconds/milliseconds.
    pub(crate) fn apply_paste(&mut self, text: &str, use_local: bool) -> bool {
        let t = text.trim();
        if t.is_empty() {
            return false;
        }

        // Absolute: carries its own offset, so route through the timezone
        // conversion `init_from_timestamp` already implements.
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
            self.init_from_timestamp(dt.timestamp() as f64, use_local);
            return true;
        }

        // Bare epoch. Guarded on an all-digit run of plausible length so a
        // stray "2026" is treated as a year elsewhere, not as 1970.
        if t.len() >= 10 && t.len() <= 13 && t.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = t.parse::<f64>() {
                let secs = if t.len() > 10 { n / 1000.0 } else { n };
                self.init_from_timestamp(secs, use_local);
                return true;
            }
        }

        // Naive: no zone to honor, so the components ARE what the user wants to
        // see in the fields. Filling them verbatim avoids a pointless
        // round-trip through a timezone the string never specified.
        for fmt in [
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%dT%H:%M",
            "%Y-%m-%d %H:%M",
        ] {
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(t, fmt) {
                self.set_parts(dt.date(), dt.time());
                return true;
            }
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d") {
            self.set_parts(d, chrono::NaiveTime::MIN);
            return true;
        }

        false
    }

    /// Write date/time components into the six buffers.
    fn set_parts(&mut self, date: chrono::NaiveDate, time: chrono::NaiveTime) {
        use chrono::{Datelike, Timelike};
        self.year = format!("{:04}", date.year());
        self.month = format!("{:02}", date.month());
        self.day = format!("{:02}", date.day());
        self.hour = format!("{:02}", time.hour());
        self.minute = format!("{:02}", time.minute());
        self.second = format!("{:02}", time.second());
    }
}

/// One editable component of [`DateTimePickerState`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PickerField {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

impl PickerField {
    /// `(min, max, zero-pad width)` for the field.
    ///
    /// The year floor is the start of the NEXRAD archive era and the ceiling is
    /// far enough out to never obstruct; the rest are calendar/clock ranges.
    /// Day tops out at 31 without consulting the month — the real validity
    /// check is `to_timestamp`, which rejects e.g. Feb 31 and already drives
    /// the "Invalid date/time" warning.
    fn range(self) -> (i64, i64, usize) {
        match self {
            PickerField::Year => (1991, 2100, 4),
            PickerField::Month => (1, 12, 2),
            PickerField::Day => (1, 31, 2),
            PickerField::Hour => (0, 23, 2),
            PickerField::Minute | PickerField::Second => (0, 59, 2),
        }
    }
}

/// Nudge one field buffer by `delta`, clamped to the field's range and
/// re-zero-padded.
///
/// Six free-text boxes make "go back an hour" a retype-and-hope operation;
/// arrow keys make it one keystroke. Clamping rather than wrapping is
/// deliberate: a correct wrap would have to carry into the neighbouring field
/// (23:00 + 1h is *tomorrow*), and a wrap that silently doesn't carry lands the
/// user a day off without telling them.
///
/// A free function so the widget layer can drive a single borrowed buffer
/// without re-borrowing the whole picker. Unparseable buffers are left alone
/// rather than snapped to an arbitrary value.
pub(crate) fn nudge_buf(buf: &mut String, field: PickerField, delta: i64) {
    let (lo, hi, width) = field.range();
    let Ok(current) = buf.trim().parse::<i64>() else {
        return;
    };
    let next = (current + delta).clamp(lo, hi);
    *buf = format!("{:0width$}", next, width = width);
}

/// Whether a pasted string looks like a whole timestamp rather than a single
/// field's digits.
///
/// The picker intercepts whole-timestamp pastes before the field editors see
/// them; this keeps pasting "07" into the month box working normally.
pub(crate) fn looks_like_timestamp(s: &str) -> bool {
    let t = s.trim();
    t.contains('-') || t.contains(':') || (t.len() >= 10 && t.bytes().all(|b| b.is_ascii_digit()))
}

/// Owns all transient modal UI state held outside [`crate::state::AppState`].
pub(crate) struct ModalStates {
    /// Site-selection modal: search filter, async location queue, and
    /// the welcome/list/zip view mode.
    pub site: SiteModalState,
    /// Event create/edit modal: form fields for name, site, time range.
    pub event: EventModalState,
    /// mPING settings modal: in-flight API-key text entry.
    pub mping: MpingModalState,
    /// Datetime jump picker popup: open flag + the six field buffers.
    pub datetime: DateTimePickerState,
}

impl ModalStates {
    /// Construct a fresh set of modal states.
    ///
    /// `has_preferred_site` controls the site modal's first-visit
    /// welcome verbiage — `true` for returning users (shorter "change
    /// site" heading), `false` for the first visit.
    pub(crate) fn new(has_preferred_site: bool) -> Self {
        let mut site = SiteModalState::default();
        if has_preferred_site {
            site.is_first_visit = false;
        }
        Self {
            site,
            event: EventModalState::default(),
            mping: MpingModalState::default(),
            datetime: DateTimePickerState::default(),
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn new_with_preferred_site_marks_not_first_visit() {
        let states = ModalStates::new(true);
        assert!(!states.site.is_first_visit);
    }

    #[wasm_bindgen_test]
    fn new_without_preferred_site_keeps_first_visit() {
        let states = ModalStates::new(false);
        assert!(states.site.is_first_visit);
    }

    #[wasm_bindgen_test]
    fn new_site_mode_defaults_to_welcome() {
        // SiteModalMode lacks Debug; compare via PartialEq through assert!.
        let states = ModalStates::new(false);
        assert!(states.site.mode == crate::ui::site_modal::SiteModalMode::Welcome);
        let states2 = ModalStates::new(true);
        assert!(states2.site.mode == crate::ui::site_modal::SiteModalMode::Welcome);
    }

    #[wasm_bindgen_test]
    fn new_site_form_fields_start_empty() {
        let states = ModalStates::new(true);
        assert_eq!(states.site.filter, "");
        assert_eq!(states.site.zip_input, "");
        assert!(states.site.error_message.is_none());
    }

    #[wasm_bindgen_test]
    fn new_event_modal_starts_with_empty_name() {
        let states = ModalStates::new(false);
        assert_eq!(states.event.name, "");
        assert_eq!(states.event.site_id, "");
    }

    #[wasm_bindgen_test]
    fn new_mping_modal_starts_with_empty_key_input() {
        let states = ModalStates::new(true);
        assert_eq!(states.mping.key_input, "");
    }
}

#[cfg(test)]
mod datetime_picker_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn datetime_picker_default_is_closed_and_empty() {
        let p = DateTimePickerState::default();
        assert!(!p.open);
        assert!(p.year.is_empty());
        assert!(p.month.is_empty());
        assert!(p.day.is_empty());
        assert!(p.hour.is_empty());
        assert!(p.minute.is_empty());
        assert!(p.second.is_empty());
    }

    #[wasm_bindgen_test]
    fn datetime_picker_close_clears_open() {
        let mut p = DateTimePickerState {
            open: true,
            ..Default::default()
        };
        p.close();
        assert!(!p.open);
    }

    #[wasm_bindgen_test]
    fn datetime_picker_to_timestamp_utc_epoch() {
        let p = DateTimePickerState {
            year: "1970".to_string(),
            month: "01".to_string(),
            day: "01".to_string(),
            hour: "00".to_string(),
            minute: "00".to_string(),
            second: "00".to_string(),
            ..Default::default()
        };
        let ts = p.to_timestamp(false).expect("valid utc datetime");
        assert!((ts - 0.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn datetime_picker_to_timestamp_utc_known_value() {
        // 2021-01-01 00:00:00 UTC == 1609459200 seconds since epoch.
        let p = DateTimePickerState {
            year: "2021".to_string(),
            month: "1".to_string(),
            day: "1".to_string(),
            hour: "0".to_string(),
            minute: "0".to_string(),
            second: "0".to_string(),
            ..Default::default()
        };
        let ts = p.to_timestamp(false).expect("valid utc datetime");
        assert!((ts - 1_609_459_200.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn datetime_picker_to_timestamp_utc_rejects_unparseable() {
        // Default (all-empty) inputs cannot parse → None.
        let p = DateTimePickerState::default();
        assert!(p.to_timestamp(false).is_none());

        // Garbage month also fails to parse.
        let bad = DateTimePickerState {
            year: "2021".to_string(),
            month: "abc".to_string(),
            day: "1".to_string(),
            hour: "0".to_string(),
            minute: "0".to_string(),
            second: "0".to_string(),
            ..Default::default()
        };
        assert!(bad.to_timestamp(false).is_none());
    }

    #[wasm_bindgen_test]
    fn datetime_picker_to_timestamp_utc_rejects_invalid_date() {
        // Month 13 is out of range → chrono returns non-Single → None.
        let p = DateTimePickerState {
            year: "2021".to_string(),
            month: "13".to_string(),
            day: "1".to_string(),
            hour: "0".to_string(),
            minute: "0".to_string(),
            second: "0".to_string(),
            ..Default::default()
        };
        assert!(p.to_timestamp(false).is_none());
    }

    // ---- nudge_buf --------------------------------------------------------

    #[wasm_bindgen_test]
    fn nudge_steps_and_keeps_zero_padding() {
        let mut b = "07".to_string();
        nudge_buf(&mut b, PickerField::Month, 1);
        assert_eq!(b, "08");
        nudge_buf(&mut b, PickerField::Month, -2);
        assert_eq!(b, "06");
        let mut y = "2026".to_string();
        nudge_buf(&mut y, PickerField::Year, -1);
        assert_eq!(y, "2025");
    }

    #[wasm_bindgen_test]
    fn nudge_clamps_instead_of_wrapping() {
        // Wrapping 23:00 + 1h to 00:00 without carrying into the day would put
        // the user a day off with no indication.
        let mut h = "23".to_string();
        nudge_buf(&mut h, PickerField::Hour, 1);
        assert_eq!(h, "23");
        let mut m = "00".to_string();
        nudge_buf(&mut m, PickerField::Minute, -1);
        assert_eq!(m, "00");
        let mut mo = "12".to_string();
        nudge_buf(&mut mo, PickerField::Month, 5);
        assert_eq!(mo, "12");
    }

    #[wasm_bindgen_test]
    fn nudge_leaves_unparseable_buffers_alone() {
        // Mid-edit garbage must not be silently rewritten under the cursor.
        let mut b = "".to_string();
        nudge_buf(&mut b, PickerField::Hour, 1);
        assert_eq!(b, "");
        let mut j = "ab".to_string();
        nudge_buf(&mut j, PickerField::Hour, 1);
        assert_eq!(j, "ab");
    }

    #[wasm_bindgen_test]
    fn year_floor_is_the_archive_era() {
        let mut y = "1991".to_string();
        nudge_buf(&mut y, PickerField::Year, -1);
        assert_eq!(y, "1991");
    }

    // ---- apply_paste ------------------------------------------------------

    #[wasm_bindgen_test]
    fn paste_naive_iso_fills_fields_verbatim() {
        // No zone in the string → the components are exactly what the user
        // means, whichever display timezone is active.
        for use_local in [false, true] {
            let mut p = DateTimePickerState::default();
            assert!(p.apply_paste("2026-07-31T14:30:05", use_local));
            assert_eq!(
                (
                    p.year.as_str(),
                    p.month.as_str(),
                    p.day.as_str(),
                    p.hour.as_str(),
                    p.minute.as_str(),
                    p.second.as_str()
                ),
                ("2026", "07", "31", "14", "30", "05")
            );
        }
    }

    #[wasm_bindgen_test]
    fn paste_accepts_space_separator_and_minute_precision() {
        let mut p = DateTimePickerState::default();
        assert!(p.apply_paste("2026-07-31 14:30", false));
        assert_eq!(p.hour, "14");
        assert_eq!(p.minute, "30");
        assert_eq!(p.second, "00");
    }

    #[wasm_bindgen_test]
    fn paste_date_only_lands_at_midnight() {
        let mut p = DateTimePickerState::default();
        assert!(p.apply_paste("2026-07-31", false));
        assert_eq!(p.day, "31");
        assert_eq!(p.hour, "00");
        assert_eq!(p.second, "00");
    }

    #[wasm_bindgen_test]
    fn paste_rfc3339_with_offset_is_converted_not_copied() {
        // 14:30 at +02:00 is 12:30 UTC — an offset-carrying string must be
        // converted, not have its wall-clock digits copied across.
        let mut p = DateTimePickerState::default();
        assert!(p.apply_paste("2026-07-31T14:30:00+02:00", false));
        assert_eq!(p.hour, "12");
        assert_eq!(p.minute, "30");
    }

    #[wasm_bindgen_test]
    fn paste_epoch_seconds_and_millis() {
        // 1_774_000_000 = 2026-03-20T05:46:40Z
        let mut secs = DateTimePickerState::default();
        assert!(secs.apply_paste("1774000000", false));
        assert_eq!(secs.year, "2026");
        let mut millis = DateTimePickerState::default();
        assert!(millis.apply_paste("1774000000000", false));
        assert_eq!(millis.year, "2026");
        assert_eq!(millis.month, secs.month);
        assert_eq!(millis.day, secs.day);
    }

    #[wasm_bindgen_test]
    fn paste_rejects_garbage_without_touching_the_fields() {
        let mut p = DateTimePickerState::default();
        p.year = "2020".to_string();
        assert!(!p.apply_paste("not a date", false));
        assert!(!p.apply_paste("", false));
        assert_eq!(p.year, "2020");
    }

    #[wasm_bindgen_test]
    fn a_bare_year_is_not_mistaken_for_an_epoch() {
        // "2026" must not become 1970-01-01T00:33:46 — the digit-run guard
        // requires at least 10 digits.
        let mut p = DateTimePickerState::default();
        assert!(!p.apply_paste("2026", false));
    }

    // ---- looks_like_timestamp --------------------------------------------

    #[wasm_bindgen_test]
    fn whole_timestamps_are_intercepted_single_fields_are_not() {
        assert!(looks_like_timestamp("2026-07-31T14:30:00Z"));
        assert!(looks_like_timestamp("2026-07-31"));
        assert!(looks_like_timestamp("14:30"));
        assert!(looks_like_timestamp("1774000000"));
        // A single field's digits must still paste into that field normally.
        assert!(!looks_like_timestamp("07"));
        assert!(!looks_like_timestamp("2026"));
        assert!(!looks_like_timestamp(""));
    }
}
