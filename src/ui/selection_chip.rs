//! The selection chip (desktop transport row and mobile transport bar): names
//! what plays and opens the menu that sets it. A thin projection of
//! [`crate::core::selection_chip`] — the label, the menu entries and the
//! intents they emit are all decided there.

use crate::core::selection_chip::{
    chip_label, chip_menu, intent_for, ChipAction, ChipEnv, ChipLabel, ChipMenu,
};
use crate::core::transport::LiveStopPlacement;
use crate::state::AppState;
use crate::ui::colors::live;
use crate::ui::time_format::parts;
use eframe::egui::{self, Color32, RichText, Stroke};

/// Amber: a range that loops (the chip's color language, round-4 design).
const LOOP_TEXT: Color32 = Color32::from_rgb(243, 207, 134);
const LOOP_BORDER: Color32 = Color32::from_rgb(122, 90, 28);

// Icon-font glyphs (the text font has no arrows).
const ARROW: &str = egui_phosphor::regular::ARROW_RIGHT;
const LOOP: &str = egui_phosphor::regular::REPEAT;

/// Where the chip is drawn: the desktop row or the mobile bar.
pub(crate) struct ChipStyle {
    pub text_size: f32,
    pub min_height: f32,
    pub stop_placement: LiveStopPlacement,
    /// Narrow bar: drop the seconds and the lag from a point's label.
    pub compact: bool,
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
        stream_active: live.mode_state.is_active(),
        lag_secs: live.frame_status.lag_secs,
    };
    let label = chip_label(&playback.state, env);
    let use_local = state.use_local_time;
    let text = format!(
        "{} {}",
        label_text(&label, use_local, style.compact),
        egui_phosphor::regular::CARET_DOWN
    );

    let (fill, text_color, stroke) = match label {
        ChipLabel::Live => {
            let pulse = live.mode_state.pulse_alpha();
            let alpha = (200.0 + 55.0 * pulse) as u8;
            let c = live::STREAMING;
            (
                Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha),
                Color32::WHITE,
                Stroke::NONE,
            )
        }
        ChipLabel::RangeToNow { .. } => (
            Color32::TRANSPARENT,
            LOOP_TEXT,
            Stroke::new(1.0, live::STREAMING),
        ),
        ChipLabel::Range { .. } => (
            Color32::TRANSPARENT,
            LOOP_TEXT,
            Stroke::new(1.0, LOOP_BORDER),
        ),
        ChipLabel::At { .. } => (
            Color32::TRANSPARENT,
            crate::ui::colors::ui::value(state.is_dark),
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
    let response = ui.add(button).on_hover_text(label.hover());

    let menu = chip_menu(&playback.state, env, view, state.frame_now.secs());
    let mut picked = None;
    egui::Popup::menu(&response).show(|ui| {
        ui.set_min_width(240.0);
        picked = render_menu(ui, &menu, use_local);
    });
    if let Some(action) = picked {
        state.push_command(intent_for(action, env, style.stop_placement));
    }
}

/// The menu body, grouped by the shape each entry selects. Returns the entry
/// picked this frame.
fn render_menu(ui: &mut egui::Ui, menu: &ChipMenu, use_local: bool) -> Option<ChipAction> {
    let mut picked = None;
    let mut entry = |ui: &mut egui::Ui, text: String, on: bool, action: ChipAction| {
        if ui.selectable_label(on, text).clicked() {
            picked = Some(action);
        }
    };

    group_heading(ui, "A point in time");
    entry(
        ui,
        format!(
            "{} Live · the newest frame",
            egui_phosphor::regular::BROADCAST
        ),
        menu.at_live,
        ChipAction::Live,
    );
    if let Some(ts) = menu.stop_looping_at {
        entry(
            ui,
            format!("Stop looping · stay at {}", hm(ts, use_local)),
            false,
            ChipAction::StopLooping,
        );
    }
    if menu.can_stop_stream {
        entry(
            ui,
            "Stop the stream".to_string(),
            false,
            ChipAction::StopStream,
        );
    }

    ui.add_space(4.0);
    group_heading(ui, "A range ending now");
    for (preset, on) in &menu.presets {
        entry(
            ui,
            format!("{} {} now", preset.label(), ARROW),
            *on,
            ChipAction::Preset(*preset),
        );
    }
    hint(ui, "Or drag a range's right handle onto the live edge.");

    ui.add_space(4.0);
    group_heading(ui, "A fixed range");
    if let Some((start, end)) = menu.view_span {
        entry(
            ui,
            format!(
                "Loop what's in view ({})",
                crate::core::LoopBasis::Duration(end - start).label()
            ),
            false,
            ChipAction::LoopView { start, end },
        );
    }
    hint(ui, "Or shift-drag along the timeline.");
    picked
}

fn group_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).weak());
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(10.0).weak().italics());
}

/// The chip's words for a selection.
fn label_text(label: &ChipLabel, use_local: bool, compact: bool) -> String {
    match *label {
        ChipLabel::Live => format!("{} LIVE", egui_phosphor::regular::BROADCAST),
        ChipLabel::At { ts, .. } if compact => format!("At {}", hm(ts, use_local)),
        ChipLabel::At { ts, behind_secs } => match behind_secs {
            Some(lag) => format!(
                "At {} · {} behind",
                hms(ts, use_local),
                crate::core::format_lag(lag)
            ),
            None => format!("At {}", hms(ts, use_local)),
        },
        ChipLabel::Range { start, end } => {
            format!("{LOOP} {}", range_text(start, end, use_local))
        }
        ChipLabel::RangeToNow { basis } => format!("{LOOP} Last {} {ARROW} now", basis.label()),
    }
}

fn hms(ts: f64, use_local: bool) -> String {
    let p = parts(ts, use_local);
    format!("{:02}:{:02}:{:02}", p.hour, p.minute, p.second)
}

fn hm(ts: f64, use_local: bool) -> String {
    let p = parts(ts, use_local);
    format!("{:02}:{:02}", p.hour, p.minute)
}

/// "13:10 → 14:40", with dates when the range crosses a day boundary.
fn range_text(start: f64, end: f64, use_local: bool) -> String {
    let (a, b) = (parts(start, use_local), parts(end, use_local));
    if (a.year, a.month, a.day) == (b.year, b.month, b.day) {
        format!(
            "{:02}:{:02} {ARROW} {:02}:{:02}",
            a.hour, a.minute, b.hour, b.minute
        )
    } else {
        format!(
            "{}/{} {:02}:{:02} {ARROW} {}/{} {:02}:{:02}",
            a.month, a.day, a.hour, a.minute, b.month, b.day, b.hour, b.minute
        )
    }
}
