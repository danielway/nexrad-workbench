//! Modal for creating, editing, and deleting saved events.
//!
//! Follows the same backdrop + centered window pattern as site_modal and wipe_modal.

use crate::state::AppState;
use chrono::{TimeZone, Utc};
use eframe::egui::{self, Color32, RichText, Vec2};

/// Transient form state for the event modal. Stored on WorkbenchApp.
#[derive(Default)]
pub struct EventModalState {
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
    pub fn init_from_selection(&mut self, site_id: &str, start: f64, end: f64, use_local: bool) {
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
    pub fn init_from_event(
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

/// Render the event create/edit modal if open.
pub fn render_event_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    modal_state: &mut EventModalState,
) {
    if !state.event_modal_open {
        modal_state.initialized = false;
        return;
    }

    // Initialize form fields on first frame after opening
    if !modal_state.initialized {
        modal_state.initialized = true;
        if let Some(editing_id) = state.event_modal_editing_id {
            // Editing existing event
            if let Some(event) = state.saved_events.events.iter().find(|e| e.id == editing_id) {
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
            let (start, end) = state
                .playback_state
                .selection_range()
                .unwrap_or_else(|| {
                    let pos = state.playback_state.playback_position();
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

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.event_modal_open = false;
        return;
    }

    let is_editing = state.event_modal_editing_id.is_some();
    let title = if is_editing {
        "Edit Event"
    } else {
        "Save Event"
    };

    // Semi-transparent backdrop
    egui::Area::new(egui::Id::new("event_modal_backdrop"))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(
                screen_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 160),
            );
            if response.clicked() {
                state.event_modal_open = false;
            }
        });

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

            let tz_label = if state.use_local_time {
                "Local"
            } else {
                "UTC"
            };

            // Start time
            ui.label(RichText::new(format!("Start Time ({tz_label}):")).strong());
            ui.horizontal(|ui| {
                let field_width = 32.0;
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.start_year).desired_width(40.0),
                );
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
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.end_year).desired_width(40.0),
                );
                ui.label("-");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.end_month)
                        .desired_width(field_width),
                );
                ui.label("-");
                ui.add(
                    egui::TextEdit::singleline(&mut modal_state.end_day)
                        .desired_width(field_width),
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
                        if let Some(id) = state.event_modal_editing_id {
                            state.saved_events.remove(id);
                        }
                        state.event_modal_open = false;
                        state.event_modal_editing_id = None;
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let save_btn = ui.add_enabled(can_save, egui::Button::new("Save"));
                    if save_btn.clicked() {
                        let start = start_ts.unwrap();
                        let end = end_ts.unwrap();
                        let name = modal_state.name.trim().to_string();

                        if let Some(id) = state.event_modal_editing_id {
                            state.saved_events.update(id, name, start, end);
                        } else {
                            state.saved_events.add(
                                name,
                                modal_state.site_id.clone(),
                                start,
                                end,
                            );
                        }

                        state.event_modal_open = false;
                        state.event_modal_editing_id = None;
                    }

                    if ui.button("Cancel").clicked() {
                        state.event_modal_open = false;
                        state.event_modal_editing_id = None;
                    }
                });
            });

            ui.add_space(4.0);
        });
}
