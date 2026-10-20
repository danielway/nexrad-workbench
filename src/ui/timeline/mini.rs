//! The phone's timeline row (phone timeline v2): a row of volumes you can
//! touch, a thin projection of [`crate::core::phone_row::row_vm`].
//!
//! Its window is [`crate::core::phone_strip::strip_window`] at one of three
//! stops (NOW, 3 H, 24 H). At NOW each volume is a block (filled on the
//! device, hollow not, a white outline under the playhead, the live one
//! growing to the red now line); at 3 H a column as tall as its weather;
//! at 24 H the storm hill with the held stretch in slate and a white line
//! at the playhead. A selected range is one amber bracket above. While the
//! frame under the playhead downloads, its block carries a progress arc;
//! the row hands the same [`crate::core::phone_row::LoadingVm`] to the map's
//! dim and pill, the chip and the top bar through
//! `chrome.phone_loading`.
//!
//! A drag scrubs ([`crate::core::phone_strip::snap_scrub`]: cut by cut in a
//! block on the device, volume by volume elsewhere) under a window that
//! holds still; a tap goes to the volume's first cut
//! ([`crate::core::phone_strip::tap_landing`]); a long-press opens the
//! action sheet; a pinch moves one stop
//! ([`crate::core::phone_strip::StripTouch`]). The strip and its gestures
//! sit in an area that reaches [`STRIP_REACH_PX`] above it into the map, so
//! a thumb that lands just short still gets the strip, and the map neither
//! pans on that press nor zooms on the strip's pinch.

use super::reduced_motion;
use crate::core::phone_range::{press_at, RangeFacts};
use crate::core::phone_row::{
    row_vm, Arc, Downloads, RowInputs, RowVm, BLOCK_H, BLOCK_TOP, BRACKET_H, BRACKET_TICK_H,
    BRACKET_TOP, COLUMN_BOTTOM, HILL_BOTTOM, HILL_H, NOW_BOTTOM, NOW_CAP_TOP, NOW_TOP, ROW_H,
};
use crate::core::phone_strip::{
    sizes_span, snap_scrub, strip_window, tap_landing, PinchInput, StripTouch, StripTouchInput,
    StripUnits, StripZoom, STRIP_REACH_PX,
};
use crate::core::transport::SeekGesture;
use crate::core::{FrameJoinInputs, Intent};
use crate::data::SiteId;
use crate::state::AppState;
use crate::ui::theme;
use crate::ui::theme::phone as colors;
use eframe::egui::{self, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

/// The row's height.
pub(crate) const MINI_TIMELINE_H: f32 = ROW_H;

/// Room kept at each end of the lane.
const LANE_PAD: f32 = 6.0;

/// The labels' font.
fn label_font() -> FontId {
    FontId::proportional(11.5)
}

/// How far back the window's volumes are looked for: past the widest
/// stop's half-span either side of the playhead.
fn lookback_secs(stop: StripZoom) -> f64 {
    match stop {
        StripZoom::Cuts | StripZoom::Volumes => 6.0 * 3600.0,
        StripZoom::Day => 26.0 * 3600.0,
    }
}

/// Draw the row at the available width and apply its gestures.
#[allow(clippy::too_many_arguments)]
pub(in crate::ui) fn render_mini_timeline(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(Vec2::new(width, MINI_TIMELINE_H), Sense::hover());
    // The strip draws in an area above the map's layer so its reach into
    // the map takes the press, clipped like the panel (it slides with it).
    let panel_clip = ui.clip_rect();
    let reach = Rect::from_min_max(
        Pos2::new(strip.left(), strip.top() - STRIP_REACH_PX),
        strip.max,
    );
    let clip = Rect::from_min_max(
        Pos2::new(panel_clip.left(), panel_clip.top() - STRIP_REACH_PX),
        panel_clip.max,
    );
    egui::Area::new(ui.id().with("mini_strip"))
        .order(egui::Order::Middle)
        .fixed_pos(reach.min)
        .constrain(false)
        .interactable(true)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(clip);
            draw_strip(ui, strip, state, timeline, live, playback, chrome);
        });
}

#[allow(clippy::too_many_arguments)]
fn draw_strip(
    ui: &mut egui::Ui,
    strip: Rect,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    let stop = chrome.phone_strip_zoom;
    let top = strip.top();
    let lane = Rect::from_min_max(
        Pos2::new(strip.left() + LANE_PAD, top),
        Pos2::new(strip.right() - LANE_PAD, strip.bottom()),
    );
    let press_rect = Rect::from_min_max(Pos2::new(strip.left(), top - STRIP_REACH_PX), strip.max);
    let response = ui.allocate_rect(press_rect, Sense::click_and_drag());
    let painter = ui.painter().clone();
    painter.rect_filled(strip, 0.0, colors::DOCK);
    painter.line_segment(
        [strip.left_top(), strip.right_top()],
        Stroke::new(1.0_f32, theme::LINE),
    );

    let now = state.frame_now.secs();
    let view = crate::core::TimelineView::build(
        &timeline.scans,
        &timeline.shadow_scan_boundaries,
        Some(&live.mode_state),
        live.radar_model.position.as_ref(),
    );
    // The same join the desktop strip makes: queued, in flight, the
    // selected product and tilt, and the frame on screen.
    let failed: Vec<i64> = Vec::new();
    let mut in_flight = state.download_progress.in_flight_scans.clone();
    in_flight.extend_from_slice(&state.download_progress.active_scans);
    let active = state.viz_state.displayed.as_ref().map(|d| {
        (
            d.identity.scan_timestamp_secs(),
            d.identity.elevation_number,
        )
    });
    let join = FrameJoinInputs {
        queued: &state.download_progress.pending_scans,
        in_flight: &in_flight,
        failed: &failed,
        product: state.viz_state.product.to_worker_string(),
        tilt: state.viz_state.elevation_selection.elevation_number(),
        active,
        prev_active: None,
    };

    let playhead = playback.state.playback_position();
    let pinned = playback.state.is_pinned() || playback.state.is_live_range();
    let look_from = now.min(playhead) - lookback_secs(stop);
    let all = view.frame_containers_in_range(look_from, now.max(playhead) + 3600.0, join);
    let starts: Vec<f64> = all.iter().map(|c| c.start_secs).collect();
    let prev = chrome.phone_strip_view.map(|(w, _)| w);
    // The selected range as the desktop strip draws it (its end on now
    // when pinned): the window shows all of it, and the bracket marks it.
    let range = crate::core::timeline_view::range_marks(
        playback.state.selection_range(),
        playback.state.is_live_range(),
        playback.state.selection_in_progress(),
        now,
        crate::core::timeline_view::RangeCost::default(),
    )
    .map(|m| (m.start, m.end));
    let window = strip_window(stop, now, playhead, pinned, &starts, prev, range);
    chrome.phone_strip_view = Some((window, lane.width()));
    let containers: Vec<_> = all
        .into_iter()
        .filter(|c| c.end_secs >= window.start && c.start_secs <= window.end)
        .collect();

    let site = SiteId::new(state.viz_state.site_id.clone());
    let (sizes_from, sizes_to) = sizes_span(stop, window);
    let sizes = timeline.activity.scans(&site, sizes_from, sizes_to);
    let units = StripUnits::from_containers(&containers).with_listed(&sizes.scans);
    let secs_per_px = window.span() / lane.width().max(1.0) as f64;
    let x_to_ts = |x: f32| window.start + (x - lane.left()) as f64 * secs_per_px;

    let font = label_font();
    let measure = |s: &str| {
        painter
            .layout_no_wrap(s.to_string(), font.clone(), theme::TEXT)
            .size()
            .x
    };
    let row = row_vm(&RowInputs {
        stop,
        window,
        lane: (lane.left(), lane.right()),
        now,
        playhead,
        pinned,
        active_ts: active.map(|(ts, _)| ts),
        containers: &containers,
        sizes: &sizes,
        range,
        downloads: Downloads {
            queued: &state.download_progress.pending_scans,
            downloading: &state.download_progress.active_scans,
            decoding: &state.download_progress.in_flight_scans,
        },
        tz_offset_secs: super::tz_offset_secs(
            window.start + window.span() / 2.0,
            state.use_local_time,
        ),
        measure: &measure,
    });
    chrome.phone_loading = row.loading.clone();
    chrome.phone_now_in_view = row.now_x.is_some();
    chrome.phone_selected_start = row.selected_start;

    let time = ui.ctx().input(|i| i.time);
    let reduced = reduced_motion(ui.ctx());
    let lane_painter = painter.with_clip_rect(lane.expand2(Vec2::new(LANE_PAD, 0.0)));
    paint_row(&lane_painter, top, &row, &font, time, reduced);
    if !reduced
        && (row.loading.is_some()
            || state.download_progress.is_active()
            || view.live_volume().is_some())
    {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(if row.loading.is_some() {
                33
            } else {
                250
            }));
    }

    // Gestures.
    let key = response.id.with("mini_touch");
    let mut touch: StripTouch = ui
        .ctx()
        .memory(|m| m.data.get_temp(key))
        .unwrap_or_default();
    let gesture_area = Rect::from_min_max(
        Pos2::new(strip.left(), strip.top() - STRIP_REACH_PX),
        strip.max,
    );
    let pinch = ui.input(|i| i.multi_touch()).map(|t| PinchInput {
        zoom_delta: t.zoom_delta,
        began_on_strip: gesture_area.contains(t.start_pos),
    });
    let pos = response.interact_pointer_pos();
    let out = touch.step(StripTouchInput {
        down: response.is_pointer_button_down_on(),
        pos: pos.map(|p| (p.x, p.y)),
        now: time,
        pinch,
        stop,
    });
    ui.ctx().memory_mut(|m| m.data.insert_temp(key, touch));
    chrome.phone_strip_claims_touch |= out.claimed;
    if let Some(next) = out.settle {
        state.push_command(Intent::SetPhoneStripZoom(next));
    }
    if out.press.waiting || out.claimed {
        ui.ctx().request_repaint();
    }
    if let Some((x, _)) = out.press.long_press {
        let ts = x_to_ts(x);
        state.push_command(Intent::OpenPressSheet(press_at(
            stop,
            ts,
            &units,
            view.scan_start_at(ts),
            &RangeFacts::of(&playback.state, now),
        )));
        return;
    }
    state.pointer_scrub_active = response.dragged() && out.may_seek;
    if !out.may_seek {
        return;
    }
    if let Some(p) = pos {
        // A still finger does not seek again (the playhead holds while the
        // strip changes under it).
        let moved = response.dragged() && touch.scrub_moved(p.x);
        ui.ctx().memory_mut(|m| m.data.insert_temp(key, touch));
        if moved {
            // A scrub runs the seek reducer synchronously so the playhead
            // keeps up with the finger.
            live.seek(
                &mut playback.state,
                now,
                state.pause_stream_while_reviewing,
                snap_scrub(stop, x_to_ts(p.x), &units, now),
                SeekGesture::Scrub,
            );
        } else if response.clicked() && !response.dragged() {
            // A tap goes to the volume: its first cut.
            state.push_command(Intent::Seek {
                ts: tap_landing(x_to_ts(p.x), &units, now),
                gesture: SeekGesture::Press,
            });
        }
    }
}

/// Paint the row's view-model: marks, labels, the bracket and the now line.
fn paint_row(p: &egui::Painter, top: f32, row: &RowVm, font: &FontId, time: f64, reduced: bool) {
    let white = theme::SEEN;
    for b in &row.blocks {
        let r = Rect::from_min_max(
            Pos2::new(b.x0, top + BLOCK_TOP),
            Pos2::new(b.x1, top + BLOCK_TOP + BLOCK_H),
        );
        let radius = 6.0_f32.min(r.width() / 2.0);
        let outline = if b.selected {
            Stroke::new(2.0_f32, white)
        } else if b.on_device {
            Stroke::new(1.5_f32, colors::SLATE)
        } else {
            Stroke::new(1.5_f32, colors::SLATE_HI)
        };
        let fill = if b.selected {
            colors::SLATE_HI
        } else {
            colors::SLATE
        };
        if b.live {
            // Open at the now line: square on the right, filled by its cuts.
            let corners = CornerRadius {
                nw: radius as u8,
                sw: radius as u8,
                ne: 0,
                se: 0,
            };
            let filled =
                Rect::from_min_max(r.min, Pos2::new(r.left() + r.width() * b.fill, r.bottom()));
            if filled.width() > 0.0 {
                p.rect_filled(filled, corners, fill);
            }
            p.rect_stroke(r, corners, outline, StrokeKind::Inside);
        } else {
            if b.on_device {
                p.rect_filled(r, radius, fill);
            }
            if b.selected || !b.on_device {
                p.rect_stroke(r, radius, outline, StrokeKind::Inside);
            }
        }
        for &x in &b.cuts {
            p.line_segment(
                [Pos2::new(x, r.top() + 4.0), Pos2::new(x, r.bottom() - 4.0)],
                Stroke::new(1.0_f32, colors::cut()),
            );
        }
        if let Some(arc) = b.arc {
            paint_arc(p, r.center(), 7.0, arc, time, reduced);
        }
    }
    for c in &row.columns {
        let r = Rect::from_min_max(
            Pos2::new(c.x0, top + COLUMN_BOTTOM - c.height),
            Pos2::new(c.x1, top + COLUMN_BOTTOM),
        );
        let corners = CornerRadius {
            nw: 2,
            ne: 2,
            sw: 0,
            se: 0,
        };
        if c.selected {
            p.rect_filled(r, corners, white);
        } else if c.on_device {
            p.rect_filled(r, corners, colors::SLATE);
        } else {
            p.rect_stroke(
                r,
                corners,
                Stroke::new(1.0_f32, colors::SLATE_HI),
                StrokeKind::Inside,
            );
        }
    }
    if !row.hill.is_empty() {
        // One quad per sliver, all in one mesh: the area is not convex, so
        // it is never one polygon.
        let base = top + HILL_BOTTOM;
        let mut mesh = egui::Mesh::default();
        for h in &row.hill {
            let color = if h.on_device {
                colors::SLATE_HI
            } else {
                colors::HILL
            };
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(Pos2::new(h.x0, base), color);
            mesh.colored_vertex(Pos2::new(h.x0, base - h.h0), color);
            mesh.colored_vertex(Pos2::new(h.x1, base - h.h1), color);
            mesh.colored_vertex(Pos2::new(h.x1, base), color);
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        }
        p.add(egui::Shape::mesh(mesh));
    }
    if let Some(x) = row.playhead_x {
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(x - 1.0, top + HILL_BOTTOM - HILL_H),
                Pos2::new(x + 1.0, top + HILL_BOTTOM),
            ),
            0.0,
            white,
        );
    }
    for l in &row.labels {
        p.text(
            Pos2::new(l.x, top + l.top),
            egui::Align2::LEFT_TOP,
            &l.text,
            font.clone(),
            if l.bright {
                theme::TEXT
            } else {
                theme::TEXT_DIM
            },
        );
    }
    if let Some(b) = row.bracket {
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(b.x0, top + BRACKET_TOP),
                Pos2::new(b.x1, top + BRACKET_TOP + BRACKET_H),
            ),
            1.5,
            theme::LOOP,
        );
        let tick = |x0: f32| {
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x0, top + BRACKET_TOP),
                    Pos2::new(x0 + 2.0, top + BRACKET_TOP + BRACKET_TICK_H),
                ),
                0.0,
                theme::LOOP,
            );
        };
        if b.start_tick {
            tick(b.x0);
        }
        if b.end_tick {
            tick(b.x1 - 2.0);
        }
    }
    if let Some(x) = row.now_x {
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(x - 1.0, top + NOW_TOP),
                Pos2::new(x + 1.0, top + NOW_BOTTOM),
            ),
            0.0,
            colors::NOW,
        );
        let cap = p.layout_no_wrap("NOW".to_string(), FontId::proportional(9.0), colors::NOW);
        let clip = p.clip_rect();
        let left = (x - cap.size().x / 2.0)
            .min(clip.right() - cap.size().x - 1.0)
            .max(clip.left());
        p.galley(Pos2::new(left, top + NOW_CAP_TOP - 2.0), cap, colors::NOW);
    }
}

/// The progress arc on a loading block (and in the map's pill): a faint
/// track and a white arc from twelve o'clock, or a spinning third while
/// nothing is known.
pub(in crate::ui) fn paint_arc(
    p: &egui::Painter,
    center: Pos2,
    r: f32,
    arc: Arc,
    time: f64,
    reduced: bool,
) {
    use std::f32::consts::{FRAC_PI_2, TAU};
    p.circle_stroke(center, r, Stroke::new(2.0_f32, colors::arc_track()));
    let (from, sweep) = match arc {
        Arc::Share(s) => (-FRAC_PI_2, s.clamp(0.0, 1.0) * TAU),
        Arc::Spinning => {
            let turn = if reduced {
                0.0
            } else {
                (time * 1.6).fract() as f32 * TAU
            };
            (-FRAC_PI_2 + turn, 0.3 * TAU)
        }
    };
    let steps = ((sweep / TAU) * 32.0).ceil().max(2.0) as usize;
    let pts: Vec<Pos2> = (0..=steps)
        .map(|k| {
            let a = from + sweep * k as f32 / steps as f32;
            center + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(2.0_f32, theme::SEEN)));
}
