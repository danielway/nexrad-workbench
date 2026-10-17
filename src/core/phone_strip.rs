//! The phone strip's zoom stops (phone timeline §1): Cuts, Volumes and Day.
//!
//! Continuous zoom suits a wheel, not a thumb, so the phone's mini timeline
//! has three stops, each with its own window rule and scrub unit:
//!
//! - **Cuts** — today's strip ([`crate::core::phone::mini_window`]): the
//!   last three volumes and the ghost of the next. A scrub snaps to cuts.
//! - **Volumes** — three hours, one cell per volume; a scrub snaps to
//!   volumes.
//! - **Day** — 24 hours of storm bars, one per volume sized by its file
//!   size from the day's listing (the desktop overview's zoomed-in path),
//!   with the on-device underline and an hour ruler. A scrub snaps to
//!   volumes.
//!
//! At Volumes and Day the window is centered on the playhead and slides with
//! it (there is no pan gesture), but never shows more than the ghost room
//! past now, so pinned the strip ends where Cuts does. Landscape keeps the
//! spans and gets more pixels per unit.
//!
//! A pinch moves one stop and settles on release ([`PinchTrack`]); the zoom
//! pill names the stop and taps through them ([`zoom_pill_label`],
//! [`next_stop`]). Zooming never touches the selection; going live returns
//! to Cuts ([`strip_zoom_after`]).
//!
//! All pure; `ui::timeline::mini` draws what these return.

use crate::core::phone::{mini_lead_secs, mini_window, MiniWindow, PressOut, PressTrack};
use crate::core::storm_activity::ScanActivity;
use crate::core::timeline_view::{FrameCell, FrameCellState, ScanContainer};
use crate::core::Intent;

/// The Volumes stop's span: three hours.
pub(crate) const VOLUMES_SPAN_SECS: f64 = 3.0 * 3600.0;

/// The Day stop's span: 24 hours.
pub(crate) const DAY_SPAN_SECS: f64 = 24.0 * 3600.0;

/// How far above the strip (points) a press still lands on it, so a thumb
/// finds it without aiming. The map does not see that press.
pub(crate) const STRIP_REACH_PX: f32 = 12.0;

/// A pinch whose fingers end this much further apart zooms in one stop.
pub(crate) const PINCH_IN_FACTOR: f32 = 1.25;

/// A pinch whose fingers end this much closer together zooms out one stop.
pub(crate) const PINCH_OUT_FACTOR: f32 = 0.8;

/// One of the strip's three zoom stops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StripZoom {
    /// The last three volumes, every cut.
    #[default]
    Cuts,
    /// Three hours, one cell per volume.
    Volumes,
    /// 24 hours of storm bars.
    Day,
}

impl StripZoom {
    /// One stop closer in (a pinch out): Day → Volumes → Cuts, stopping at
    /// Cuts.
    pub(crate) fn zoom_in(self) -> Self {
        match self {
            StripZoom::Day => StripZoom::Volumes,
            StripZoom::Volumes | StripZoom::Cuts => StripZoom::Cuts,
        }
    }

    /// One stop further out (a pinch in): Cuts → Volumes → Day, stopping at
    /// Day.
    pub(crate) fn zoom_out(self) -> Self {
        match self {
            StripZoom::Cuts => StripZoom::Volumes,
            StripZoom::Volumes | StripZoom::Day => StripZoom::Day,
        }
    }

    /// Whether the strip draws one cell (or bar) per volume, and so snaps a
    /// scrub to volumes rather than cuts.
    pub(crate) fn per_volume(self) -> bool {
        self != StripZoom::Cuts
    }
}

/// The zoom pill's word for a stop.
pub(crate) fn zoom_pill_label(stop: StripZoom) -> &'static str {
    match stop {
        StripZoom::Cuts => "CUTS",
        StripZoom::Volumes => "VOLS",
        StripZoom::Day => "DAY",
    }
}

/// The stop a tap on the pill goes to: Cuts → Volumes → Day → Cuts.
pub(crate) fn next_stop(stop: StripZoom) -> StripZoom {
    match stop {
        StripZoom::Cuts => StripZoom::Volumes,
        StripZoom::Volumes => StripZoom::Day,
        StripZoom::Day => StripZoom::Cuts,
    }
}

/// The stop after an intent. [`Intent::SetPhoneStripZoom`] sets it; going
/// live ([`Intent::GoLive`], [`Intent::ReturnToLive`]: the cap, the chip's
/// Go live) returns to Cuts, the one stop where a live volume filling cut by
/// cut is worth watching. Nothing else moves it: zoom never changes the
/// selection, and the selection never changes the zoom.
pub(crate) fn strip_zoom_after(stop: StripZoom, intent: &Intent) -> StripZoom {
    match intent {
        Intent::SetPhoneStripZoom(next) => *next,
        Intent::GoLive | Intent::ReturnToLive => StripZoom::Cuts,
        _ => stop,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Windows
// ───────────────────────────────────────────────────────────────────────────

/// The strip's window at a stop, in Unix seconds. `volume_starts` are the
/// containers' starts, oldest first.
///
/// Cuts is [`mini_window`]. Volumes and Day are centered on the playhead and
/// slide with it, clamped so the window never reaches past now plus the
/// ghost room Cuts leaves ([`mini_lead_secs`]): pinned, or near now, the
/// window ends there and now sits inside it. The span is the stop's in
/// either orientation; a wider strip only gets more pixels per unit.
pub(crate) fn strip_window(
    stop: StripZoom,
    now: f64,
    playhead: f64,
    pinned: bool,
    volume_starts: &[f64],
) -> MiniWindow {
    let span = match stop {
        StripZoom::Cuts => return mini_window(now, playhead, pinned, volume_starts),
        StripZoom::Volumes => VOLUMES_SPAN_SECS,
        StripZoom::Day => DAY_SPAN_SECS,
    };
    let center = if pinned { now } else { playhead };
    let end = (center + span / 2.0).min(now + mini_lead_secs(volume_starts));
    MiniWindow {
        start: end - span,
        end,
    }
}

/// The storm-activity listing the phone strip asks for: at Day, the window
/// in view and the lane's width, which decide (through
/// [`crate::core::storm_activity::needs_scans`]) that the day is listed
/// once for its volumes' sizes. `None` at the other stops, which draw the
/// strip's own cells.
pub(crate) fn activity_window(
    stop: StripZoom,
    window: MiniWindow,
    lane_px: f32,
) -> Option<(f64, f64, f64)> {
    (stop == StripZoom::Day && lane_px > 0.0).then_some((window.start, window.end, lane_px as f64))
}

// ───────────────────────────────────────────────────────────────────────────
// Scrub snapping
// ───────────────────────────────────────────────────────────────────────────

/// Something a scrub can land on: a cut, or a volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SnapUnit {
    pub start: f64,
    pub end: f64,
    /// Where the playhead goes: the frame the unit stands for, so it shows
    /// whole (a cut's end; a volume's first frame's end, or its start when
    /// its structure isn't known yet).
    pub land: f64,
}

/// The cuts and volumes on the strip, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StripUnits {
    pub cuts: Vec<SnapUnit>,
    pub volumes: Vec<SnapUnit>,
}

impl StripUnits {
    /// The units of the strip's containers. Projected cells and ghost
    /// volumes are not landing places: nothing is there yet.
    pub(crate) fn from_containers(containers: &[ScanContainer]) -> Self {
        let mut out = StripUnits::default();
        for c in containers.iter().filter(|c| !c.is_projected) {
            let cells: Vec<&FrameCell> = c
                .cells
                .iter()
                .filter(|cell| cell.state != FrameCellState::Projected)
                .collect();
            out.cuts.extend(cells.iter().map(|cell| SnapUnit {
                start: cell.start_secs,
                end: cell.end_secs.max(cell.start_secs),
                land: cell.end_secs.max(cell.start_secs),
            }));
            out.volumes.push(SnapUnit {
                start: c.start_secs,
                end: c.end_secs.max(c.start_secs),
                land: cells
                    .first()
                    .map_or(c.start_secs, |cell| cell.end_secs.max(cell.start_secs)),
            });
        }
        out.cuts.sort_by(|a, b| a.start.total_cmp(&b.start));
        out.volumes.sort_by(|a, b| a.start.total_cmp(&b.start));
        out
    }

    /// Add the volumes a day's listing knows of that no container covers
    /// (the Day stop's bars reach days the strip has no containers for).
    pub(crate) fn with_listed(mut self, listed: &[ScanActivity]) -> Self {
        let known = self.volumes.clone();
        self.volumes.extend(
            listed
                .iter()
                .filter(|s| !known.iter().any(|v| s.start >= v.start && s.start < v.end))
                .map(|s| SnapUnit {
                    start: s.start,
                    end: s.end.max(s.start),
                    land: s.start,
                }),
        );
        self.volumes.sort_by(|a, b| a.start.total_cmp(&b.start));
        self
    }
}

/// Where a scrub at `ts` puts the playhead at `stop`: the landing time of
/// the unit under the finger (cuts at Cuts, volumes at Volumes and Day), or
/// of the nearest unit when the finger is in a gap, never past `now`. With
/// no units on the strip the scrub goes where the finger is.
pub(crate) fn snap_scrub(stop: StripZoom, ts: f64, units: &StripUnits, now: f64) -> f64 {
    let list = if stop.per_volume() {
        &units.volumes
    } else {
        &units.cuts
    };
    let distance = |u: &SnapUnit| {
        if ts < u.start {
            u.start - ts
        } else if ts >= u.end {
            ts - u.end
        } else {
            0.0
        }
    };
    list.iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .map_or(ts, |u| u.land.min(now))
}

// ───────────────────────────────────────────────────────────────────────────
// Pinch
// ───────────────────────────────────────────────────────────────────────────

/// One frame of a two-finger gesture, as the strip reads it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PinchInput {
    /// This frame's change in finger spread (1.0 = none, >1 apart).
    pub zoom_delta: f32,
    /// The gesture began over the strip or its reach above it.
    pub began_on_strip: bool,
}

/// A two-finger gesture in progress. Whose it is (the strip's or the map's)
/// is decided when it begins and kept until both fingers lift.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PinchTrack {
    /// The spread so far, as a factor of the spread at the start.
    factor: f32,
    /// The pinch began on the strip: it is the strip's, not the map's.
    owned: bool,
}

/// What a pinch did this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PinchOut {
    /// The strip owns a pinch in progress: the map must not zoom, and any
    /// press on the strip is cancelled.
    pub claimed: bool,
    /// The pinch ended this frame and settles on this stop (only when it
    /// differs from the current one).
    pub settle: Option<StripZoom>,
}

/// The stop a pinch that ended at `factor` settles on: one stop in past
/// [`PINCH_IN_FACTOR`], one out below [`PINCH_OUT_FACTOR`], else where it
/// was.
pub(crate) fn pinch_stop(stop: StripZoom, factor: f32) -> StripZoom {
    if factor > PINCH_IN_FACTOR {
        stop.zoom_in()
    } else if factor < PINCH_OUT_FACTOR {
        stop.zoom_out()
    } else {
        stop
    }
}

impl PinchTrack {
    /// Advance the gesture. `input` is `None` once fewer than two fingers
    /// are down, which is the release: the strip's pinch settles then, never
    /// in between.
    pub(crate) fn step(
        prev: Option<PinchTrack>,
        input: Option<PinchInput>,
        stop: StripZoom,
    ) -> (Option<PinchTrack>, PinchOut) {
        let Some(i) = input else {
            let settle = prev
                .filter(|p| p.owned)
                .map(|p| pinch_stop(stop, p.factor))
                .filter(|s| *s != stop);
            return (
                None,
                PinchOut {
                    claimed: false,
                    settle,
                },
            );
        };
        let mut t = prev.unwrap_or(PinchTrack {
            factor: 1.0,
            owned: i.began_on_strip,
        });
        if i.zoom_delta.is_finite() && i.zoom_delta > 0.0 {
            t.factor *= i.zoom_delta;
        }
        (
            Some(t),
            PinchOut {
                claimed: t.owned,
                settle: None,
            },
        )
    }

    /// The spread so far.
    #[cfg(test)]
    pub(crate) fn factor(&self) -> f32 {
        self.factor
    }
}

/// The strip's touch state between frames: one press and one pinch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StripTouch {
    pub press: Option<PressTrack>,
    pub pinch: Option<PinchTrack>,
}

/// One frame of input on the strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StripTouchInput {
    /// A finger (or button) is down on the strip.
    pub down: bool,
    /// Where it is.
    pub pos: Option<(f32, f32)>,
    /// The frame clock, in seconds.
    pub now: f64,
    /// Two fingers are down somewhere.
    pub pinch: Option<PinchInput>,
    /// The stop on screen.
    pub stop: StripZoom,
}

/// What the strip's touch did this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StripTouchOut {
    /// The press's own outcome (long-press, waiting, swallowed release).
    pub press: PressOut,
    /// The strip owns a pinch: the map must not zoom or pan.
    pub claimed: bool,
    /// A pinch ended and the strip goes to this stop.
    pub settle: Option<StripZoom>,
    /// This frame may seek (a tap or a scrub): the press is neither a
    /// long-press nor taken over by a pinch.
    pub may_seek: bool,
}

impl StripTouch {
    /// Advance the press and the pinch together. A pinch the strip owns
    /// cancels the press under it (and any press that starts during it), so
    /// a pinch never seeks, scrubs or opens the inspector; the press stays
    /// cancelled until its finger lifts.
    pub(crate) fn step(&mut self, i: StripTouchInput) -> StripTouchOut {
        let (pinch, pinch_out) = PinchTrack::step(self.pinch, i.pinch, i.stop);
        self.pinch = pinch;
        let prev = self.press.map(|mut p| {
            if pinch_out.claimed {
                p.cancel();
            }
            p
        });
        let (mut press, mut press_out) = PressTrack::step(prev, i.down, i.pos, i.now);
        if pinch_out.claimed {
            if let Some(p) = press.as_mut() {
                if !p.cancelled() {
                    p.cancel();
                    // A long-press that would fire this frame is the pinch's.
                    press_out.long_press = None;
                }
            }
            press_out.waiting = false;
        }
        self.press = press;
        let may_seek = !pinch_out.claimed
            && !press_out.swallow_release
            && press_out.long_press.is_none()
            && press.is_none_or(|p| p.seeks());
        StripTouchOut {
            press: press_out,
            claimed: pinch_out.claimed,
            settle: pinch_out.settle,
            may_seek,
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// What each stop draws
// ───────────────────────────────────────────────────────────────────────────

/// A container drawn as one cell spanning the volume (the Volumes stop),
/// `gap_secs` short of its end so neighbouring volumes read apart. The cell
/// says the volume's most pressing state: failed, then filling, queued,
/// held (any cut cached), on the server, and the ghost last. It carries the
/// ring when any of its cuts is the frame on screen.
pub(crate) fn volume_cell(c: &ScanContainer, gap_secs: f64) -> ScanContainer {
    let has = |s: FrameCellState| c.cells.iter().any(|cell| cell.state == s);
    let state = [
        FrameCellState::Failed,
        FrameCellState::InFlight,
        FrameCellState::Queued,
        FrameCellState::Cached,
        FrameCellState::Available,
    ]
    .into_iter()
    .find(|s| has(*s))
    .unwrap_or(if c.is_projected {
        FrameCellState::Projected
    } else if c.is_available {
        FrameCellState::Available
    } else {
        FrameCellState::Cached
    });
    let first = c.cells.first();
    let end = (c.end_secs - gap_secs.max(0.0)).max(c.start_secs);
    ScanContainer {
        cells: vec![FrameCell {
            start_secs: c.start_secs,
            end_secs: end,
            elevation_number: first.map_or(0, |f| f.elevation_number),
            // No angle label at one cell per volume.
            elevation_angle: 0.0,
            state,
            chunks: None,
            is_active: c.cells.iter().any(|cell| cell.is_active),
            is_prev_active: false,
        }],
        sweep_spans: Vec::new(),
        vcp: 0,
        ..c.clone()
    }
}

/// One label on the strip's ruler.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RulerTick {
    pub ts: f64,
    pub label: String,
}

/// The ruler under the strip, in the chosen zone (`tz_offset_secs` east of
/// UTC; 0 for UTC): each volume's start (`HH:MM`) at Cuts, every hour
/// (`HH:00`) at Volumes, and every six hours (`00 06 12 18`) at Day.
pub(crate) fn ruler_ticks(
    stop: StripZoom,
    window: MiniWindow,
    volume_starts: &[f64],
    tz_offset_secs: i64,
) -> Vec<RulerTick> {
    let off = tz_offset_secs as f64;
    let clock = |ts: f64| {
        let secs = (ts + off).rem_euclid(86_400.0) as i64;
        (secs / 3600, (secs % 3600) / 60)
    };
    let every = |step: f64, label: &dyn Fn(i64, i64) -> String| {
        let mut out = Vec::new();
        let mut t = ((window.start + off) / step).ceil() * step - off;
        while t <= window.end {
            let (h, m) = clock(t);
            out.push(RulerTick {
                ts: t,
                label: label(h, m),
            });
            t += step;
        }
        out
    };
    match stop {
        StripZoom::Cuts => volume_starts
            .iter()
            .filter(|&&s| s >= window.start && s <= window.end)
            .map(|&s| {
                let (h, m) = clock(s);
                RulerTick {
                    ts: s,
                    label: format!("{h:02}:{m:02}"),
                }
            })
            .collect(),
        StripZoom::Volumes => every(3600.0, &|h, _| format!("{h:02}:00")),
        StripZoom::Day => every(6.0 * 3600.0, &|h, _| format!("{h:02}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::phone::{DEFAULT_VOLUME_SECS, MINI_LEAD_SHARE, MINI_VOLUMES};
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Volumes every 300 s up to `last`, oldest first.
    fn starts(first: f64, last: f64) -> Vec<f64> {
        let mut v = Vec::new();
        let mut t = first;
        while t <= last {
            v.push(t);
            t += 300.0;
        }
        v
    }

    const NOW: f64 = 1_000_000.0;

    fn lead() -> f64 {
        MINI_VOLUMES * DEFAULT_VOLUME_SECS * MINI_LEAD_SHARE
    }

    // ── stops, pill, cycle ────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn the_strip_starts_at_cuts() {
        assert_eq!(StripZoom::default(), StripZoom::Cuts);
    }

    #[wasm_bindgen_test]
    fn the_pill_names_the_stop_and_cycles_through_all_three() {
        assert_eq!(zoom_pill_label(StripZoom::Cuts), "CUTS");
        assert_eq!(zoom_pill_label(StripZoom::Volumes), "VOLS");
        assert_eq!(zoom_pill_label(StripZoom::Day), "DAY");
        assert_eq!(next_stop(StripZoom::Cuts), StripZoom::Volumes);
        assert_eq!(next_stop(StripZoom::Volumes), StripZoom::Day);
        assert_eq!(next_stop(StripZoom::Day), StripZoom::Cuts);
    }

    #[wasm_bindgen_test]
    fn zooming_in_and_out_stops_at_the_ends() {
        assert_eq!(StripZoom::Day.zoom_in(), StripZoom::Volumes);
        assert_eq!(StripZoom::Volumes.zoom_in(), StripZoom::Cuts);
        assert_eq!(StripZoom::Cuts.zoom_in(), StripZoom::Cuts);
        assert_eq!(StripZoom::Cuts.zoom_out(), StripZoom::Volumes);
        assert_eq!(StripZoom::Volumes.zoom_out(), StripZoom::Day);
        assert_eq!(StripZoom::Day.zoom_out(), StripZoom::Day);
    }

    // ── intents ───────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn going_live_returns_the_strip_to_cuts() {
        for stop in [StripZoom::Volumes, StripZoom::Day] {
            assert_eq!(strip_zoom_after(stop, &Intent::GoLive), StripZoom::Cuts);
            assert_eq!(
                strip_zoom_after(stop, &Intent::ReturnToLive),
                StripZoom::Cuts
            );
        }
    }

    #[wasm_bindgen_test]
    fn the_zoom_intent_sets_the_stop_and_nothing_else_moves_it() {
        assert_eq!(
            strip_zoom_after(StripZoom::Cuts, &Intent::SetPhoneStripZoom(StripZoom::Day)),
            StripZoom::Day
        );
        use crate::core::transport::SeekGesture;
        let others = [
            Intent::Seek {
                ts: 5.0,
                gesture: SeekGesture::Scrub,
            },
            Intent::TogglePlayPause,
            Intent::ClearLoop,
            Intent::SelectRange {
                start: 1.0,
                end: 2.0,
                to_live: false,
            },
            Intent::StopLive(crate::core::transport::LiveStopPlacement::InPlace),
        ];
        for intent in others {
            assert_eq!(
                strip_zoom_after(StripZoom::Day, &intent),
                StripZoom::Day,
                "{intent:?} must not move the zoom"
            );
        }
    }

    // ── windows ───────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn cuts_is_todays_mini_window() {
        let s = starts(NOW - 3_000.0, NOW);
        for (playhead, pinned) in [(NOW, true), (NOW - 400.0, false), (NOW - 50_000.0, false)] {
            assert_eq!(
                strip_window(StripZoom::Cuts, NOW, playhead, pinned, &s),
                mini_window(NOW, playhead, pinned, &s)
            );
        }
    }

    #[wasm_bindgen_test]
    fn far_back_volumes_and_day_center_on_the_playhead() {
        let s = starts(NOW - 200_000.0, NOW);
        let playhead = NOW - 150_000.0;
        let v = strip_window(StripZoom::Volumes, NOW, playhead, false, &s);
        assert_eq!(v.span(), VOLUMES_SPAN_SECS);
        assert_eq!((v.start + v.end) / 2.0, playhead);
        let d = strip_window(StripZoom::Day, NOW, playhead, false, &s);
        assert_eq!(d.span(), DAY_SPAN_SECS);
        assert_eq!((d.start + d.end) / 2.0, playhead);
    }

    #[wasm_bindgen_test]
    fn the_window_slides_with_the_playhead() {
        let s = starts(NOW - 200_000.0, NOW);
        let a = strip_window(StripZoom::Volumes, NOW, NOW - 50_000.0, false, &s);
        let b = strip_window(StripZoom::Volumes, NOW, NOW - 49_000.0, false, &s);
        assert_eq!(b.start - a.start, 1_000.0);
        assert_eq!(b.end - a.end, 1_000.0);
    }

    #[wasm_bindgen_test]
    fn pinned_the_window_ends_at_cuts_ghost_room() {
        let s = starts(NOW - 100_000.0, NOW);
        let cuts = strip_window(StripZoom::Cuts, NOW, NOW, true, &s);
        for stop in [StripZoom::Volumes, StripZoom::Day] {
            let w = strip_window(stop, NOW, NOW, true, &s);
            assert_eq!(w.end, cuts.end, "{stop:?} ends where Cuts does");
            assert!((w.end - (NOW + lead())).abs() < 1e-9);
            assert!(w.start < NOW && NOW < w.end, "now sits inside");
        }
        // A playhead that lags now a little while pinned changes nothing.
        let w = strip_window(StripZoom::Day, NOW, NOW - 3.0, true, &s);
        assert_eq!(w.end, NOW + lead());
    }

    #[wasm_bindgen_test]
    fn detached_near_now_clamps_so_nothing_past_the_ghost_room_shows() {
        let s = starts(NOW - 100_000.0, NOW);
        // Half an hour back: centered would reach an hour past now.
        let w = strip_window(StripZoom::Volumes, NOW, NOW - 1_800.0, false, &s);
        assert_eq!(w.end, NOW + lead());
        assert_eq!(w.span(), VOLUMES_SPAN_SECS);
        assert!(w.start < NOW - 1_800.0, "the playhead stays in view");
        // Two hours back is far enough to center.
        let w = strip_window(StripZoom::Volumes, NOW, NOW - 7_200.0, false, &s);
        assert_eq!((w.start + w.end) / 2.0, NOW - 7_200.0);
        assert!(w.end < NOW);
        // At Day, six hours back still clamps.
        let w = strip_window(StripZoom::Day, NOW, NOW - 6.0 * 3600.0, false, &s);
        assert_eq!(w.end, NOW + lead());
    }

    #[wasm_bindgen_test]
    fn the_ghost_room_follows_the_volume_length() {
        // VCP 35-ish: 10-minute volumes leave twice the room.
        let slow: Vec<f64> = (0..20).map(|i| NOW - 12_000.0 + i as f64 * 600.0).collect();
        let w = strip_window(StripZoom::Day, NOW, NOW, true, &slow);
        assert!((w.end - (NOW + MINI_VOLUMES * 600.0 * MINI_LEAD_SHARE)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn landscape_keeps_the_spans_and_gets_more_pixels_per_volume() {
        // The window has no width in it; a wider lane is more px per unit.
        let s = starts(NOW - 100_000.0, NOW);
        let w = strip_window(StripZoom::Day, NOW, NOW, true, &s);
        let px_per_volume = |lane_px: f64| lane_px / w.span() * DEFAULT_VOLUME_SECS;
        let portrait = px_per_volume(330.0);
        let landscape = px_per_volume(780.0);
        assert!(portrait > 1.0 && portrait < 2.0, "{portrait}");
        assert!(landscape > 2.0 * portrait);
        let v = strip_window(StripZoom::Volumes, NOW, NOW, true, &s);
        assert_eq!(v.span(), VOLUMES_SPAN_SECS);
        assert!((330.0 / v.span() * DEFAULT_VOLUME_SECS - 9.17).abs() < 0.01);
    }

    #[wasm_bindgen_test]
    fn only_day_asks_for_the_days_listing() {
        let w = MiniWindow {
            start: 0.0,
            end: DAY_SPAN_SECS,
        };
        assert_eq!(
            activity_window(StripZoom::Day, w, 330.0),
            Some((0.0, DAY_SPAN_SECS, 330.0))
        );
        assert_eq!(activity_window(StripZoom::Volumes, w, 330.0), None);
        assert_eq!(activity_window(StripZoom::Cuts, w, 330.0), None);
        assert_eq!(activity_window(StripZoom::Day, w, 0.0), None);
        // A phone's Day lane is wide enough to draw each volume, so the
        // day is listed for its volume sizes.
        assert!(crate::core::storm_activity::needs_scans(
            330.0,
            DAY_SPAN_SECS
        ));
    }

    // ── snapping ──────────────────────────────────────────────────────

    fn cell(start: f64, end: f64, state: FrameCellState) -> FrameCell {
        FrameCell {
            start_secs: start,
            end_secs: end,
            elevation_number: 1,
            elevation_angle: 0.5,
            state,
            chunks: None,
            is_active: false,
            is_prev_active: false,
        }
    }

    fn container(start: f64, end: f64, cells: Vec<FrameCell>) -> ScanContainer {
        ScanContainer {
            start_secs: start,
            end_secs: end,
            key_secs: start,
            vcp: 212,
            cells,
            sweep_spans: vec![(start, start + 20.0)],
            is_available: false,
            is_live: false,
            is_projected: false,
        }
    }

    /// Two volumes with two cuts each, and the next volume's ghost.
    fn strip() -> Vec<ScanContainer> {
        vec![
            container(
                0.0,
                300.0,
                vec![
                    cell(10.0, 40.0, FrameCellState::Cached),
                    cell(150.0, 180.0, FrameCellState::Cached),
                ],
            ),
            container(
                300.0,
                600.0,
                vec![
                    cell(310.0, 340.0, FrameCellState::Cached),
                    cell(450.0, 480.0, FrameCellState::Projected),
                ],
            ),
            ScanContainer {
                is_projected: true,
                ..container(
                    600.0,
                    900.0,
                    vec![cell(610.0, 640.0, FrameCellState::Projected)],
                )
            },
        ]
    }

    #[wasm_bindgen_test]
    fn units_skip_ghosts_and_land_on_whole_frames() {
        let u = StripUnits::from_containers(&strip());
        assert_eq!(
            u.cuts.iter().map(|c| c.land).collect::<Vec<_>>(),
            vec![40.0, 180.0, 340.0]
        );
        assert_eq!(
            u.volumes
                .iter()
                .map(|v| (v.start, v.land))
                .collect::<Vec<_>>(),
            vec![(0.0, 40.0), (300.0, 340.0)]
        );
    }

    #[wasm_bindgen_test]
    fn cuts_snap_to_the_cut_under_the_finger_or_the_nearest() {
        let u = StripUnits::from_containers(&strip());
        assert_eq!(snap_scrub(StripZoom::Cuts, 20.0, &u, 10_000.0), 40.0);
        assert_eq!(snap_scrub(StripZoom::Cuts, 170.0, &u, 10_000.0), 180.0);
        // In the gap between cuts: the nearer one.
        assert_eq!(snap_scrub(StripZoom::Cuts, 60.0, &u, 10_000.0), 40.0);
        assert_eq!(snap_scrub(StripZoom::Cuts, 140.0, &u, 10_000.0), 180.0);
        // Over the ghost: the last real cut.
        assert_eq!(snap_scrub(StripZoom::Cuts, 620.0, &u, 10_000.0), 340.0);
    }

    #[wasm_bindgen_test]
    fn volumes_and_day_snap_to_the_volume_under_the_finger() {
        let u = StripUnits::from_containers(&strip());
        for stop in [StripZoom::Volumes, StripZoom::Day] {
            assert_eq!(snap_scrub(stop, 170.0, &u, 10_000.0), 40.0);
            assert_eq!(snap_scrub(stop, 299.0, &u, 10_000.0), 40.0);
            assert_eq!(snap_scrub(stop, 301.0, &u, 10_000.0), 340.0);
            assert_eq!(snap_scrub(stop, 800.0, &u, 10_000.0), 340.0);
        }
    }

    #[wasm_bindgen_test]
    fn a_snap_never_lands_past_now_and_an_empty_strip_does_not_snap() {
        let u = StripUnits::from_containers(&strip());
        assert_eq!(snap_scrub(StripZoom::Cuts, 330.0, &u, 335.0), 335.0);
        let empty = StripUnits::default();
        assert_eq!(
            snap_scrub(StripZoom::Day, 1_234.5, &empty, 10_000.0),
            1_234.5
        );
    }

    #[wasm_bindgen_test]
    fn a_volume_without_known_cuts_lands_on_its_start() {
        let u = StripUnits::from_containers(&[ScanContainer {
            is_available: true,
            ..container(0.0, 300.0, Vec::new())
        }]);
        assert_eq!(snap_scrub(StripZoom::Volumes, 200.0, &u, 10_000.0), 0.0);
    }

    #[wasm_bindgen_test]
    fn day_also_snaps_to_listed_volumes_the_strip_has_no_container_for() {
        let listed = [
            ScanActivity {
                start: 10.0,
                end: 300.0,
                bytes: 1,
                level: 0.1,
            },
            ScanActivity {
                start: 5_000.0,
                end: 5_300.0,
                bytes: 1,
                level: 0.1,
            },
        ];
        let u = StripUnits::from_containers(&strip()).with_listed(&listed);
        assert_eq!(u.volumes.len(), 3, "the covered one is not added twice");
        assert_eq!(snap_scrub(StripZoom::Day, 5_100.0, &u, 10_000.0), 5_000.0);
        assert_eq!(snap_scrub(StripZoom::Day, 100.0, &u, 10_000.0), 40.0);
    }

    // ── pinch ─────────────────────────────────────────────────────────

    fn pinch(zoom_delta: f32, on_strip: bool) -> Option<PinchInput> {
        Some(PinchInput {
            zoom_delta,
            began_on_strip: on_strip,
        })
    }

    /// Run a pinch of per-frame deltas, then release; the settled stop.
    fn run_pinch(stop: StripZoom, deltas: &[f32], on_strip: bool) -> Option<StripZoom> {
        let mut t = None;
        for d in deltas {
            let (next, out) = PinchTrack::step(t, pinch(*d, on_strip), stop);
            assert_eq!(out.settle, None, "never settles mid-pinch");
            assert_eq!(out.claimed, on_strip);
            t = next;
        }
        let (t, out) = PinchTrack::step(t, None, stop);
        assert!(t.is_none());
        out.settle
    }

    #[wasm_bindgen_test]
    fn a_pinch_moves_one_stop_by_its_factor_at_release() {
        // Apart past 1.25: one stop in.
        assert_eq!(
            run_pinch(StripZoom::Day, &[1.1, 1.1, 1.1], true),
            Some(StripZoom::Volumes)
        );
        // Together past 0.8: one stop out, never two.
        assert_eq!(
            run_pinch(StripZoom::Cuts, &[0.7, 0.7, 0.7], true),
            Some(StripZoom::Volumes)
        );
        // In between: stays.
        assert_eq!(run_pinch(StripZoom::Volumes, &[1.1, 1.1], true), None);
        assert_eq!(run_pinch(StripZoom::Volumes, &[0.9], true), None);
        // Out, then back in: only where it ends counts.
        assert_eq!(run_pinch(StripZoom::Volumes, &[1.5, 0.6], true), None);
    }

    #[wasm_bindgen_test]
    fn the_thresholds_are_exclusive() {
        assert_eq!(pinch_stop(StripZoom::Day, PINCH_IN_FACTOR), StripZoom::Day);
        assert_eq!(pinch_stop(StripZoom::Day, 1.26), StripZoom::Volumes);
        assert_eq!(
            pinch_stop(StripZoom::Cuts, PINCH_OUT_FACTOR),
            StripZoom::Cuts
        );
        assert_eq!(pinch_stop(StripZoom::Cuts, 0.79), StripZoom::Volumes);
        // Past the ends, nothing to settle on.
        assert_eq!(run_pinch(StripZoom::Cuts, &[2.0], true), None);
        assert_eq!(run_pinch(StripZoom::Day, &[0.3], true), None);
    }

    #[wasm_bindgen_test]
    fn a_pinch_that_began_on_the_map_is_the_maps() {
        assert_eq!(run_pinch(StripZoom::Volumes, &[2.0, 2.0], false), None);
        // Ownership is decided at the start, not by where it drifts.
        let (t, _) = PinchTrack::step(None, pinch(1.0, false), StripZoom::Cuts);
        let (t, out) = PinchTrack::step(t, pinch(1.5, true), StripZoom::Cuts);
        assert!(!out.claimed);
        assert_eq!(t.unwrap().factor(), 1.5);
    }

    fn touch(down: bool, x: f32, now: f64, pinch: Option<PinchInput>) -> StripTouchInput {
        StripTouchInput {
            down,
            pos: down.then_some((x, 10.0)),
            now,
            pinch,
            stop: StripZoom::Volumes,
        }
    }

    #[wasm_bindgen_test]
    fn a_pinch_cancels_a_scrub_and_never_seeks() {
        let mut t = StripTouch::default();
        assert!(t.step(touch(true, 100.0, 0.0, None)).may_seek);
        // The finger scrubs...
        assert!(t.step(touch(true, 130.0, 0.1, None)).may_seek);
        // ...then a second finger lands: the strip's pinch.
        let out = t.step(touch(true, 140.0, 0.2, pinch(1.2, true)));
        assert!(out.claimed && !out.may_seek);
        assert!(t.press.unwrap().cancelled());
        let out = t.step(touch(true, 160.0, 0.3, pinch(1.2, true)));
        assert!(!out.may_seek);
        // The second finger lifts first: the pinch settles, the press stays
        // cancelled while its finger is down...
        let out = t.step(touch(true, 170.0, 0.4, None));
        assert_eq!(out.settle, Some(StripZoom::Cuts));
        assert!(!out.claimed && !out.may_seek);
        // ...and its release is not a tap.
        let out = t.step(touch(false, 0.0, 0.5, None));
        assert!(out.press.swallow_release && !out.may_seek);
        // The next press is an ordinary one.
        assert!(t.step(touch(true, 50.0, 1.0, None)).may_seek);
    }

    #[wasm_bindgen_test]
    fn a_pinch_cancels_a_pending_long_press() {
        let mut t = StripTouch::default();
        let out = t.step(touch(true, 100.0, 0.0, None));
        assert!(out.press.waiting);
        // Two fingers, held still past the long-press time.
        let out = t.step(touch(true, 100.0, 0.3, pinch(1.0, true)));
        assert!(!out.press.waiting);
        let out = t.step(touch(true, 100.0, 0.9, pinch(1.0, true)));
        assert_eq!(out.press.long_press, None);
        let out = t.step(touch(true, 100.0, 1.2, None));
        assert_eq!(out.press.long_press, None, "not after the pinch either");
        assert_eq!(out.settle, None, "a still pinch changes nothing");
    }

    #[wasm_bindgen_test]
    fn a_press_that_starts_during_a_pinch_is_cancelled_at_once() {
        let mut t = StripTouch::default();
        t.step(touch(false, 0.0, 0.0, pinch(1.0, true)));
        let out = t.step(touch(true, 100.0, 0.1, pinch(0.5, true)));
        assert!(!out.may_seek);
        assert!(t.press.unwrap().cancelled());
    }

    #[wasm_bindgen_test]
    fn the_maps_pinch_leaves_the_strip_alone() {
        let mut t = StripTouch::default();
        t.step(touch(true, 100.0, 0.0, None));
        let out = t.step(touch(true, 130.0, 0.1, pinch(1.5, false)));
        assert!(!out.claimed);
        assert!(out.may_seek);
    }

    // ── what each stop draws ──────────────────────────────────────────

    #[wasm_bindgen_test]
    fn a_volume_draws_as_one_cell_with_its_most_pressing_state() {
        let mut c = container(
            0.0,
            300.0,
            vec![
                cell(10.0, 40.0, FrameCellState::Cached),
                cell(150.0, 180.0, FrameCellState::InFlight),
            ],
        );
        c.cells[0].is_active = true;
        let v = volume_cell(&c, 10.0);
        assert_eq!(v.cells.len(), 1);
        let only = &v.cells[0];
        assert_eq!((only.start_secs, only.end_secs), (0.0, 290.0));
        assert_eq!(only.state, FrameCellState::InFlight);
        assert!(only.is_active, "the ring rides the volume");
        assert!(v.sweep_spans.is_empty());
        assert_eq!((v.start_secs, v.end_secs, v.key_secs), (0.0, 300.0, 0.0));

        let failed = container(
            0.0,
            300.0,
            vec![
                cell(10.0, 40.0, FrameCellState::Failed),
                cell(150.0, 180.0, FrameCellState::InFlight),
            ],
        );
        assert_eq!(
            volume_cell(&failed, 0.0).cells[0].state,
            FrameCellState::Failed
        );
        let held = container(
            0.0,
            300.0,
            vec![
                cell(10.0, 40.0, FrameCellState::Available),
                cell(150.0, 180.0, FrameCellState::Cached),
            ],
        );
        assert_eq!(
            volume_cell(&held, 0.0).cells[0].state,
            FrameCellState::Cached
        );
        let ghost = ScanContainer {
            is_projected: true,
            ..container(600.0, 900.0, Vec::new())
        };
        assert_eq!(
            volume_cell(&ghost, 0.0).cells[0].state,
            FrameCellState::Projected
        );
        // A gap wider than the volume never inverts the cell.
        assert_eq!(volume_cell(&held, 1_000.0).cells[0].end_secs, 0.0);
    }

    #[wasm_bindgen_test]
    fn the_ruler_marks_volumes_hours_and_quarter_days_in_the_zone() {
        // 2024-10-09 18:41:00 UTC.
        let t0 = 1_728_499_260.0;
        let cuts = ruler_ticks(
            StripZoom::Cuts,
            MiniWindow {
                start: t0 - 10.0,
                end: t0 + 900.0,
            },
            &[t0 - 300.0, t0, t0 + 300.0, t0 + 600.0],
            0,
        );
        assert_eq!(
            cuts.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            vec!["18:41", "18:46", "18:51"]
        );

        let vols = ruler_ticks(
            StripZoom::Volumes,
            MiniWindow {
                start: t0 - VOLUMES_SPAN_SECS,
                end: t0,
            },
            &[],
            0,
        );
        assert_eq!(
            vols.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            vec!["16:00", "17:00", "18:00"]
        );
        assert!(vols.iter().all(|t| t.ts.rem_euclid(3600.0) == 0.0));

        let day = ruler_ticks(
            StripZoom::Day,
            MiniWindow {
                start: t0 - DAY_SPAN_SECS,
                end: t0,
            },
            &[],
            0,
        );
        assert_eq!(
            day.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            vec!["00", "06", "12", "18"]
        );

        // In US Central daylight time (UTC−5) the same day reads in its
        // own hours, on its own midnight.
        let cdt = ruler_ticks(
            StripZoom::Day,
            MiniWindow {
                start: t0 - DAY_SPAN_SECS,
                end: t0,
            },
            &[],
            -5 * 3600,
        );
        assert_eq!(
            cdt.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            vec!["18", "00", "06", "12"]
        );
        assert_eq!((cdt[1].ts + (-5.0 * 3600.0)).rem_euclid(86_400.0), 0.0);
    }
}
