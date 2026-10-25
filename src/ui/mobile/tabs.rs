//! Mobile bottom chrome (phone timeline v2): the dock, 120 px of it.
//!
//! Two stacked `Panel::bottom` panels:
//!   1. The transport row (54 px) — bottommost. Play/pause, the selection
//!      chip, the NOW · 3 H · 24 H control and, when the selection is not
//!      pinned to now, LIVE. Nothing else: stepping is a tap on the next
//!      block, the speed is in the sheet's View tab, failures are a dot on
//!      the top bar's bell.
//!   2. The row (66 px) — topmost. A row of volumes you can touch
//!      (`ui::timeline::mini`).

use crate::core::phone_dock::{live_button, phone_chip};
use crate::core::phone_strip::{zoom_pill_label, StripZoom};
use crate::core::transport::PlayButton;
use crate::core::Intent;
use crate::state::AppState;
use crate::ui::theme;
use crate::ui::theme::phone as colors;
use eframe::egui::{self, FontId, Sense, Stroke, StrokeKind};

const TRANSPORT_BAR_HEIGHT: f32 = 54.0;
const MINI_AREA_HEIGHT: f32 = crate::ui::timeline::mini::MINI_TIMELINE_H;

/// The play button's diameter.
const PLAY_D: f32 = 42.0;
/// The chip's and LIVE's height.
const PILL_H: f32 = 38.0;

pub(in crate::ui) struct MobileChromeLayer;

impl super::super::layout::Layer for MobileChromeLayer {
    fn kind(&self) -> super::super::layout::LayerKind {
        super::super::layout::LayerKind::Chrome
    }
    fn z_order(&self) -> i32 {
        20
    }
    fn render(&self, ctx: &mut super::super::layout::LayoutCtx) {
        draw_mobile_chrome(ctx);
    }
}

fn draw_mobile_chrome(ctx: &mut super::super::layout::LayoutCtx) {
    let super::super::layout::LayoutCtx {
        root_ui,
        state,
        timeline,
        sites,
        live,
        playback,
        chrome,
        ..
    } = ctx;
    // iOS safe area: when installed as a home-screen PWA, the canvas extends
    // under the home indicator reservation. Pad the transport row below so the
    // controls don't sit flush with the bottom edge of the screen.
    let (_t, _r, inset_bottom, _l) = super::safe_area_insets();

    // Both panels slide away together while playback runs (§13); the
    // decision is `chrome.mobile_auto_hide`, resolved once per frame. A press
    // anywhere resets its clock, so using the transport keeps it up.
    let shown = !chrome.mobile_auto_hide.hidden;
    // The strip claims its own pinch afresh each frame it draws.
    chrome.phone_strip_claims_touch = false;
    let dock = egui::Frame::NONE
        .fill(colors::DOCK)
        .inner_margin(egui::Margin::ZERO);

    root_ui.style_mut().animation_time = crate::core::phone::CHROME_SLIDE_SECS;
    // Bottommost — the transport row.
    let mut expanded = shown;
    egui::Panel::bottom("mobile_transport_bar")
        .resizable(false)
        .exact_size(TRANSPORT_BAR_HEIGHT + inset_bottom)
        .frame(dock)
        .show_collapsible(root_ui, &mut expanded, |ui| {
            ui.allocate_ui(
                egui::vec2(ui.available_width(), TRANSPORT_BAR_HEIGHT),
                |ui| {
                    render_transport_row(ui, state, live, playback, chrome);
                },
            );
        });

    // The row — just above the transport row.
    let mut expanded = shown;
    egui::Panel::bottom("mobile_mini_timeline")
        .resizable(false)
        .exact_size(MINI_AREA_HEIGHT)
        .frame(dock)
        .show_collapsible(root_ui, &mut expanded, |ui| {
            crate::ui::timeline::mini::render_mini_timeline(
                ui, state, timeline, sites, live, playback, chrome,
            );
        });
}

/// The transport row: play, the chip, the stop control and LIVE. The
/// trailing pair lays out first, right to left, so the chip knows the room
/// it has; the core keeps its words short enough for it.
fn render_transport_row(
    ui: &mut egui::Ui,
    state: &mut AppState,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    let vm = crate::core::transport::transport_vm(&playback.state, state.viz_state.tilt_role());
    let strip_view = chrome.phone_strip_view.map(|(w, _)| (w.start, w.end));
    let narrow = ui.available_width() < 380.0;
    let gap = if narrow { 6.0 } else { 10.0 };
    let edge = if narrow { 8.0 } else { 12.0 };
    let live_intents = live_button(live.session.now_cap, chrome.phone_now_in_view);

    ui.horizontal_centered(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(edge);
            ui.spacing_mut().item_spacing.x = gap;
            if let Some(intents) = &live_intents {
                if live_pill(ui).clicked() {
                    for intent in intents {
                        state.push_command(intent.clone());
                    }
                }
            }
            if let Some(stop) = stop_control(ui, chrome.phone_strip_zoom, narrow) {
                state.push_command(Intent::SetPhoneStripZoom(stop));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space(edge);
                ui.spacing_mut().item_spacing.x = gap;
                if play_button(ui, vm.play).clicked() {
                    toggle_play(state);
                }
                let room = ui.available_width() - gap;
                let clock = crate::ui::time_format::ZoneClock {
                    use_local: state.use_local_time,
                };
                let chip = phone_chip(
                    &live.session.selection_chip,
                    chrome.phone_selected_start,
                    chrome.phone_loading.is_some(),
                    &clock,
                    crate::ui::icons::ARROW_RIGHT,
                );
                // "Loop what's in view" loops the row's window on the phone.
                let view_start = playback.state.timeline_view_start;
                let view = crate::core::selection_chip::menu_view(
                    true,
                    (view_start, view_start + playback.state.view_width_secs()),
                    strip_view,
                );
                crate::ui::selection_chip::render_selection_chip(
                    ui,
                    state,
                    live,
                    playback,
                    view,
                    &crate::ui::selection_chip::ChipStyle {
                        text_size: 14.0,
                        min_height: PILL_H,
                        // The phone's stop leaves the playhead where it is;
                        // LIVE is the way back to now.
                        stop_placement: crate::core::phone_dock::PHONE_STOP_PLACEMENT,
                        compact: true,
                        max_width: Some(room),
                        phone: Some(chip),
                    },
                );
            });
        });
    });
}

/// The 42 px round play/pause button.
fn play_button(ui: &mut egui::Ui, play: PlayButton) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(PLAY_D, PLAY_D), Sense::click());
    let p = ui.painter();
    let c = rect.center();
    if resp.is_pointer_button_down_on() {
        p.circle_filled(c, PLAY_D / 2.0, theme::HOVER);
    }
    p.circle_stroke(c, PLAY_D / 2.0 - 0.75, Stroke::new(1.5, theme::TEXT));
    // Drawn, not a glyph: the icon font's play and pause are outlines.
    match play {
        PlayButton::Pause => {
            for dx in [-4.0, 4.0] {
                p.rect_filled(
                    egui::Rect::from_center_size(c + egui::vec2(dx, 0.0), egui::vec2(4.0, 14.0)),
                    1.0,
                    theme::TEXT,
                );
            }
        }
        PlayButton::Play => {
            p.add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(-4.5, -7.5),
                    c + egui::vec2(8.0, 0.0),
                    c + egui::vec2(-4.5, 7.5),
                ],
                theme::TEXT,
                Stroke::NONE,
            ));
        }
    }
    resp.on_hover_text(match play {
        PlayButton::Pause => "Pause",
        PlayButton::Play => "Play",
    })
}

/// NOW · 3 H · 24 H: which stop the row is at, and a tap to go to another.
/// Returns the stop tapped, when it is not the current one. Laid out right
/// to left, so the segments are placed from the last.
fn stop_control(ui: &mut egui::Ui, current: StripZoom, narrow: bool) -> Option<StripZoom> {
    let font = FontId::monospace(10.5);
    let pad = if narrow { 5.0 } else { 7.0 };
    let seg_h = 20.0;
    let widths: Vec<f32> = StripZoom::ALL
        .iter()
        .map(|s| {
            ui.painter()
                .layout_no_wrap(zoom_pill_label(*s).to_string(), font.clone(), theme::TEXT)
                .size()
                .x
                + 2.0 * pad
        })
        .collect();
    let total = widths.iter().sum::<f32>() + 2.0 * (StripZoom::ALL.len() as f32) + 2.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total, seg_h + 6.0), Sense::hover());
    let p = ui.painter().clone();
    p.rect(
        rect,
        6.0,
        colors::CONTROL,
        Stroke::new(1.0, theme::LINE),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 3.0;
    let mut picked = None;
    for (stop, w) in StripZoom::ALL.iter().zip(widths) {
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 3.0), egui::vec2(w, seg_h));
        let resp = ui
            .interact(
                seg.expand2(egui::vec2(1.0, 9.0)),
                ui.id().with(("phone_stop", zoom_pill_label(*stop))),
                Sense::click(),
            )
            .on_hover_text("Zoom: now, 3 hours or the day");
        let on = *stop == current;
        if on {
            p.rect_filled(seg, 4.0, colors::CONTROL_ON);
        }
        p.text(
            seg.center(),
            egui::Align2::CENTER_CENTER,
            zoom_pill_label(*stop),
            font.clone(),
            if on { theme::TEXT } else { theme::TEXT_DIM },
        );
        if resp.clicked() && !on {
            picked = Some(*stop);
        }
        x += w + 2.0;
    }
    picked
}

/// The red outlined LIVE button.
fn live_pill(ui: &mut egui::Ui) -> egui::Response {
    let font = FontId::proportional(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap("LIVE".to_string(), font, colors::NOW);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(galley.size().x + 28.0, PILL_H), Sense::click());
    let p = ui.painter();
    if resp.is_pointer_button_down_on() {
        p.rect_filled(rect, PILL_H / 2.0, theme::alpha(colors::NOW, 40));
    }
    p.rect_stroke(
        rect,
        PILL_H / 2.0,
        Stroke::new(1.5, colors::NOW),
        StrokeKind::Inside,
    );
    let at = rect.center() - galley.size() / 2.0;
    p.galley(at, galley, colors::NOW);
    resp.on_hover_text("Back to the newest frame")
}

// ---------------------------------------------------------------------------
// Playback helpers.
// ---------------------------------------------------------------------------

fn toggle_play(state: &mut AppState) {
    // Same decoupled play/pause as desktop: in live this freezes the feed;
    // stopping the stream is the chip menu's job.
    state.push_command(crate::core::Intent::TogglePlayPause);
}
