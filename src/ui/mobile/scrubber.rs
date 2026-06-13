//! Compact mobile scrubber.
//!
//! A horizontal coverage track (~44px hit area, spec §13 phone) that shares the
//! desktop timeline's visual language:
//!   - Spans the union of cached and archive-listed data on the width
//!   - Solid steel-blue segments = data on the device
//!   - Faint slate segments = available in the cloud archive
//!   - Neutral draggable thumb = playback position
//!   - In live mode, a red "now" marker
//!
//! Interaction: tap-to-seek, drag-to-scrub (long-press opens the inspector).
//! Scrubbing pauses playback so the thumb stays where the user put it. The
//! whole strip is a 44pt-tall touch target per the spec, even though the
//! painted bar is thin — the surrounding height is hittable.

use crate::state::AppState;
use crate::ui::colors::timeline as tl_colors;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

/// Total scrubber height in egui logical pixels. Sized to the 44pt touch-target
/// guidance (spec §13 phone: "~44px strip") so the whole hit area is thumb-safe.
pub(super) const SCRUBBER_HEIGHT: f32 = 44.0;

pub(super) fn render_scrubber(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline: &crate::subsystem::Timeline,
    live: &mut crate::subsystem::Live,
    playback: &mut crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
) {
    let available_w = ui.available_width();
    let (response, painter) = ui.allocate_painter(
        Vec2::new(available_w, SCRUBBER_HEIGHT),
        Sense::click_and_drag(),
    );
    let full_rect = response.rect;

    // Track rect — a low-profile bar centered in the tall (44pt) hit area so
    // the surrounding height stays hittable for easy touch targeting while the
    // painted bar keeps the calm coverage-strip look.
    let track_y = full_rect.center().y;
    let track_rect = Rect::from_min_max(
        Pos2::new(full_rect.left() + 8.0, track_y - 4.0),
        Pos2::new(full_rect.right() - 8.0, track_y + 4.0),
    );
    let dark = state.is_dark;
    painter.rect_filled(track_rect, 2.0, tl_colors::background(dark));

    // Find the time range to render: the union of cached data and the
    // archive listing, so not-yet-downloaded data is visible too. Fall
    // back to a 1-hour window centered on `now` so the track isn't
    // zero-width.
    let mut data_range = timeline.scans.overall_time_range();
    for b in &timeline.shadow_scan_boundaries {
        data_range = match data_range {
            Some((s, e)) => Some((s.min(b.start as f64), e.max(b.end as f64))),
            None => Some((b.start as f64, b.end as f64)),
        };
    }
    let frame_now = state.frame_now.secs();
    // While streaming, the track must reach "now" so the live edge (and its
    // growth) stays visible even when the cached range trails behind.
    if live.mode_state.is_active() {
        data_range = data_range.map(|(s, e)| (s, e.max(frame_now)));
    }
    let (t_start, t_end) = match data_range {
        Some((s, e)) if e > s => (s, e),
        _ => (frame_now - 1800.0, frame_now + 1800.0),
    };
    let span = t_end - t_start;

    let ts_to_x = |ts: f64| -> f32 {
        let t = ((ts - t_start) / span).clamp(0.0, 1.0) as f32;
        track_rect.left() + t * track_rect.width()
    };
    let x_to_ts = |x: f32| -> f64 {
        let t = ((x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0) as f64;
        t_start + t * span
    };

    // Helper: paint a time span as a segment on the bar.
    let draw_segment = |start: f64, end: f64, color: Color32| {
        let x0 = ts_to_x(start);
        let x1 = ts_to_x(end).max(x0 + 2.0);
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, track_rect.top()),
                Pos2::new(x1.min(track_rect.right()), track_rect.bottom()),
            ),
            2.0,
            color,
        );
    };

    // Available (in cloud archive, not downloaded) — faint slate, same
    // meaning as the desktop timeline's hollow blocks. Drawn first so
    // cached segments paint over any overlap.
    {
        // The same suppress-where-cached rule the desktop timeline gets
        // from TimelineView, without the live-merge machinery.
        let view = crate::state::TimelineView::build(
            &timeline.scans,
            &timeline.shadow_scan_boundaries,
            None,
            None,
            None,
            frame_now,
        );
        for b in view.shadow_boundaries() {
            if view.is_covered_by_cached(b.start) {
                continue;
            }
            draw_segment(
                b.start as f64,
                b.end as f64,
                tl_colors::available_border(dark),
            );
        }
    }

    // Cached (on device) — solid steel blue.
    for range in timeline.scans.time_ranges() {
        draw_segment(range.start, range.end, tl_colors::cached_fill(dark, false));
    }

    let playback_ts = playback.state.playback_position();
    let thumb_x = ts_to_x(playback_ts);

    // "Now" marker in live mode.
    if live.mode_state.is_active() {
        let now = frame_now;
        if now >= t_start && now <= t_end {
            let x = ts_to_x(now);
            painter.line_segment(
                [
                    Pos2::new(x, full_rect.top() + 2.0),
                    Pos2::new(x, full_rect.bottom() - 2.0),
                ],
                Stroke::new(1.5, tl_colors::LIVE_ACTIVE),
            );
        }
    }

    // Thumb — the neutral playback needle, drawn last so it sits on top.
    let thumb_r = 8.0;
    painter.circle_filled(
        Pos2::new(thumb_x, track_y),
        thumb_r,
        tl_colors::selection(dark),
    );
    painter.circle_stroke(
        Pos2::new(thumb_x, track_y),
        thumb_r,
        Stroke::new(1.5, tl_colors::background(dark)),
    );

    // Outline for the whole hit area (subtle so it reads as draggable).
    painter.rect_stroke(
        full_rect,
        4.0,
        Stroke::new(0.5, Color32::from_rgba_unmultiplied(128, 128, 128, 40)),
        StrokeKind::Inside,
    );

    // Rejoin pill — while a background stream ingests with the playhead
    // detached, a tap re-pins to the live edge instantly (mobile twin of
    // the desktop now-cap's REJOIN state). Owns its taps: a tap on the
    // pill is never also a track seek.
    let mut pill_rect: Option<Rect> = None;
    if live.is_detached(&playback.state) {
        let label = "REJOIN";
        let font = egui::FontId::proportional(9.0);
        let galley = painter.layout_no_wrap(label.to_string(), font.clone(), Color32::WHITE);
        let pad = Vec2::new(6.0, 2.5);
        let size = galley.size() + pad * 2.0;
        let rect = Rect::from_min_size(
            Pos2::new(
                full_rect.right() - 4.0 - size.x,
                full_rect.top() + (full_rect.height() - size.y) / 2.0,
            ),
            size,
        );
        let pulse = live.mode_state.pulse_alpha();
        let alpha = (200.0 + 55.0 * pulse) as u8;
        let fill = Color32::from_rgba_unmultiplied(
            tl_colors::LIVE_ACTIVE.r(),
            tl_colors::LIVE_ACTIVE.g(),
            tl_colors::LIVE_ACTIVE.b(),
            alpha,
        );
        painter.rect_filled(rect, 3.0, fill);
        painter.galley(rect.min + pad, galley, Color32::WHITE);
        pill_rect = Some(rect);
    }

    // Long-press → scan inspector (spec §12, mobile twin of the desktop
    // right-click). Checked before seek so a held press opens the inspector
    // instead of dropping the playhead. Resolves the press x to a scan-start
    // via the merged view, then suppresses this frame's seek.
    if let Some(pos) = crate::ui::long_press::detect(ui.ctx(), &response, response.id) {
        let ts = x_to_ts(pos.x);
        let view = crate::state::TimelineView::build(
            &timeline.scans,
            &timeline.shadow_scan_boundaries,
            Some(&live.mode_state),
            live.radar_model.position.as_ref(),
            None,
            frame_now,
        );
        if let Some(scan_start) = view.scan_start_at(ts) {
            chrome.scan_inspector = Some(scan_start);
        }
        return;
    }

    // Interaction: drag or click to seek (or tap the rejoin pill).
    let interact_pos = response
        .interact_pointer_pos()
        .filter(|_| response.dragged() || response.clicked());
    if let Some(pos) = interact_pos {
        if response.clicked() && pill_rect.is_some_and(|r| r.contains(pos)) {
            state.push_command(crate::state::AppCommand::ReturnToLive);
            return;
        }
        let new_ts = x_to_ts(pos.x);
        // Detach the playhead on a manual seek — the stream (if running)
        // keeps ingesting in the background unless the data-saver policy
        // stops it.
        live.detach_playhead(
            &mut playback.state,
            frame_now,
            state.pause_stream_while_reviewing,
        );
        // Scrubbing pauses playback so the thumb stays where the user dropped
        // it — otherwise a running playback loop would immediately snap it
        // forward on the next frame.
        if response.dragged() {
            playback.state.playing = false;
        }
        playback.state.set_playback_position(new_ts);
    }
}
