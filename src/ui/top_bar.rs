//! The desktop top bar (one-instrument §9): a 1:1 projection of
//! [`TopBarVm`].
//!
//! Left: site, product and tilt pickers, then the frame time with its zone and
//! the age pill. Right: the alerts chip, 2D/3D, help, the ⋯ menu and the panel
//! toggle. Every word comes from the view-model and every click is a
//! [`TopBarIntent`]; the only thing decided here is how wide each piece draws,
//! which feeds the pure width rule [`fold`].

use super::layout::{Layer, LayerKind, LayoutCtx};
use super::overflow_menu::overflow_menu;
use crate::core::top_bar::{
    fold, AlertsChip, PillMark, ReadoutPill, TopBarFold, TopBarIntent, TopBarVm, TopBarWidths,
};
use crate::core::Intent;
use crate::geo::ViewMode;
use crate::state::AppState;
use crate::ui::theme;
use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icons;

const BAR_HEIGHT: f32 = 36.0;
/// The releases page behind ⋯ › About.
const CHANGELOG_URL: &str = "https://github.com/danielway/nexrad-workbench/releases";

pub(super) struct TopBarLayer;

impl Layer for TopBarLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        10
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_top_bar(ctx.root_ui, ctx.top_bar_vm, ctx.state);
    }
}

pub(super) fn emit(state: &mut AppState, intent: TopBarIntent) {
    state.push_command(Intent::TopBar(intent));
}

fn draw_top_bar(root_ui: &mut egui::Ui, vm: &TopBarVm, state: &mut AppState) {
    let response = egui::Panel::top("top_bar")
        .exact_size(BAR_HEIGHT)
        .show(root_ui, |ui| {
            ui.horizontal_centered(|ui| {
                let f = fold(ui.available_width(), &measure(ui, vm));

                if vm.show_left_toggle
                    && icon_button(ui, icons::SIDEBAR_SIMPLE, "Toggle left panel")
                {
                    emit(state, TopBarIntent::ToggleLeftPanel);
                }
                site_picker(ui, vm, state, f.city);
                product_picker(ui, vm, state);
                tilt_picker(ui, vm, state);
                ui.add_space(6.0);
                frame_readout(ui, vm, state, f.zone);
                if let Some(pill) = &vm.pill {
                    readout_pill(ui, pill);
                }
                if let Some(err) = &vm.worker_error {
                    worker_error(ui, err, state);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, icons::SIDEBAR_SIMPLE, "Toggle right panel") {
                        emit(state, TopBarIntent::ToggleRightPanel);
                    }
                    overflow_menu(ui, |ui| overflow_contents(ui, vm, state, f));
                    if f.help_and_views {
                        if icon_button(ui, icons::QUESTION, "Keyboard shortcuts (?)") {
                            emit(state, TopBarIntent::ToggleShortcutsHelp);
                        }
                        view_toggle(ui, vm, state);
                    }
                    alerts_chip(ui, &vm.alerts, state, false);
                });
            });
        });
    super::toast::set_anchor(root_ui.ctx(), response.response.rect.bottom());
}

// ── Measuring (the shell's half of the width rule) ─────────────────────────

fn text_width(ui: &egui::Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, theme::TEXT)
        .size()
        .x
}

fn label_font() -> egui::FontId {
    egui::FontId::proportional(theme::LABEL)
}

/// A framed button around `text`, plus the gap after it.
fn button_width(ui: &egui::Ui, text: &str, font: egui::FontId) -> f32 {
    text_width(ui, text, font) + 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x
}

fn measure(ui: &egui::Ui, vm: &TopBarVm) -> TopBarWidths {
    let sp = ui.spacing().item_spacing.x;
    let icon = |g: &str| button_width(ui, g, egui::FontId::proportional(theme::BODY));
    let label = |t: &str| button_width(ui, t, label_font());
    let mono = |t: &str| text_width(ui, t, theme::readout_font()) + sp;

    let mut fixed = 0.0;
    if vm.show_left_toggle {
        fixed += icon(icons::SIDEBAR_SIMPLE);
    }
    fixed += label(&format!("{} {}", vm.site.id, icons::CARET_DOWN));
    fixed += label(&format!("{} {}", vm.product.code, icons::CARET_DOWN));
    fixed += label(&format!("{} {}", vm.tilt.label, icons::CARET_DOWN));
    fixed += 6.0;
    if let Some(date) = &vm.readout.date {
        fixed += mono(date);
    }
    fixed += mono(&vm.readout.time) + 2.0 * ui.spacing().button_padding.x;
    if let Some(pill) = &vm.pill {
        fixed += text_width(ui, &pill.text, label_font()) + 36.0 + sp;
    }
    if let Some(err) = &vm.worker_error {
        fixed += text_width(ui, err, label_font()) + icon(icons::WARNING) + label("Retry");
    }
    // Right cluster: panel toggle, ⋯ and the alerts chip.
    fixed += icon(icons::SIDEBAR_SIMPLE) + icon(icons::DOTS_THREE);
    fixed += match &vm.alerts {
        AlertsChip::Hidden => 0.0,
        AlertsChip::Loading | AlertsChip::Quiet { .. } => icon(icons::BELL_SIMPLE),
        AlertsChip::Active { label: l, .. } => label(&format!("{l} {}", icons::CARET_DOWN)),
    };

    TopBarWidths {
        fixed,
        city: vm
            .site
            .city
            .as_deref()
            .map_or(0.0, |c| text_width(ui, c, label_font()) + 6.0),
        zone: text_width(ui, &vm.readout.zone, label_font())
            + 2.0 * ui.spacing().button_padding.x
            + sp,
        help_and_views: icon(icons::QUESTION) + label("2D") + label("3D"),
    }
}

// ── Pieces ─────────────────────────────────────────────────────────────────

fn icon_button(ui: &mut egui::Ui, glyph: &str, hover: &str) -> bool {
    ui.button(RichText::new(glyph).size(theme::BODY))
        .on_hover_text(hover)
        .clicked()
}

/// "KTLX  Oklahoma City, OK ▾" — the ID always, the city while it fits.
fn site_picker(ui: &mut egui::Ui, vm: &TopBarVm, state: &mut AppState, show_city: bool) {
    let mut job = egui::text::LayoutJob::default();
    let style = ui.style().clone();
    let mut push = |text: RichText| {
        text.append_to(
            &mut job,
            &style,
            egui::FontSelection::Default,
            egui::Align::Center,
        )
    };
    push(RichText::new(&vm.site.id).strong().color(theme::TEXT));
    if let (true, Some(city)) = (show_city, &vm.site.city) {
        push(RichText::new(format!("  {city}")).color(theme::TEXT_DIM));
    }
    push(RichText::new(format!(" {}", icons::CARET_DOWN)).color(theme::TEXT_FAINT));
    let hover = match &vm.site.city {
        Some(city) => format!("{} · {city} — change site", vm.site.id),
        None => format!("{} — change site", vm.site.id),
    };
    if ui.button(job).on_hover_text(hover).clicked() {
        emit(state, TopBarIntent::OpenSitePicker);
    }
}

fn picker_label(text: &str) -> RichText {
    RichText::new(format!("{text} {}", icons::CARET_DOWN)).color(theme::TEXT)
}

fn product_picker(ui: &mut egui::Ui, vm: &TopBarVm, state: &mut AppState) {
    let resp = ui.menu_button(picker_label(vm.product.code), |ui| {
        ui.set_min_width(200.0);
        for row in &vm.product.rows {
            if ui.selectable_label(row.selected, &row.label).clicked() {
                emit(state, TopBarIntent::SetProduct(row.product));
                ui.close();
            }
        }
    });
    resp.response.on_hover_text(vm.product.hover);
}

fn tilt_picker(ui: &mut egui::Ui, vm: &TopBarVm, state: &mut AppState) {
    let resp = ui.menu_button(picker_label(&vm.tilt.label), |ui| {
        ui.set_min_width(120.0);
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .id_salt("topbar_tilt_rows")
            .show(ui, |ui| {
                for row in &vm.tilt.rows {
                    let mut text = RichText::new(&row.label);
                    if row.label != "Latest" {
                        text = text.monospace();
                    }
                    let resp = ui.horizontal(|ui| {
                        let r = ui
                            .add_enabled(row.enabled, egui::Button::selectable(row.selected, text));
                        if let Some(note) = row.note {
                            ui.label(
                                RichText::new(note)
                                    .size(theme::CAPTION)
                                    .color(theme::TEXT_FAINT),
                            );
                        }
                        r
                    });
                    let r = resp
                        .inner
                        .on_disabled_hover_text("Not recorded at this tilt");
                    if r.clicked() {
                        emit(state, TopBarIntent::SetTilt(row.pick));
                        ui.close();
                    }
                }
            });
    });
    resp.response.on_hover_text("Tilt");
}

/// The frame time in the one clock, the zone shown once after it. Clicking
/// either toggles local / UTC.
fn frame_readout(ui: &mut egui::Ui, vm: &TopBarVm, state: &mut AppState, show_zone: bool) {
    let r = &vm.readout;
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 4.0;
    if let Some(date) = &r.date {
        ui.label(theme::readout(date).color(theme::TEXT_DIM));
    }
    let color = if r.has_frame {
        theme::TEXT
    } else {
        theme::TEXT_FAINT
    };
    let hover = if r.has_frame {
        format!("Frame on screen · {}", r.zone_hover)
    } else {
        r.zone_hover.clone()
    };
    let time = ui
        .add(egui::Button::new(theme::readout(&r.time).color(color)).frame(false))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hover);
    let mut clicked = time.clicked();
    if show_zone {
        clicked |= ui
            .add(
                egui::Button::new(
                    RichText::new(&r.zone)
                        .size(theme::LABEL)
                        .color(theme::TEXT_DIM),
                )
                .frame(false),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(&r.zone_hover)
            .clicked();
    }
    ui.spacing_mut().item_spacing.x = spacing;
    if clicked {
        emit(state, TopBarIntent::ToggleTimeZone);
    }
}

/// The pill: red dot (pinned to now), amber loop mark (looping to now), or
/// nothing ("no frame yet"), then the words.
pub(super) fn readout_pill(ui: &mut egui::Ui, pill: &ReadoutPill) {
    // Painted at a fixed height so the pill never stretches to the bar.
    const HEIGHT: f32 = 20.0;
    const PAD: f32 = 8.0;
    const GAP: f32 = 5.0;
    let font = egui::FontId::proportional(theme::LABEL);
    let text = ui
        .painter()
        .layout_no_wrap(pill.text.clone(), font.clone(), theme::TEXT_DIM);
    let mark = match pill.mark {
        PillMark::Pinned => None,
        PillMark::Loop => Some(ui.painter().layout_no_wrap(
            icons::REPEAT.to_string(),
            font,
            theme::LOOP,
        )),
        PillMark::None => None,
    };
    let mark_w = match (pill.mark, &mark) {
        (PillMark::Pinned, _) => 7.0 + GAP,
        (_, Some(g)) => g.size().x + GAP,
        _ => 0.0,
    };
    let size = egui::vec2(PAD * 2.0 + mark_w + text.size().x, HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::CornerRadius::same((HEIGHT / 2.0) as u8),
        theme::RAISED,
        egui::Stroke::new(1.0, theme::LINE),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + PAD;
    let cy = rect.center().y;
    match (pill.mark, mark) {
        (PillMark::Pinned, _) => {
            painter.circle_filled(egui::pos2(x + 3.5, cy), 3.5, theme::NOW);
        }
        (_, Some(g)) => {
            painter.galley(egui::pos2(x, cy - g.size().y / 2.0), g, theme::LOOP);
        }
        _ => {}
    }
    x += mark_w;
    painter.galley(
        egui::pos2(x, cy - text.size().y / 2.0),
        text,
        theme::TEXT_DIM,
    );
    resp.on_hover_text(pill.hover);
}

/// The decode worker failed to start. ⚠ in text color, never red.
pub(super) fn worker_error(ui: &mut egui::Ui, message: &str, state: &mut AppState) {
    ui.label(RichText::new(icons::WARNING).color(theme::TEXT));
    ui.label(RichText::new(message).color(theme::TEXT_DIM));
    if ui
        .small_button("Retry")
        .on_hover_text("Retry starting the decode worker")
        .clicked()
    {
        state.push_command(Intent::RetryWorker);
    }
}

/// 2D / 3D as a two-segment toggle; the active segment is the one selected
/// color. Drawn right-to-left, so 3D first.
fn view_toggle(ui: &mut egui::Ui, vm: &TopBarVm, state: &mut AppState) {
    for (label, mode, key) in [
        ("3D", ViewMode::Globe3D, "2"),
        ("2D", ViewMode::Flat2D, "1"),
    ] {
        let active = vm.view_mode == mode;
        let text = RichText::new(label).color(if active {
            theme::SELECTED
        } else {
            theme::TEXT_DIM
        });
        if ui
            .add(egui::Button::new(text).frame(active))
            .on_hover_text(format!("{label} view ({key})"))
            .clicked()
            && !active
        {
            emit(state, TopBarIntent::SetView(mode));
        }
    }
}

/// The alerts chip, in the most severe event's color. `compact` (the phone
/// bar) shows the count instead of the words.
pub(super) fn alerts_chip(
    ui: &mut egui::Ui,
    chip: &AlertsChip,
    state: &mut AppState,
    compact: bool,
) {
    match chip {
        AlertsChip::Hidden => {}
        AlertsChip::Loading => {
            ui.label(RichText::new(icons::BELL_SIMPLE).color(theme::TEXT_FAINT))
                .on_hover_text("Loading NWS alerts\u{2026}");
        }
        AlertsChip::Quiet {
            failed,
            hover,
            click,
        } => {
            let (glyph, color) = if *failed {
                (icons::WARNING, theme::TEXT_DIM)
            } else {
                (icons::BELL_SIMPLE, theme::TEXT_FAINT)
            };
            if ui
                .add(egui::Button::new(RichText::new(glyph).color(color)).frame(false))
                .on_hover_text(hover)
                .clicked()
            {
                state.push_command(click.clone());
            }
        }
        AlertsChip::Active {
            label,
            compact: count,
            event,
            hover,
            click,
        } => {
            let (r, g, b) = crate::alerts::event_color(event);
            let color = Color32::from_rgb(r, g, b);
            let text = if compact {
                format!("{} {count}", icons::BELL_SIMPLE)
            } else {
                format!("{label} {}", icons::CARET_DOWN)
            };
            if ui
                .button(RichText::new(text).strong().color(color))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(hover)
                .clicked()
            {
                state.push_command(click.clone());
            }
        }
    }
}

/// ⋯: the keyboard map, settings and About (§9, §11), plus help and 2D/3D
/// once they have folded.
fn overflow_contents(ui: &mut egui::Ui, vm: &TopBarVm, state: &mut AppState, f: TopBarFold) {
    if !f.help_and_views {
        ui.label(
            RichText::new("View")
                .size(theme::CAPTION)
                .color(theme::TEXT_FAINT),
        );
        ui.horizontal(|ui| {
            for (label, mode) in [("2D", ViewMode::Flat2D), ("3D", ViewMode::Globe3D)] {
                if ui.selectable_label(vm.view_mode == mode, label).clicked() {
                    emit(state, TopBarIntent::SetView(mode));
                }
            }
        });
        ui.separator();
    }
    if ui
        .button(format!("{}  Keyboard shortcuts", icons::KEYBOARD))
        .clicked()
    {
        emit(state, TopBarIntent::ToggleShortcutsHelp);
        ui.close();
    }
    ui.separator();
    ui.label(
        RichText::new("Settings")
            .size(theme::CAPTION)
            .color(theme::TEXT_FAINT),
    );
    let mut advanced = vm.advanced;
    if ui
        .checkbox(&mut advanced, "Advanced controls")
        .on_hover_text("Show every control (Ctrl+Shift+A)")
        .changed()
    {
        emit(state, TopBarIntent::SetAdvanced(advanced));
    }
    ui.separator();
    ui.label(
        RichText::new("About")
            .size(theme::CAPTION)
            .color(theme::TEXT_FAINT),
    );
    ui.label(
        RichText::new(&vm.version)
            .size(theme::CAPTION)
            .monospace()
            .color(theme::TEXT_DIM),
    );
    if ui
        .button(format!("{}  Changelog", icons::ARROW_SQUARE_OUT))
        .clicked()
    {
        state.push_command(Intent::OpenExternalUrl(CHANGELOG_URL));
        ui.close();
    }
}
