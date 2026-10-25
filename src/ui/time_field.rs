//! The time field widget: a clock you click to type into (§4 "one clock").
//! Shows [`crate::core::time_field::format_time_field`]; on click it becomes a
//! text box, Enter reads it with
//! [`crate::core::time_field::parse_time_field`], Escape or clicking away
//! cancels, and a refusal shows inline beside the box. The typed text is
//! widget state (egui memory); what a submitted time *does* is the caller's
//! intent.

use crate::core::time_field::{format_time_field, parse_time_field};
use crate::ui::theme;
use crate::ui::time_format::ZoneClock;
use eframe::egui::{self, Color32, RichText};

/// The text being edited, kept in egui memory while the field is open.
#[derive(Clone, Default)]
struct Editing {
    open: bool,
    text: String,
    error: Option<String>,
    /// Focus and select the text on the first frame open.
    fresh: bool,
}

/// How the field looks when it is not being edited.
pub(crate) struct TimeFieldLook {
    pub color: Color32,
    pub size: f32,
    pub hover: &'static str,
}

/// Draw a time field showing `ts`. Returns the time the user entered, the
/// frame they press Enter on a readable one.
pub(crate) fn time_field(
    ui: &mut egui::Ui,
    id_salt: &str,
    ts: f64,
    now: f64,
    use_local: bool,
    look: &TimeFieldLook,
) -> Option<f64> {
    let id = ui.make_persistent_id(("time_field", id_salt));
    let clock = ZoneClock { use_local };
    let shown = format_time_field(ts, now, &clock);
    let mut editing: Editing = ui.data(|d| d.get_temp(id)).unwrap_or_default();

    if !editing.open {
        let response = ui
            .add(
                egui::Button::new(
                    RichText::new(&shown)
                        .monospace()
                        .size(look.size)
                        .color(look.color),
                )
                .frame(false),
            )
            .on_hover_cursor(egui::CursorIcon::Text)
            .on_hover_text(look.hover);
        if response.clicked() {
            editing = Editing {
                open: true,
                text: shown,
                error: None,
                fresh: true,
            };
            ui.data_mut(|d| d.insert_temp(id, editing));
            ui.ctx().request_repaint();
        }
        return None;
    }

    let edit_id = id.with("edit");
    let mut submitted = None;
    ui.horizontal(|ui| {
        // Room for the longest form ("Oct 30 2026 00:00:00") in mono.
        let width = look.size * 0.62 * 21.0;
        let response = ui.add(
            egui::TextEdit::singleline(&mut editing.text)
                .id(edit_id)
                .font(egui::FontId::monospace(look.size))
                .text_color(look.color)
                .desired_width(width),
        );
        if editing.fresh {
            editing.fresh = false;
            response.request_focus();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), edit_id) {
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(editing.text.chars().count()),
                );
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), edit_id);
            }
        }
        // Escape cancels the edit and is claimed, so the dialog the field
        // sits in stays open (the stack only sees an unclaimed Escape).
        let (enter, escape) = ui.input_mut(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if escape {
            editing.open = false;
        } else if enter && (response.lost_focus() || response.has_focus()) {
            match parse_time_field(&editing.text, ts, now, &clock) {
                Ok(t) => {
                    submitted = Some(t);
                    editing.open = false;
                }
                Err(e) => {
                    editing.error = Some(e.message());
                    response.request_focus();
                }
            }
        } else if response.lost_focus() {
            editing.open = false;
        } else if response.changed() {
            editing.error = None;
        }
        if let Some(error) = &editing.error {
            ui.label(
                RichText::new(format!("{} {error}", crate::ui::icons::WARNING))
                    .size(theme::CAPTION)
                    .color(theme::TEXT),
            );
        }
    });
    ui.data_mut(|d| {
        if editing.open {
            d.insert_temp(id, editing);
        } else {
            d.remove::<Editing>(id);
        }
    });
    submitted
}
