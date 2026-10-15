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
        stream_active: live.session.stream.is_running(),
    };
    let label = live.session.selection_chip;
    let use_local = state.use_local_time;
    let text = format!(
        "{} {}",
        live.session.selection_chip_text(
            &crate::ui::time_format::ZoneClock { use_local },
            &ICON_GLYPHS,
            style.compact
        ),
        egui_phosphor::regular::CARET_DOWN
    );

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
    hint(ui, "Or drop a range's right handle on the live edge.");

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
    hint(ui, "Or drag along the strip above the frames.");
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
