//! Modal for creating, editing, and deleting saved events.
//!
//! Follows the same backdrop + centered window pattern as site_modal and wipe_modal.

use crate::state::AppState;
use chrono::{TimeZone, Utc};
use eframe::egui::{self, Color32, RichText, Vec2};

/// Transient form state for the event modal. Stored on WorkbenchApp.
#[derive(Default)]
pub(crate) struct EventModalState {
    /// Whether the form has been initialized for the current open.
    initialized: bool,
    pub name: String,
    pub site_id: String,
    // Start time fields
    pub start_year: String,
    pub start_month: String,
    pub start_day: String,
    pub start_hour: String,
    pub start_minute: String,
    pub start_second: String,
    // End time fields
    pub end_year: String,
    pub end_month: String,
    pub end_day: String,
    pub end_hour: String,
    pub end_minute: String,
    pub end_second: String,
}

impl EventModalState {
    /// Initialize form fields from a selection range and current site.
    pub(crate) fn init_from_selection(
        &mut self,
        site_id: &str,
        start: f64,
        end: f64,
        use_local: bool,
    ) {
        self.name.clear();
        self.site_id = site_id.to_string();
        (
            self.start_year,
            self.start_month,
            self.start_day,
            self.start_hour,
            self.start_minute,
            self.start_second,
        ) = Self::format_time_fields(start, use_local);
        (
            self.end_year,
            self.end_month,
            self.end_day,
            self.end_hour,
            self.end_minute,
            self.end_second,
        ) = Self::format_time_fields(end, use_local);
    }

    /// Initialize form fields from an existing event for editing.
    pub(crate) fn init_from_event(
        &mut self,
        name: &str,
        site_id: &str,
        start: f64,
        end: f64,
        use_local: bool,
    ) {
        self.name = name.to_string();
        self.site_id = site_id.to_string();
        (
            self.start_year,
            self.start_month,
            self.start_day,
            self.start_hour,
            self.start_minute,
            self.start_second,
        ) = Self::format_time_fields(start, use_local);
        (
            self.end_year,
            self.end_month,
            self.end_day,
            self.end_hour,
            self.end_minute,
            self.end_second,
        ) = Self::format_time_fields(end, use_local);
    }

    /// Convert a timestamp to date/time string fields.
    fn format_time_fields(
        ts: f64,
        use_local: bool,
    ) -> (String, String, String, String, String, String) {
        if use_local {
            let d = js_sys::Date::new_0();
            d.set_time(ts * 1000.0);
            (
                format!("{:04}", d.get_full_year()),
                format!("{:02}", d.get_month() + 1),
                format!("{:02}", d.get_date()),
                format!("{:02}", d.get_hours()),
                format!("{:02}", d.get_minutes()),
                format!("{:02}", d.get_seconds()),
            )
        } else {
            let dt = Utc.timestamp_opt(ts as i64, 0).unwrap();
            (
                dt.format("%Y").to_string(),
                dt.format("%m").to_string(),
                dt.format("%d").to_string(),
                dt.format("%H").to_string(),
                dt.format("%M").to_string(),
                dt.format("%S").to_string(),
            )
        }
    }

    /// Parse start time fields into a UTC timestamp (seconds).
    fn parse_start(&self, use_local: bool) -> Option<f64> {
        Self::parse_time(
            &self.start_year,
            &self.start_month,
            &self.start_day,
            &self.start_hour,
            &self.start_minute,
            &self.start_second,
            use_local,
        )
    }

    /// Parse end time fields into a UTC timestamp (seconds).
    fn parse_end(&self, use_local: bool) -> Option<f64> {
        Self::parse_time(
            &self.end_year,
            &self.end_month,
            &self.end_day,
            &self.end_hour,
            &self.end_minute,
            &self.end_second,
            use_local,
        )
    }

    fn parse_time(
        year: &str,
        month: &str,
        day: &str,
        hour: &str,
        minute: &str,
        second: &str,
        use_local: bool,
    ) -> Option<f64> {
        let y: i32 = year.parse().ok()?;
        let mo: u32 = month.parse().ok()?;
        let d: u32 = day.parse().ok()?;
        let h: u32 = hour.parse().ok()?;
        let mi: u32 = minute.parse().ok()?;
        let s: u32 = second.parse().ok()?;

        if use_local {
            let date = js_sys::Date::new_0();
            date.set_full_year(y as u32);
            date.set_month(mo.checked_sub(1)?);
            date.set_date(d);
            date.set_hours(h);
            date.set_minutes(mi);
            date.set_seconds(s);
            date.set_milliseconds(0);
            let ts = date.get_time();
            if ts.is_nan() {
                return None;
            }
            Some(ts / 1000.0)
        } else {
            let dt = Utc.with_ymd_and_hms(y, mo, d, h, mi, s);
            match dt {
                chrono::LocalResult::Single(dt) => Some(dt.timestamp() as f64),
                _ => None,
            }
        }
    }
}

pub(super) struct EventModalLayer;

impl super::layout::Layer for EventModalLayer {
    fn kind(&self) -> super::layout::LayerKind {
        super::layout::LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        70
    }
    fn visible(&self, ctx: &super::layout::LayoutCtx) -> bool {
        // Also visible while modal_state needs the close-time reset that
        // the body's early-return performs.
        ctx.chrome.event_modal_open || ctx.modals.event.initialized
    }
    fn render(&self, ctx: &mut super::layout::LayoutCtx) {
        draw_event_modal(
            ctx.ctx,
            ctx.state,
            ctx.playback,
            &mut ctx.modals.event,
            ctx.chrome,
        );
    }
}

fn draw_event_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    playback: &crate::subsystem::Playback,
    modal_state: &mut EventModalState,
    chrome: &mut crate::subsystem::Chrome,
) {
    if !chrome.event_modal_open {
        modal_state.initialized = false;
        return;
    }

    // Initialize form fields on first frame after opening
    if !modal_state.initialized {
        modal_state.initialized = true;
        if let Some(editing_id) = chrome.event_modal_editing_id {
            // Editing existing event
            if let Some(event) = state
                .saved_events
                .events
                .iter()
                .find(|e| e.id == editing_id)
            {
                modal_state.init_from_event(
                    &event.name,
                    &event.site_id,
                    event.start_time,
                    event.end_time,
                    state.use_local_time,
                );
            }
        } else {
            // Creating new event — pre-fill from selection range
            let (start, end) = playback.state.selection_range().unwrap_or_else(|| {
                let pos = playback.state.playback_position();
                (pos - 1800.0, pos + 1800.0)
            });
            modal_state.init_from_selection(
                &state.viz_state.site_id,
                start,
                end,
                state.use_local_time,
            );
        }
    }

    if super::modal_helper::modal_backdrop(ctx, "event_modal_backdrop", 160) {
        chrome.event_modal_open = false;
        return;
    }

    let is_editing = chrome.event_modal_editing_id.is_some();
    let title = if is_editing {
        "Edit Event"
    } else {
        "Save Event"
    };

    // Modal window
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(360.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Name input
            ui.horizontal(|ui| {
                ui.label("Name:");
                let response = ui.add(
                    // two-way binding: the egui widget owns this value while
                    // the user edits it (as does every field in the two date
                    // rows below).
                    egui::TextEdit::singleline(&mut modal_state.name)
                        .hint_text("Event name...")
                        .desired_width(260.0),
                );
                if !is_editing {
                    response.request_focus();
                }
            });

            ui.add_space(4.0);

            // Site display
            ui.horizontal(|ui| {
                ui.label("Site:");
                ui.label(RichText::new(&modal_state.site_id).strong());
            });

            ui.add_space(8.0);

            let tz_label = if state.use_local_time { "Local" } else { "UTC" };

            // Start time
            ui.label(RichText::new(format!("Start Time ({tz_label}):")).strong());
            ui.horizontal(|ui| {
                let field_width = 32.0;
                ui.add(egui::TextEdit::singleline(&mut modal_state.start_year).desired_width(40.0));
                ui.label("-");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.start_month)
                        .desired_width(field_width),
                );
                ui.label("-");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.start_day)
                        .desired_width(field_width),
                );
                ui.label(" ");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.start_hour)
                        .desired_width(field_width),
                );
                ui.label(":");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.start_minute)
                        .desired_width(field_width),
                );
                ui.label(":");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.start_second)
                        .desired_width(field_width),
                );
            });

            ui.add_space(4.0);

            // End time
            ui.label(RichText::new(format!("End Time ({tz_label}):")).strong());
            ui.horizontal(|ui| {
                let field_width = 32.0;
                ui.add(egui::TextEdit::singleline(&mut modal_state.end_year).desired_width(40.0));
                ui.label("-");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.end_month)
                        .desired_width(field_width),
                );
                ui.label("-");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.end_day).desired_width(field_width),
                );
                ui.label(" ");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.end_hour)
                        .desired_width(field_width),
                );
                ui.label(":");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.end_minute)
                        .desired_width(field_width),
                );
                ui.label(":");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.end_second)
                        .desired_width(field_width),
                );
            });

            // Validation
            let start_ts = modal_state.parse_start(state.use_local_time);
            let end_ts = modal_state.parse_end(state.use_local_time);
            let name_valid = !modal_state.name.trim().is_empty();
            let times_valid =
                start_ts.is_some() && end_ts.is_some() && start_ts.unwrap() < end_ts.unwrap();
            let can_save = name_valid && times_valid;

            if !times_valid && (start_ts.is_some() || end_ts.is_some()) {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Start time must be before end time")
                        .small()
                        .color(Color32::from_rgb(255, 120, 120)),
                );
            }

            // Enter key submits the form when valid
            let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_save;

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            // Buttons
            ui.horizontal(|ui| {
                // Delete button (only when editing)
                if is_editing {
                    let delete_btn = ui.add(
                        egui::Button::new(RichText::new("Delete").color(Color32::WHITE))
                            .fill(Color32::from_rgb(200, 60, 60)),
                    );
                    if delete_btn.clicked() {
                        if let Some(id) = chrome.event_modal_editing_id {
                            state.saved_events.remove(id);
                        }
                        chrome.event_modal_open = false;
                        chrome.event_modal_editing_id = None;
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let save_btn = ui.add_enabled(can_save, egui::Button::new("Save"));
                    if save_btn.clicked() || enter_pressed {
                        let start = start_ts.unwrap();
                        let end = end_ts.unwrap();
                        let name = modal_state.name.trim().to_string();

                        if let Some(id) = chrome.event_modal_editing_id {
                            state.saved_events.update(id, name, start, end);
                        } else {
                            state
                                .saved_events
                                .add(name, modal_state.site_id.clone(), start, end);
                        }

                        chrome.event_modal_open = false;
                        chrome.event_modal_editing_id = None;
                    }

                    if ui.button("Cancel").clicked() {
                        chrome.event_modal_open = false;
                        chrome.event_modal_editing_id = None;
                    }
                });
            });

            ui.add_space(4.0);
        });
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // 1705321845 == 2024-01-15 12:30:45 UTC
    // 1705327200 == 2024-01-15 14:00:00 UTC
    // 1700000000 == 2023-11-14 22:13:20 UTC

    #[wasm_bindgen_test]
    fn init_from_selection_formats_utc_fields() {
        let mut s = EventModalState::default();
        s.name = "stale".to_string();
        s.init_from_selection("KTLX", 1705321845.0, 1705327200.0, false);

        // name is cleared, site set
        assert!(s.name.is_empty());
        assert!(s.site_id == "KTLX");

        // start = 2024-01-15 12:30:45
        assert!(s.start_year == "2024");
        assert!(s.start_month == "01");
        assert!(s.start_day == "15");
        assert!(s.start_hour == "12");
        assert!(s.start_minute == "30");
        assert!(s.start_second == "45");

        // end = 2024-01-15 14:00:00
        assert!(s.end_year == "2024");
        assert!(s.end_month == "01");
        assert!(s.end_day == "15");
        assert!(s.end_hour == "14");
        assert!(s.end_minute == "00");
        assert!(s.end_second == "00");
    }

    #[wasm_bindgen_test]
    fn init_from_event_sets_name_site_and_fields() {
        let mut s = EventModalState::default();
        s.init_from_event("Tornado", "KOUN", 1700000000.0, 1705321845.0, false);

        assert!(s.name == "Tornado");
        assert!(s.site_id == "KOUN");

        // start = 2023-11-14 22:13:20
        assert!(s.start_year == "2023");
        assert!(s.start_month == "11");
        assert!(s.start_day == "14");
        assert!(s.start_hour == "22");
        assert!(s.start_minute == "13");
        assert!(s.start_second == "20");

        // end = 2024-01-15 12:30:45
        assert!(s.end_year == "2024");
        assert!(s.end_month == "01");
        assert!(s.end_day == "15");
        assert!(s.end_hour == "12");
        assert!(s.end_minute == "30");
        assert!(s.end_second == "45");
    }

    #[wasm_bindgen_test]
    fn parse_start_and_end_roundtrip_utc() {
        let mut s = EventModalState::default();
        s.init_from_event("e", "KXXX", 1705321845.0, 1705327200.0, false);

        let start = s.parse_start(false);
        let end = s.parse_end(false);
        assert!(start.is_some());
        assert!(end.is_some());
        assert!((start.unwrap() - 1705321845.0).abs() < 1.0);
        assert!((end.unwrap() - 1705327200.0).abs() < 1.0);
        // ordering holds for a valid selection
        assert!(start.unwrap() < end.unwrap());
    }

    #[wasm_bindgen_test]
    fn parse_start_handles_raw_fields_utc() {
        let mut s = EventModalState::default();
        s.start_year = "2023".to_string();
        s.start_month = "11".to_string();
        s.start_day = "14".to_string();
        s.start_hour = "22".to_string();
        s.start_minute = "13".to_string();
        s.start_second = "20".to_string();

        let ts = s.parse_start(false);
        assert!(ts.is_some());
        assert!((ts.unwrap() - 1700000000.0).abs() < 1.0);
    }

    #[wasm_bindgen_test]
    fn parse_start_none_on_empty_field() {
        let mut s = EventModalState::default();
        // year left empty -> parse::<i32>() fails -> None
        s.start_month = "01".to_string();
        s.start_day = "15".to_string();
        s.start_hour = "12".to_string();
        s.start_minute = "00".to_string();
        s.start_second = "00".to_string();
        assert!(s.parse_start(false).is_none());
    }

    #[wasm_bindgen_test]
    fn parse_start_none_on_nonnumeric_field() {
        let mut s = EventModalState::default();
        s.start_year = "2024".to_string();
        s.start_month = "abc".to_string();
        s.start_day = "15".to_string();
        s.start_hour = "12".to_string();
        s.start_minute = "00".to_string();
        s.start_second = "00".to_string();
        assert!(s.parse_start(false).is_none());
    }

    #[wasm_bindgen_test]
    fn parse_start_none_on_invalid_calendar_date() {
        let mut s = EventModalState::default();
        // month 13 is out of range -> chrono LocalResult is not Single -> None
        s.start_year = "2024".to_string();
        s.start_month = "13".to_string();
        s.start_day = "15".to_string();
        s.start_hour = "12".to_string();
        s.start_minute = "00".to_string();
        s.start_second = "00".to_string();
        assert!(s.parse_start(false).is_none());
    }
}
