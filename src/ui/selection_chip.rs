//! The selection chip (desktop transport row and mobile transport bar): names
//! what plays and opens the menu that sets it. A thin projection of
//! [`crate::core::selection_chip`] — the label, the menu entries and the
//! intents they emit are all decided there.

use crate::core::selection_chip::{
    chip_menu, intent_for, ChipAction, ChipEnv, ChipLabel, ChipMenu,
};
use crate::core::session::ChipGlyphs;
use crate::core::transport::LiveStopPlacement;
use crate::state::AppState;
use crate::ui::theme;
use crate::ui::time_format::parts;
use eframe::egui::{self, Color32, RichText, Stroke};

// Icon-font glyphs (the text font has no arrows).
const ARROW: &str = egui_phosphor::regular::ARROW_RIGHT;

/// The chip's glyphs in the icon font, standing in for the design's ● ⟲ →.
const ICON_GLYPHS: ChipGlyphs = ChipGlyphs {
    live: egui_phosphor::regular::BROADCAST,
    loop_mark: egui_phosphor::regular::REPEAT,
    arrow: ARROW,
};

/// Where the chip is drawn: the desktop row or the mobile bar.
pub(crate) struct ChipStyle {
    pub text_size: f32,
    pub min_height: f32,
    pub stop_placement: LiveStopPlacement,
    /// Narrow bar: the core's compact words.
    pub compact: bool,
    /// The widest the chip may draw; a longer label ends in an ellipsis
    /// rather than crowding the controls beside it.
    pub max_width: Option<f32>,
}

/// Draw the chip and its menu, pushing the intent of any entry picked.
pub(crate) fn render_selection_chip(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &crate::subsystem::Live,
    playback: &crate::subsystem::Playback,
    view: (f64, f64),
    style: &ChipStyle,
) {
    let env = ChipEnv {
        stream_active: live.session.stream.is_running(),
        touch: state.is_mobile,
    };
    let label = live.session.selection_chip;
    let use_local = state.use_local_time;
    let clock = crate::ui::time_format::ZoneClock { use_local };
    let caret = |t: &str| format!("{t} {}", egui_phosphor::regular::CARET_DOWN);
    // In a tight row the chip takes the longest of the core's forms that
    // fits: the caret goes first (the chip's frame already reads as a
    // button), then the words the core lets go of; the shortest form
    // truncates only when even it does not fit.
    let text = match style.max_width {
        Some(max_w) => {
            let room = max_w - 2.0 * ui.spacing().button_padding.x;
            let width = |t: &str| {
                egui::WidgetText::from(RichText::new(t).size(style.text_size).strong())
                    .into_galley(
                        ui,
                        Some(egui::TextWrapMode::Extend),
                        f32::INFINITY,
                        egui::TextStyle::Button,
                    )
                    .size()
                    .x
            };
            let forms = live
                .session
                .selection_chip_forms(&clock, &ICON_GLYPHS, style.compact);
            let full = caret(&forms[0]);
            if width(&full) <= room {
                full
            } else {
                forms
                    .iter()
                    .find(|f| width(f) <= room)
                    .or(forms.last())
                    .cloned()
                    .unwrap_or_default()
            }
        }
        None => caret(
            &live
                .session
                .selection_chip_text(&clock, &ICON_GLYPHS, style.compact),
        ),
    };

    let (fill, text_color, stroke) = match label {
        ChipLabel::Live => {
            let pulse = live.mode_state.pulse_alpha();
            let alpha = (200.0 + 55.0 * pulse) as u8;
            let c = theme::NOW;
            (
                Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha),
                theme::TEXT,
                Stroke::NONE,
            )
        }
        ChipLabel::RangeToNow { .. } => (
            Color32::TRANSPARENT,
            theme::LOOP_TEXT,
            Stroke::new(1.0, theme::NOW),
        ),
        ChipLabel::Range { .. } => (
            Color32::TRANSPARENT,
            theme::LOOP_TEXT,
            Stroke::new(1.0, theme::LOOP_DEEP),
        ),
        ChipLabel::At { .. } => (
            Color32::TRANSPARENT,
            theme::TEXT_DIM,
            ui.visuals().widgets.inactive.bg_stroke,
        ),
    };

    let button = egui::Button::new(
        RichText::new(text)
            .size(style.text_size)
            .strong()
            .color(text_color),
    )
    .fill(fill)
    .stroke(stroke)
    .min_size(egui::vec2(0.0, style.min_height));
    let response = match style.max_width {
        Some(max_w) => {
            ui.allocate_ui_with_layout(
                egui::vec2(max_w.max(0.0), style.min_height),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| ui.add(button.truncate()),
            )
            .inner
        }
        None => ui.add(button),
    }
    .on_hover_text(label.hover());

    let now = state.frame_now.secs();
    let menu = chip_menu(&playback.state, env, view, now);
    let mut picked = None;
    // Closes on a click outside, or on picking an entry (below), so the range
    // editor's time fields can be clicked into.
    egui::Popup::menu(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(260.0);
            picked = render_menu(ui, &menu, use_local, now);
            if picked.is_some() {
                ui.close();
            }
        });
    if let Some(action) = picked {
        state.push_command(intent_for(action, env, style.stop_placement));
    }
}

/// The menu body, grouped by the shape each entry selects. Returns the entry
/// picked this frame.
fn render_menu(
    ui: &mut egui::Ui,
    menu: &ChipMenu,
    use_local: bool,
    now: f64,
) -> Option<ChipAction> {
    let mut picked = None;
    let keys = menu.pointer_hints;

    // The selection itself first, where it lives: how it loops, its times,
    // and saving it.
    if let Some(current) = menu.loop_style {
        group_heading(ui, "This range");
        ui.horizontal(|ui| {
            for style in crate::core::LoopMode::all() {
                if ui
                    .selectable_label(current == *style, style.label())
                    .clicked()
                {
                    picked = Some(ChipAction::LoopStyle(*style));
                }
            }
        });
        if let Some((start, end)) = menu.fixed_range {
            if let Some(action) = range_editor(ui, start, end, use_local, now) {
                picked = Some(action);
            }
        }
        if menu.can_save_event
            && entry(
                ui,
                format!("{} Save as event", egui_phosphor::regular::BOOKMARK_SIMPLE),
                false,
                ChipAction::SaveAsEvent,
                keys,
            )
        {
            picked = Some(ChipAction::SaveAsEvent);
        }
        ui.add_space(4.0);
    }

    group_heading(ui, "A point in time");
    if entry(
        ui,
        format!(
            "{} Live · the newest frame",
            egui_phosphor::regular::BROADCAST
        ),
        menu.at_live,
        ChipAction::Live,
        keys,
    ) {
        picked = Some(ChipAction::Live);
    }
    if let Some(ts) = menu.stop_looping_at {
        if entry(
            ui,
            format!("Stop looping · stay at {}", hm(ts, use_local)),
            false,
            ChipAction::StopLooping,
            keys,
        ) {
            picked = Some(ChipAction::StopLooping);
        }
    }
    if menu.can_stop_stream
        && entry(
            ui,
            "Stop the stream".to_string(),
            false,
            ChipAction::StopStream,
            keys,
        )
    {
        picked = Some(ChipAction::StopStream);
    }
    if menu.go_to_day
        && entry(
            ui,
            format!(
                "{} Go to a day\u{2026}",
                egui_phosphor::regular::CALENDAR_BLANK
            ),
            false,
            ChipAction::GoToDay,
            keys,
        )
    {
        picked = Some(ChipAction::GoToDay);
    }

    ui.add_space(4.0);
    group_heading(ui, "A range ending now");
    for (preset, on) in &menu.presets {
        let action = ChipAction::Preset(*preset);
        if entry(
            ui,
            format!("{} {} now", preset.label(), ARROW),
            *on,
            action,
            keys,
        ) {
            picked = Some(action);
        }
    }
    for line in menu.ending_now_hints() {
        hint(ui, line);
    }

    ui.add_space(4.0);
    group_heading(ui, "A fixed range");
    if let Some((start, end)) = menu.view_span {
        let action = ChipAction::LoopView { start, end };
        if entry(
            ui,
            format!(
                "Loop what's in view ({})",
                crate::core::LoopBasis::Duration(end - start).label()
            ),
            false,
            action,
            keys,
        ) {
            picked = Some(action);
        }
    }
    for line in menu.fixed_range_hints() {
        hint(ui, line);
    }
    picked
}

/// One menu entry, with its key beside it when the device has keys. Returns
/// whether it was clicked.
fn entry(ui: &mut egui::Ui, text: String, on: bool, action: ChipAction, keys: bool) -> bool {
    let text = match action.key_hint().filter(|_| keys) {
        Some(key) => format!("{text}   {key}"),
        None => text,
    };
    ui.selectable_label(on, text).clicked()
}

/// The range's start and end as time fields: entering either retimes it.
fn range_editor(
    ui: &mut egui::Ui,
    start: f64,
    end: f64,
    use_local: bool,
    now: f64,
) -> Option<ChipAction> {
    let look = crate::ui::time_field::TimeFieldLook {
        color: theme::LOOP_TEXT,
        size: theme::LABEL,
        hover: "Click to type a time",
    };
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new("From").size(theme::CAPTION).weak());
        if let Some(t) =
            crate::ui::time_field::time_field(ui, "range_start", start, now, use_local, &look)
        {
            picked = Some(ChipAction::EditRange { start: t, end });
        }
        ui.label(RichText::new("to").size(theme::CAPTION).weak());
        if let Some(t) =
            crate::ui::time_field::time_field(ui, "range_end", end, now, use_local, &look)
        {
            picked = Some(ChipAction::EditRange { start, end: t });
        }
    });
    picked
}

fn group_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).weak());
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(10.0).weak().italics());
}

fn hm(ts: f64, use_local: bool) -> String {
    let p = parts(ts, use_local);
    format!("{:02}:{:02}", p.hour, p.minute)
}
