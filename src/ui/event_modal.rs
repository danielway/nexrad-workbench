//! The event dialog: create or edit a saved event (§12).
//!
//! Its start and end are the transport's time field ([`super::time_field`]):
//! the same clock you click to type into, with the same accepted forms. No
//! field steals focus — the name field is focused once when a new event's
//! dialog opens, never again. Saving, deleting and validation are the core's
//! ([`crate::core::dialogs::DialogIntent::SaveEvent`],
//! [`crate::core::dialogs::validate_event`]); deleting goes through the
//! destructive confirmation.

use super::dialog::{self, Dialog, Width};
use super::time_field::{time_field, TimeFieldLook};
use crate::core::dialogs::{validate_event, Confirm, DialogId, DialogIntent};
use crate::core::Intent;
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, RichText};

/// Transient form state for the event dialog.
#[derive(Default)]
pub(crate) struct EventModalState {
    /// The dialog opening (stack generation) the form was filled for.
    filled_for: Option<u64>,
    pub name: String,
    pub site_id: String,
    /// Start, Unix seconds.
    pub start: f64,
    /// End, Unix seconds.
    pub end: f64,
}

impl EventModalState {
    /// Fill the form for a new event over a selection range.
    pub(crate) fn init_from_selection(&mut self, site_id: &str, start: f64, end: f64) {
        self.name.clear();
        self.site_id = site_id.to_string();
        self.start = start;
        self.end = end;
    }

    /// Fill the form from an existing event.
    pub(crate) fn init_from_event(&mut self, name: &str, site_id: &str, start: f64, end: f64) {
        self.name = name.to_string();
        self.site_id = site_id.to_string();
        self.start = start;
        self.end = end;
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
        ctx.chrome.event_modal_open
    }
    fn render(&self, ctx: &mut super::layout::LayoutCtx) {
        draw_event_modal(
            ctx.ctx,
            ctx.state,
            &ctx.sites.focus().id.0,
            ctx.playback,
            &mut ctx.modals.event,
            ctx.chrome,
        );
    }
}

fn draw_event_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    site_id: &str,
    playback: &crate::subsystem::Playback,
    form: &mut EventModalState,
    chrome: &crate::subsystem::Chrome,
) {
    let editing = chrome.event_modal_editing_id;
    let generation = chrome.dialogs.generation(DialogId::Event);
    let fresh = generation.is_some() && form.filled_for != generation;
    if fresh {
        form.filled_for = generation;
        let existing = editing.and_then(|id| state.saved_events.events.iter().find(|e| e.id == id));
        match existing {
            Some(e) => form.init_from_event(&e.name, &e.site_id, e.start_time, e.end_time),
            None => {
                let (start, end) = crate::core::dialogs::event_prefill(
                    playback.state.selection_range(),
                    playback.state.playback_position(),
                );
                form.init_from_selection(site_id, start, end);
            }
        }
    }

    let use_local = state.use_local_time;
    let now = state.frame_now.secs();
    let zone = if use_local { "local time" } else { "UTC" };
    let title = if editing.is_some() {
        "Edit event"
    } else {
        "Save event"
    };
    let look = TimeFieldLook {
        color: theme::TEXT,
        size: theme::READOUT,
        hover: "Click to type a time",
    };

    let mut save = false;
    let mut cancel = false;
    let mut delete = false;
    let out = Dialog::new(DialogId::Event, title)
        .subtitle(format!("{} · times in {zone}", form.site_id))
        .width(Width::Narrow)
        .show(ctx, |ui| {
            dialog::group_heading(ui, "Name");
            let name = ui.add(
                // two-way binding: the egui widget owns this value while the
                // user edits it.
                egui::TextEdit::singleline(&mut form.name)
                    .hint_text("Event name")
                    .desired_width(f32::INFINITY),
            );
            // Focus once, when a new event's dialog opens.
            if fresh && editing.is_none() {
                name.request_focus();
            }
            let enter_in_name = name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            ui.add_space(8.0);
            egui::Grid::new("event_times")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Start").color(theme::TEXT_DIM));
                    if let Some(t) =
                        time_field(ui, "event_start", form.start, now, use_local, &look)
                    {
                        form.start = t;
                    }
                    ui.end_row();
                    ui.label(RichText::new("End").color(theme::TEXT_DIM));
                    if let Some(t) = time_field(ui, "event_end", form.end, now, use_local, &look) {
                        form.end = t;
                    }
                    ui.end_row();
                });

            let valid = validate_event(&form.name, form.start, form.end);
            // Only the time problem is worth a line; an empty name just
            // disables Save (with its reason on hover).
            if let Err(msg) = valid {
                if !form.name.trim().is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("{} {msg}", crate::ui::icons::WARNING))
                            .size(theme::CAPTION)
                            .color(theme::TEXT),
                    );
                }
            }
            save = enter_in_name && valid.is_ok();

            dialog::footer(ui, |ui| {
                let reason = valid.err().unwrap_or("");
                save |= dialog::primary_enabled(ui, valid.is_ok(), "Save", reason).clicked();
                cancel = dialog::secondary(ui, "Cancel").clicked();
                if editing.is_some() {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        delete = dialog::destructive(ui, "Delete").clicked();
                    });
                }
            });
        });

    if save {
        state.push_command(Intent::Dialog(DialogIntent::SaveEvent {
            editing,
            name: form.name.clone(),
            site_id: form.site_id.clone(),
            start: form.start,
            end: form.end,
        }));
    } else if delete {
        if let Some(id) = editing {
            state.push_command(Intent::Dialog(DialogIntent::RequestConfirm(
                Confirm::DeleteEvent(id),
            )));
        }
    } else if cancel || out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::Event)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn init_from_selection_clears_the_name_and_keeps_the_range() {
        let mut s = EventModalState {
            name: "stale".into(),
            ..Default::default()
        };
        s.init_from_selection("KTLX", 1705321845.0, 1705327200.0);
        assert!(s.name.is_empty());
        assert_eq!(s.site_id, "KTLX");
        assert_eq!((s.start, s.end), (1705321845.0, 1705327200.0));
    }

    #[wasm_bindgen_test]
    fn init_from_event_copies_the_event() {
        let mut s = EventModalState::default();
        s.init_from_event("Tornado", "KOUN", 1700000000.0, 1705321845.0);
        assert_eq!(s.name, "Tornado");
        assert_eq!(s.site_id, "KOUN");
        assert_eq!((s.start, s.end), (1700000000.0, 1705321845.0));
    }
}
