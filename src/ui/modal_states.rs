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
}
