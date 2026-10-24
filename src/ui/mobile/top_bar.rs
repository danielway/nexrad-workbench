//! The phone's top bar (phone timeline v2): the site, one clock, one age
//! pill, the bell and the sheet. The clock is the frame on screen
//! ([`crate::core::phone_dock::phone_clock`], dated only when not today),
//! the pill says live, how old, or the loop's age
//! ([`crate::core::phone_dock::phone_age`]), and the bell carries the
//! alerts with a dot when something failed
//! ([`crate::core::phone_dock::phone_bell`]). No stacked date or zone, no
//! mode badge and no version stamp (the version lives in the sheet's
//! Settings › About, as in the desktop bar's ⋯ › About).

use super::super::layout::{Layer, LayerKind, LayoutCtx};
use super::super::top_bar::emit;
use crate::core::phone_dock::{
    phone_age, phone_bell, phone_clock, AgeTone, BellTone, PhoneAge, PhoneBell, PhoneClock,
};
use crate::core::top_bar::{TopBarIntent, TopBarVm};
use crate::state::AppState;
use crate::ui::theme;
use crate::ui::theme::phone as colors;
use eframe::egui::{self, FontId, Frame, Margin, Sense, Stroke, StrokeKind};
use egui_phosphor::regular as icons;

const TOP_BAR_CONTENT_HEIGHT: f32 = 48.0;
/// The bell's and the sheet button's square.
const ICON_BOX: f32 = 32.0;
const GAP: f32 = 10.0;

pub(in crate::ui) struct MobileTopBarLayer;

impl Layer for MobileTopBarLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        10
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        // Slides away with the bottom chrome while playback runs (§13): the
        // decision is `chrome.mobile_auto_hide`, resolved once per frame.
        let shown = !ctx.chrome.mobile_auto_hide.hidden;
        let warning = ctx.activity_vm.warning().map(|(_, hover)| hover);
        let bell = phone_bell(&ctx.top_bar_vm.alerts, warning.as_deref());
        let clock = phone_clock(
            &ctx.top_bar_vm.readout,
            ctx.chrome.phone_loading.as_ref().map(|l| l.time.as_str()),
        );
        let age = phone_age(&ctx.live.session);
        draw_mobile_top_bar(
            ctx.root_ui,
            shown,
            ctx.top_bar_vm,
            &Bar { clock, age, bell },
            ctx.state,
            ctx.chrome,
        );
    }
}

/// What the bar draws besides the site.
struct Bar {
    clock: PhoneClock,
    age: PhoneAge,
    bell: PhoneBell,
}

fn draw_mobile_top_bar(
    root_ui: &mut egui::Ui,
    shown: bool,
    vm: &TopBarVm,
    bar: &Bar,
    state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
) {
    let ctx = root_ui.ctx().clone();
    // iOS safe area: zero in portrait now that the installed app uses an
    // opaque status bar (index.html), but kept so any top inset the browser
    // reports still keeps OS icons off our content.
    let (inset_top, _inset_right, _inset_bottom, _inset_left) = super::safe_area_insets();
    let frame = Frame::NONE.fill(theme::MAP).inner_margin(Margin::ZERO);

    // `show_collapsible` slides the bar in and out over egui's 200 ms
    // animation (core::phone::CHROME_SLIDE_SECS).
    root_ui.style_mut().animation_time = crate::core::phone::CHROME_SLIDE_SECS;
    let mut expanded = shown;
    let response = egui::Panel::top("mobile_top_bar")
        .resizable(false)
        .exact_size(inset_top + TOP_BAR_CONTENT_HEIGHT)
        .frame(frame)
        .show_collapsible(root_ui, &mut expanded, |ui| {
            if inset_top > 0.0 {
                ui.add_space(inset_top);
            }
            ui.horizontal_centered(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if icon_button(ui, icons::LIST, theme::TEXT_DIM, false)
                        .on_hover_text("View, layers, events and settings")
                        .clicked()
                    {
                        chrome.mobile_settings_open = true;
                    }
                    bell_button(ui, &bar.bell, state);
                    // The decode worker failed to start: critical, so it
                    // shows here too, as a tap to retry.
                    if let Some(err) = &vm.worker_error {
                        if icon_button(ui, icons::WARNING, theme::TEXT, false)
                            .on_hover_text(format!("{err} \u{2014} tap to retry"))
                            .clicked()
                        {
                            state.push_command(crate::core::Intent::RetryWorker);
                        }
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        ui.spacing_mut().item_spacing.x = GAP;
                        if site_chip(ui, &vm.site.id).clicked() {
                            emit(state, TopBarIntent::OpenSitePicker);
                        }
                        let pill_w = age_width(ui, &bar.age);
                        let room = ui.available_width() - pill_w - 2.0 * GAP;
                        if clock(ui, &bar.clock, room).clicked() {
                            emit(state, TopBarIntent::ToggleTimeZone);
                        }
                        age_pill(ui, &bar.age);
                    });
                });
            });
        });
    let anchor = response.map_or(inset_top, |r| r.response.rect.bottom());
    super::super::toast::set_anchor(&ctx, anchor);
}

/// The site: a small mono chip that opens the site picker.
fn site_chip(ui: &mut egui::Ui, id: &str) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(id.to_string(), FontId::monospace(13.0), theme::TEXT);
    // The caret is an icon-font glyph: the mono font has none.
    let caret = ui.painter().layout_no_wrap(
        icons::CARET_DOWN.to_string(),
        FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
    let size = egui::vec2(
        galley.size().x + 5.0 + caret.size().x + 16.0,
        galley.size().y + 8.0,
    );
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect(
        rect,
        6.0,
        colors::CONTROL,
        Stroke::new(1.0, theme::LINE),
        StrokeKind::Inside,
    );
    let gw = galley.size().x;
    p.galley(rect.min + egui::vec2(8.0, 4.0), galley, theme::TEXT);
    p.galley(
        egui::pos2(
            rect.left() + 8.0 + gw + 5.0,
            rect.center().y - caret.size().y / 2.0,
        ),
        caret,
        theme::TEXT_DIM,
    );
    resp.on_hover_text("Choose a radar site")
}

/// The one clock: the first form that fits `room`, with the zone as a
/// small suffix while it fits too. A tap toggles local / UTC.
fn clock(ui: &mut egui::Ui, c: &PhoneClock, room: f32) -> egui::Response {
    let font = FontId::monospace(15.0);
    let color = if c.bright {
        theme::TEXT
    } else {
        theme::TEXT_FAINT
    };
    let layout = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_string(), font.clone(), color)
    };
    let zone =
        ui.painter()
            .layout_no_wrap(c.zone.clone(), FontId::proportional(11.0), theme::TEXT_DIM);
    let zone_w = zone.size().x + 5.0;
    let mut chosen = None;
    for f in &c.forms {
        let g = layout(f);
        if g.size().x + zone_w <= room {
            chosen = Some((g, true));
            break;
        }
        if g.size().x <= room {
            chosen = Some((g, false));
            break;
        }
    }
    let (galley, with_zone) =
        chosen.unwrap_or_else(|| (layout(c.forms.last().map_or("", String::as_str)), false));
    let w = galley.size().x + if with_zone { zone_w } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ICON_BOX), Sense::click());
    let y = rect.center().y - galley.size().y / 2.0;
    let gw = galley.size().x;
    let baseline = y + galley.size().y;
    ui.painter()
        .galley(egui::pos2(rect.left(), y), galley, color);
    if with_zone {
        let zy = baseline - zone.size().y - 1.0;
        ui.painter().galley(
            egui::pos2(rect.left() + gw + 5.0, zy),
            zone,
            theme::TEXT_DIM,
        );
    }
    resp.on_hover_text("The frame on screen \u{2014} tap for local time or UTC")
}

const PILL_H: f32 = 22.0;
const PILL_PAD: f32 = 8.0;
const PILL_GAP: f32 = 6.0;

/// The pill's mark: the live dot, or the loop glyph.
fn age_mark(age: &PhoneAge) -> Option<&'static str> {
    match age.tone {
        AgeTone::Loop => Some(icons::REPEAT),
        AgeTone::Live | AgeTone::Point => None,
    }
}

fn age_font() -> FontId {
    FontId::proportional(12.0)
}

fn age_width(ui: &egui::Ui, age: &PhoneAge) -> f32 {
    let text = ui
        .painter()
        .layout_no_wrap(age.text.clone(), age_font(), theme::TEXT)
        .size()
        .x;
    let mark = match (age.tone, age_mark(age)) {
        (AgeTone::Live, _) => 7.0 + PILL_GAP,
        (_, Some(m)) => {
            ui.painter()
                .layout_no_wrap(m.to_string(), age_font(), theme::TEXT)
                .size()
                .x
                + PILL_GAP
        }
        _ => 0.0,
    };
    2.0 * PILL_PAD + mark + text
}

/// The age pill: red filled `● LIVE`, grey `20 min ago`, amber
/// `⟲ 13 min ago`.
fn age_pill(ui: &mut egui::Ui, age: &PhoneAge) {
    let w = age_width(ui, age);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, PILL_H), Sense::hover());
    let p = ui.painter();
    let (fill, stroke, color) = match age.tone {
        AgeTone::Live => (colors::NOW, colors::NOW, theme::TEXT),
        AgeTone::Loop => (egui::Color32::TRANSPARENT, theme::LOOP, theme::LOOP),
        AgeTone::Point => (egui::Color32::TRANSPARENT, theme::LINE, theme::TEXT_DIM),
    };
    p.rect(
        rect,
        PILL_H / 2.0,
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    let cy = rect.center().y;
    let mut x = rect.left() + PILL_PAD;
    match (age.tone, age_mark(age)) {
        (AgeTone::Live, _) => {
            p.circle_filled(egui::pos2(x + 3.5, cy), 3.5, theme::TEXT);
            x += 7.0 + PILL_GAP;
        }
        (_, Some(m)) => {
            let g = p.layout_no_wrap(m.to_string(), age_font(), color);
            let gw = g.size().x;
            p.galley(egui::pos2(x, cy - g.size().y / 2.0), g, color);
            x += gw + PILL_GAP;
        }
        _ => {}
    }
    let g = p.layout_no_wrap(age.text.clone(), age_font(), color);
    p.galley(egui::pos2(x, cy - g.size().y / 2.0), g, color);
}

/// A bare icon in a square target.
fn icon_button(ui: &mut egui::Ui, glyph: &str, color: egui::Color32, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ICON_BOX, ICON_BOX), Sense::click());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(19.0),
        if on { theme::SELECTED } else { color },
    );
    resp
}

/// The bell: the alerts in view (their count in the most severe one's
/// colour), with an amber dot when something failed.
fn bell_button(ui: &mut egui::Ui, bell: &PhoneBell, state: &mut AppState) {
    let color = match &bell.tone {
        BellTone::Quiet => theme::TEXT_DIM,
        BellTone::Failed => theme::TEXT_FAINT,
        BellTone::Alert(event) => {
            let (r, g, b) = crate::alerts::event_color(event);
            egui::Color32::from_rgb(r, g, b)
        }
    };
    let count = bell.count.as_ref().map(|c| {
        ui.painter()
            .layout_no_wrap(c.clone(), FontId::proportional(12.0), color)
    });
    let extra = count.as_ref().map_or(0.0, |g| g.size().x + 2.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ICON_BOX + extra, ICON_BOX), Sense::click());
    let icon_c = egui::pos2(rect.left() + ICON_BOX / 2.0, rect.center().y);
    let glyph = if bell.tone == BellTone::Failed {
        icons::BELL_SIMPLE_SLASH
    } else {
        icons::BELL_SIMPLE
    };
    ui.painter().text(
        icon_c,
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(19.0),
        color,
    );
    if let Some(g) = count {
        let at = egui::pos2(
            rect.left() + ICON_BOX - 4.0,
            rect.center().y - g.size().y / 2.0,
        );
        ui.painter().galley(at, g, color);
    }
    if bell.dot {
        ui.painter()
            .circle_filled(icon_c + egui::vec2(6.5, -7.0), 3.5, theme::LOOP);
    }
    let resp = if bell.hover.is_empty() {
        resp
    } else {
        resp.on_hover_text(&bell.hover)
    };
    if resp.clicked() {
        state.push_command(bell.tap.clone());
    }
}
