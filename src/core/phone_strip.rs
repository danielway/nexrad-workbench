//! The phone strip's zoom stops (phone timeline §1): NOW, 3 H and 24 H.
//!
//! Continuous zoom suits a wheel, not a thumb, so the phone's row has three
//! stops, each with its own window rule and scrub unit. What each stop
//! draws is [`crate::core::phone_row`]; this module owns the windows, the
//! scrub and tap landings, and the touch.
//!
//! - **Cuts** ("NOW") — the last three volumes
//!   ([`crate::core::phone::mini_window`]), each a block. A scrub snaps per
//!   cut where cuts are known and per volume elsewhere
//!   ([`StripUnits::landing_units`]). A selected range always shows whole:
//!   the window widens past three volumes when the range needs it
//!   ([`strip_window`]'s `range`).
//! - **Volumes** ("3 H") — three hours, one column per volume, as tall as
//!   the weather the volume held. A scrub snaps to volumes.
//! - **Day** ("24 H") — 24 hours of storm hill, one step per half-hour
//!   bucket ([`activity_bars`]). A scrub snaps to volumes.
//!
//! A tap goes to the volume under the finger, its first cut, at every stop
//! ([`tap_landing`]).
//!
//! Pinned, the window ends just past now and slides with it. Detached, it
//! centres on the playhead when the playhead arrives from off the window,
//! holds still while the finger works inside it, and slides once the
//! playhead nears an edge, or with now when it showed now and the playhead
//! keeps its margin ([`strip_window`]). Landscape keeps the spans and
//! gets more pixels per unit.
//!
//! A pinch moves one stop and settles on release ([`PinchTrack`]); the
//! transport row's NOW · 3 H · 24 H control names the stop and sets it
//! ([`zoom_pill_label`], [`StripZoom::ALL`]). Zoom and selection are
//! independent: zooming never touches the playhead or the pin, and going
//! live keeps the zoom ([`strip_zoom_after`]).
//!
//! All pure; `ui::timeline::mini` draws what these return.

use crate::core::phone::{
    mini_lead_secs, mini_near_now, mini_window, typical_volume_secs, MiniWindow, PressOut,
    PressTrack, MINI_VOLUMES,
};
use crate::core::storm_activity::{ScanActivity, ScanSizes, ViewScale};
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
    /// The last three volumes, every cut ("NOW").
    #[default]
    Cuts,
    /// Three hours of storm bars, one per volume ("3 H").
    Volumes,
    /// 24 hours of storm hill, one step per half-hour ("24 H").
    Day,
}

impl StripZoom {
    /// The stops in the segmented control's order: NOW · 3 H · 24 H.
    pub(crate) const ALL: [StripZoom; 3] = [StripZoom::Cuts, StripZoom::Volumes, StripZoom::Day];

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

    /// Whether the strip draws storm bars rather than cuts, and so snaps a
    /// scrub to volumes.
    pub(crate) fn per_volume(self) -> bool {
        self != StripZoom::Cuts
    }
}

/// The stop control's words for a stop.
pub(crate) fn zoom_pill_label(stop: StripZoom) -> &'static str {
    match stop {
        StripZoom::Cuts => "NOW",
        StripZoom::Volumes => "3 H",
        StripZoom::Day => "24 H",
    }
}

/// The stop after an intent. [`Intent::SetPhoneStripZoom`] (the pill, a
/// pinch) sets it, and a day picked in the calendar sheet
/// ([`Intent::GoToDay`]) opens at Volumes ("3 H"), centred on the landing
/// volume with its storm bars around it. Nothing else moves it: zoom never
/// changes the selection, and the selection never changes the zoom, so going
/// live or rejoining from a zoomed-out stop stays zoomed out.
pub(crate) fn strip_zoom_after(stop: StripZoom, intent: &Intent) -> StripZoom {
    match intent {
        Intent::SetPhoneStripZoom(next) => *next,
        Intent::GoToDay(_) => StripZoom::Volumes,
        _ => stop,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Windows
// ───────────────────────────────────────────────────────────────────────────

/// How close to either end of a detached window (as a share of its span)
/// the playhead may come before the window slides to keep it in.
pub(crate) const EDGE_SHARE: f64 = 0.1;

/// How close (as a share of the span) the last window's span must be to
/// this stop's for the window to be held rather than re-centred.
const SAME_STOP_SHARE: f64 = 0.3;

/// The strip's window at a stop, in Unix seconds. `volume_starts` are the
/// containers' starts, oldest first; `prev` is the window the strip drew
/// last frame.
///
/// Cuts near now is [`mini_window`] (anchored to now). Further back it
/// follows the same rule as the other stops over three volumes, so a scrub
/// moves the playhead under a still strip instead of re-centring the strip
/// on every hop. At Volumes and Day, pinned, the window ends at
/// now plus the ghost room Cuts leaves ([`mini_lead_secs`]), so now sits
/// near the right edge with the ring on it and the window slides with now.
/// Detached, it centres on the playhead when the playhead lands off the
/// last window (a typed time, a day from the calendar, a zoom change); while
/// the playhead stays inside it the window holds still, so a scrub moves
/// the ring under the finger rather than the strip, and once the playhead
/// comes within [`EDGE_SHARE`] of an end the window slides just enough to
/// keep it there. It never reaches past now plus the ghost room. The span
/// is the stop's in either orientation; a wider strip only gets more pixels
/// per unit.
///
/// `range` is the selected range as the strip draws it (its end at now
/// when pinned), if any. At Cuts the whole range shows: see
/// [`cuts_range_window`].
pub(crate) fn strip_window(
    stop: StripZoom,
    now: f64,
    playhead: f64,
    pinned: bool,
    volume_starts: &[f64],
    prev: Option<MiniWindow>,
    range: Option<(f64, f64)>,
) -> MiniWindow {
    if let (StripZoom::Cuts, Some(range)) = (stop, range) {
        return cuts_range_window(now, range, volume_starts);
    }
    let span = match stop {
        StripZoom::Cuts if mini_near_now(now, playhead, pinned, volume_starts) => {
            return mini_window(now, playhead, pinned, volume_starts)
        }
        StripZoom::Cuts => mini_window(now, playhead, pinned, volume_starts).span(),
        StripZoom::Volumes => VOLUMES_SPAN_SECS,
        StripZoom::Day => DAY_SPAN_SECS,
    };
    let limit = now + mini_lead_secs(volume_starts);
    let end = if pinned {
        limit
    } else {
        // The last window is kept when it is this stop's (a 15 MIN window
        // near now spans a little more or less than three volumes) and the
        // playhead is still inside it.
        match prev.filter(|p| {
            (p.span() - span).abs() < SAME_STOP_SHARE * span
                && playhead >= p.start
                && playhead <= p.end
        }) {
            Some(p) => {
                let margin = span * EDGE_SHARE;
                let start = if playhead < p.start + margin {
                    playhead - margin
                } else if playhead > p.end - margin {
                    playhead + margin - span
                } else {
                    p.start
                };
                // A window that showed now keeps showing it while the
                // playhead keeps its margin: a tap back from live leaves
                // the now line in view as now moves on.
                let start = if p.end >= now - margin {
                    start.max((limit - span).min(playhead - margin))
                } else {
                    start
                };
                (start + span).min(limit)
            }
            None => (playhead + span / 2.0).min(limit),
        }
    };
    MiniWindow {
        start: end - span,
        end,
    }
}

/// The room kept before a range's start at Cuts, as a share of the span,
/// when the range is what sets the window's left edge.
pub(crate) const RANGE_EDGE_SHARE: f64 = 0.05;

/// The Cuts window while a range is selected: never narrower than the
/// stop's three volumes, and as wide as it takes to show the whole range.
///
/// Near now (the range ends within three volumes of now, as a range pinned
/// to now always does) it is [`mini_window`] anchored to now, ending at now
/// plus the ghost room; the left edge moves out as far as it takes to keep
/// [`RANGE_EDGE_SHARE`] of the span before the block the range starts in
/// (four frames at a tilt are four volumes), so that block shows whole.
/// Further back it centres on the range, spanning the three volumes or the
/// range plus that room either side, whichever is wider, and never reaches
/// past now plus the ghost room. Either way the window follows only the
/// range, now and the volumes, never the playhead looping inside the range,
/// so it holds still while the loop plays and slides only with now.
fn cuts_range_window(now: f64, (start, end): (f64, f64), volume_starts: &[f64]) -> MiniWindow {
    let m = RANGE_EDGE_SHARE;
    // The room is kept before the block the range starts in, so that block
    // shows whole: the volume holding the range's first frame.
    let start = volume_starts
        .iter()
        .copied()
        .filter(|&s| s <= start + 1.0 && start - s < MAX_FIRST_FRAME_LAG_SECS)
        .max_by(f64::total_cmp)
        .map_or(start, |s| s.min(start));
    if mini_near_now(now, end, false, volume_starts) {
        let base = mini_window(now, end, true, volume_starts);
        // start = first block − m · (end − start), solved for start.
        let room = (start - m * base.end) / (1.0 - m);
        return MiniWindow {
            start: base.start.min(room),
            end: base.end,
        };
    }
    let nominal = MINI_VOLUMES * typical_volume_secs(volume_starts);
    let span = nominal.max((end - start) / (1.0 - 2.0 * m));
    let limit = now + mini_lead_secs(volume_starts);
    let w_end = ((start + end) / 2.0 + span / 2.0).min(limit);
    MiniWindow {
        start: w_end - span,
        end: w_end,
    }
}

/// How far after its volume's start a range's first frame may fall and
/// still be that volume's.
const MAX_FIRST_FRAME_LAG_SECS: f64 = 900.0;

/// The span whose volume sizes a stop reads: the window at Volumes and
/// Day; at Cuts the three hours around the window's middle, so a volume's
/// bar is as tall at 15 MIN as at 3 H (bars are scaled to the volumes in
/// view, and three volumes alone would exaggerate any difference).
pub(crate) fn sizes_span(stop: StripZoom, window: MiniWindow) -> (f64, f64) {
    match stop {
        StripZoom::Cuts => {
            // A range wider than three hours widens the window past them.
            let half = VOLUMES_SPAN_SECS.max(window.span()) / 2.0;
            let mid = (window.start + window.end) / 2.0;
            (mid - half, mid + half)
        }
        StripZoom::Volumes | StripZoom::Day => (window.start, window.end),
    }
}

/// The storm-activity listing the phone strip asks for: the span it reads
/// sizes over ([`sizes_span`]) and the lane's width, which decide (through
/// [`crate::core::storm_activity::needs_scans`]) that its days are listed
/// for their volumes' sizes, as the desktop overview does zoomed in (days
/// the nightly index covers have no per-volume sizes). Cuts asks as 3 H
/// does, for the volumes it draws as bars.
pub(crate) fn activity_window(
    stop: StripZoom,
    window: MiniWindow,
    lane_px: f32,
) -> Option<(f64, f64, f64)> {
    let (start, end) = sizes_span(stop, window);
    (lane_px > 0.0).then_some((start, end, lane_px as f64))
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

/// How far into a volume whose cuts aren't known a scrub lands: past the
/// gap between the volume's listed time and its first sweep, and inside its
/// first cut, so once it arrives the playhead is on that cut (a playhead on
/// the listed time itself resolves to the volume before).
pub(crate) const VOLUME_LAND_SECS: f64 = 10.0;

/// Where a scrub lands in a volume with no known cuts.
pub(crate) fn volume_landing(start: f64, end: f64) -> f64 {
    (start + VOLUME_LAND_SECS)
        .min((start + end) / 2.0)
        .max(start)
}

impl StripUnits {
    /// The units of the strip's containers. Projected cells and ghost
    /// volumes are not landing places: nothing is there yet. A volume not on
    /// the device has no cuts here (its one cell is a placeholder for the
    /// tilt, not a frame) and lands just past its start, which is what fetching it
    /// asks for; its cuts join once it arrives.
    /// ([`VOLUME_LAND_SECS`] in, so the first cut is what shows.)
    pub(crate) fn from_containers(containers: &[ScanContainer]) -> Self {
        let mut out = StripUnits::default();
        for c in containers.iter().filter(|c| !c.is_projected) {
            let cells: Vec<&FrameCell> = if c.is_available {
                Vec::new()
            } else {
                c.cells
                    .iter()
                    .filter(|cell| cell.state != FrameCellState::Projected)
                    .collect()
            };
            out.cuts.extend(cells.iter().map(|cell| SnapUnit {
                start: cell.start_secs,
                end: cell.end_secs.max(cell.start_secs),
                land: cell.end_secs.max(cell.start_secs),
            }));
            out.volumes.push(SnapUnit {
                start: c.start_secs,
                end: c.end_secs.max(c.start_secs),
                land: cells.first().map_or_else(
                    || volume_landing(c.start_secs, c.end_secs),
                    |cell| cell.end_secs.max(cell.start_secs),
                ),
            });
        }
        out.cuts.sort_by(|a, b| a.start.total_cmp(&b.start));
        out.volumes.sort_by(|a, b| a.start.total_cmp(&b.start));
        out
    }

    /// Add the volumes a day's listing knows of that no container covers
    /// (bars reach volumes the strip has no container for).
    pub(crate) fn with_listed(mut self, listed: &[ScanActivity]) -> Self {
        let known = self.volumes.clone();
        self.volumes.extend(
            listed
                .iter()
                .filter(|s| !known.iter().any(|v| s.start >= v.start && s.start < v.end))
                .map(|s| SnapUnit {
                    start: s.start,
                    end: s.end.max(s.start),
                    land: volume_landing(s.start, s.end),
                }),
        );
        self.volumes.sort_by(|a, b| a.start.total_cmp(&b.start));
        self
    }

    /// What a scrub can land on at `stop`: volumes at Volumes and Day; at
    /// Cuts every cut the strip knows, and each volume with no known cut as
    /// one unit. Oldest first.
    pub(crate) fn landing_units(&self, stop: StripZoom) -> Vec<SnapUnit> {
        if stop.per_volume() {
            return self.volumes.clone();
        }
        let mut out = self.cuts.clone();
        out.extend(self.volumes.iter().copied().filter(|v| {
            !self
                .cuts
                .iter()
                .any(|c| c.start >= v.start - 1.0 && c.start < v.end)
        }));
        out.sort_by(|a, b| a.start.total_cmp(&b.start));
        out
    }
}

/// Where a scrub at `ts` puts the playhead at `stop`: the landing time of
/// the unit under the finger ([`StripUnits::landing_units`]), or of the
/// nearest unit when the finger is in a gap, never past `now`. With no
/// units on the strip the scrub goes where the finger is.
pub(crate) fn snap_scrub(stop: StripZoom, ts: f64, units: &StripUnits, now: f64) -> f64 {
    let distance = |u: &SnapUnit| {
        if ts < u.start {
            u.start - ts
        } else if ts >= u.end {
            ts - u.end
        } else {
            0.0
        }
    };
    units
        .landing_units(stop)
        .iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .map_or(ts, |u| u.land.min(now))
}

/// Where a tap at `ts` puts the playhead, at every stop: the first cut of
/// the volume under the finger (or of the nearest one), never past `now`.
/// A drag goes cut by cut ([`snap_scrub`]); a tap goes to a volume.
pub(crate) fn tap_landing(ts: f64, units: &StripUnits, now: f64) -> f64 {
    snap_scrub(StripZoom::Volumes, ts, units, now)
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
    /// Where the finger was the last time the press scrubbed (points).
    scrub_x: Option<f32>,
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
    /// Whether a drag frame with the finger at `x` scrubs: only when the
    /// finger has moved since the last scrub (or this is the press's first).
    /// A still finger never seeks again, so the playhead holds while the
    /// strip changes under it: a volume's cuts arriving, the window sliding
    /// near an edge.
    pub(crate) fn scrub_moved(&mut self, x: f32) -> bool {
        let moved = self.scrub_x.is_none_or(|last| (x - last).abs() >= 0.5);
        if moved {
            self.scrub_x = Some(x);
        }
        moved
    }

    /// Advance the press and the pinch together. A pinch the strip owns
    /// cancels the press under it (and any press that starts during it), so
    /// a pinch never seeks, scrubs or opens the sheet; the press stays
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
        if press.is_none() {
            self.scrub_x = None;
        }
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

/// The Day stop's bucket: one bar per half hour.
pub(crate) const DAY_BUCKET_SECS: f64 = 1800.0;

/// One storm-activity bar on a zoomed-out stop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StripBar {
    pub start: f64,
    pub end: f64,
    /// Height as a fraction of the bar lane, on the scale of the bars in
    /// view ([`ViewScale`]).
    pub frac: f32,
}

/// The storm-activity bars a stop draws over `window`, from the volumes'
/// sizes in `sizes` (the same data as the desktop overview's): at Volumes
/// one bar per volume ([`ScanSizes::bars`]); at Day one per
/// [`DAY_BUCKET_SECS`] bucket, as tall as its stormiest volume, ending at
/// its last volume's end. Nothing at Cuts.
pub(crate) fn activity_bars(
    stop: StripZoom,
    window: MiniWindow,
    sizes: &ScanSizes,
) -> Vec<StripBar> {
    match stop {
        StripZoom::Cuts => Vec::new(),
        StripZoom::Volumes => sizes
            .bars(window.start, window.end)
            .into_iter()
            .map(|(s, frac)| StripBar {
                start: s.start,
                end: s.end,
                frac,
            })
            .collect(),
        StripZoom::Day => {
            // (bucket start, stormiest level, last volume's end)
            let mut buckets: Vec<(f64, f32, f64)> = Vec::new();
            for s in sizes
                .scans
                .iter()
                .filter(|s| s.end >= window.start && s.start <= window.end)
            {
                let b = (s.start / DAY_BUCKET_SECS).floor() * DAY_BUCKET_SECS;
                match buckets.last_mut() {
                    Some(last) if last.0 == b => {
                        last.1 = last.1.max(s.level);
                        last.2 = last.2.max(s.end);
                    }
                    _ => buckets.push((b, s.level, s.end)),
                }
            }
            let scale = ViewScale::from_levels(buckets.iter().map(|b| b.1));
            buckets
                .into_iter()
                .map(|(b, level, last_end)| StripBar {
                    start: b,
                    end: last_end.min(b + DAY_BUCKET_SECS),
                    frac: scale.height_frac(level),
                })
                .collect()
        }
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
    fn the_control_names_the_three_stops_in_order() {
        assert_eq!(zoom_pill_label(StripZoom::Cuts), "NOW");
        assert_eq!(zoom_pill_label(StripZoom::Volumes), "3 H");
        assert_eq!(zoom_pill_label(StripZoom::Day), "24 H");
        assert_eq!(
            StripZoom::ALL,
            [StripZoom::Cuts, StripZoom::Volumes, StripZoom::Day]
        );
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
    fn going_live_or_rejoining_keeps_the_zoom() {
        for stop in [StripZoom::Cuts, StripZoom::Volumes, StripZoom::Day] {
            assert_eq!(strip_zoom_after(stop, &Intent::GoLive), stop);
            assert_eq!(strip_zoom_after(stop, &Intent::ReturnToLive), stop);
        }
    }

    #[wasm_bindgen_test]
    fn a_day_from_the_calendar_lands_at_three_hours_from_any_stop() {
        let day = crate::core::calendar::CalendarDay::of(
            chrono::NaiveDate::from_ymd_opt(2025, 9, 22).unwrap(),
            &crate::core::calendar::FixedZone(0),
        );
        for stop in [StripZoom::Cuts, StripZoom::Volumes, StripZoom::Day] {
            assert_eq!(
                strip_zoom_after(stop, &Intent::GoToDay(day)),
                StripZoom::Volumes
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
                strip_window(StripZoom::Cuts, NOW, playhead, pinned, &s, None, None),
                mini_window(NOW, playhead, pinned, &s)
            );
        }
    }

    /// The last volume starts 300 s before now; frames at a tilt start
    /// 20 s into their volumes.
    fn live_volumes() -> Vec<f64> {
        starts(NOW - 6_000.0, NOW - 300.0)
    }

    fn last_frames(n: f64) -> (f64, f64) {
        (NOW - 300.0 * n + 20.0, NOW)
    }

    fn assert_range_inside(w: MiniWindow, (start, end): (f64, f64)) {
        assert!(
            w.start < start && end <= w.end,
            "{w:?} misses {start}..{end}"
        );
    }

    #[wasm_bindgen_test]
    fn cuts_widens_so_four_frames_pinned_to_now_all_show() {
        let s = live_volumes();
        let base = mini_window(NOW, NOW, true, &s);
        let range = last_frames(4.0);
        assert!(range.0 < base.start, "the nominal window cuts the range");
        let w = strip_window(
            StripZoom::Cuts,
            NOW,
            range.0 + 10.0,
            true,
            &s,
            None,
            Some(range),
        );
        assert_range_inside(w, range);
        // About 5 % of the span before the block it starts in, and still
        // ending at now plus the ghost room. The margin is kept before the block the range starts in (its
        // first frame is 20 s into it).
        assert!(((range.0 - 20.0) - w.start - RANGE_EDGE_SHARE * w.span()).abs() < 1e-6);
        assert_eq!(w.end, NOW + lead());
        assert!(w.span() > base.span());
    }

    #[wasm_bindgen_test]
    fn cuts_keeps_its_window_for_a_range_that_already_fits() {
        let s = live_volumes();
        let base = mini_window(NOW, NOW, true, &s);
        let range = last_frames(2.0);
        let w = strip_window(StripZoom::Cuts, NOW, range.0, true, &s, None, Some(range));
        assert_eq!(w, base);
        assert_range_inside(w, range);
        // Three frames start in the window's first block: it moves out just
        // enough to keep the margin before that block.
        let range = last_frames(3.0);
        let w = strip_window(StripZoom::Cuts, NOW, range.0, true, &s, None, Some(range));
        assert_eq!(w.end, base.end);
        assert!(((range.0 - 20.0) - w.start - RANGE_EDGE_SHARE * w.span()).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn cuts_holds_still_while_the_loop_plays_and_ignores_the_last_window() {
        let s = live_volumes();
        let range = last_frames(4.0);
        let first = strip_window(StripZoom::Cuts, NOW, range.0, true, &s, None, Some(range));
        let mut ph = range.0;
        while ph <= range.1 {
            for prev in [None, Some(first)] {
                assert_eq!(
                    strip_window(StripZoom::Cuts, NOW, ph, true, &s, prev, Some(range)),
                    first
                );
            }
            ph += 37.0;
        }
        // A detached fixed range near now is shown the same way: the
        // playhead looping back more than two volumes never flips it.
        for ph in [range.0, NOW - 400.0] {
            assert_eq!(
                strip_window(
                    StripZoom::Cuts,
                    NOW,
                    ph,
                    false,
                    &s,
                    Some(first),
                    Some(range)
                ),
                first
            );
        }
    }

    #[wasm_bindgen_test]
    fn cuts_fits_a_fixed_range_far_back_wider_than_three_volumes() {
        let s = starts(NOW - 60_000.0, NOW - 300.0);
        let range = (NOW - 30_000.0, NOW - 30_000.0 + 5.0 * 300.0);
        let w = strip_window(
            StripZoom::Cuts,
            NOW,
            range.0 + 100.0,
            false,
            &s,
            None,
            Some(range),
        );
        assert_range_inside(w, range);
        let room = RANGE_EDGE_SHARE * w.span();
        assert!((range.0 - w.start - room).abs() < 1e-6);
        assert!((w.end - range.1 - room).abs() < 1e-6);
        // The same window wherever the playhead loops in it.
        for ph in [range.0, range.1, (range.0 + range.1) / 2.0] {
            assert_eq!(
                strip_window(StripZoom::Cuts, NOW, ph, false, &s, Some(w), Some(range)),
                w
            );
        }
        // A short range far back keeps the stop's three volumes, centred on
        // it.
        let short = (NOW - 30_000.0, NOW - 29_700.0);
        let w = strip_window(StripZoom::Cuts, NOW, short.0, false, &s, None, Some(short));
        assert_eq!(w.span(), MINI_VOLUMES * DEFAULT_VOLUME_SECS);
        assert_eq!((w.start + w.end) / 2.0, (short.0 + short.1) / 2.0);
        // A wide fixed range that ends just behind now stops at the ghost
        // room.
        let recent = (NOW - 3_000.0, NOW - 700.0);
        let w = strip_window(
            StripZoom::Cuts,
            NOW,
            recent.0,
            false,
            &s,
            None,
            Some(recent),
        );
        assert_range_inside(w, recent);
        assert!(w.end <= NOW + lead() + 1e-9);
    }

    #[wasm_bindgen_test]
    fn a_point_or_another_stop_ignores_the_range() {
        let s = live_volumes();
        let range = last_frames(4.0);
        // A point: today's window.
        assert_eq!(
            strip_window(StripZoom::Cuts, NOW, NOW, true, &s, None, None),
            mini_window(NOW, NOW, true, &s)
        );
        for stop in [StripZoom::Volumes, StripZoom::Day] {
            assert_eq!(
                strip_window(stop, NOW, NOW, true, &s, None, Some(range)),
                strip_window(stop, NOW, NOW, true, &s, None, None)
            );
        }
    }

    #[wasm_bindgen_test]
    fn cuts_reads_sizes_over_at_least_its_window() {
        let narrow = MiniWindow {
            start: NOW - 900.0,
            end: NOW,
        };
        let (a, b) = sizes_span(StripZoom::Cuts, narrow);
        assert_eq!(b - a, VOLUMES_SPAN_SECS);
        let wide = MiniWindow {
            start: NOW - 5.0 * 3600.0,
            end: NOW,
        };
        assert_eq!(sizes_span(StripZoom::Cuts, wide), (wide.start, wide.end));
    }

    #[wasm_bindgen_test]
    fn far_back_volumes_and_day_center_on_the_playhead() {
        let s = starts(NOW - 200_000.0, NOW);
        let playhead = NOW - 150_000.0;
        let v = strip_window(StripZoom::Volumes, NOW, playhead, false, &s, None, None);
        assert_eq!(v.span(), VOLUMES_SPAN_SECS);
        assert_eq!((v.start + v.end) / 2.0, playhead);
        let d = strip_window(StripZoom::Day, NOW, playhead, false, &s, None, None);
        assert_eq!(d.span(), DAY_SPAN_SECS);
        assert_eq!((d.start + d.end) / 2.0, playhead);
    }

    #[wasm_bindgen_test]
    fn with_no_last_window_detached_centres_on_the_playhead() {
        let s = starts(NOW - 200_000.0, NOW);
        let a = strip_window(
            StripZoom::Volumes,
            NOW,
            NOW - 50_000.0,
            false,
            &s,
            None,
            None,
        );
        let b = strip_window(
            StripZoom::Volumes,
            NOW,
            NOW - 49_000.0,
            false,
            &s,
            None,
            None,
        );
        assert_eq!(b.start - a.start, 1_000.0);
        assert_eq!(b.end - a.end, 1_000.0);
    }

    #[wasm_bindgen_test]
    fn pinned_the_window_ends_at_cuts_ghost_room() {
        let s = starts(NOW - 100_000.0, NOW);
        let cuts = strip_window(StripZoom::Cuts, NOW, NOW, true, &s, None, None);
        for stop in [StripZoom::Volumes, StripZoom::Day] {
            let w = strip_window(stop, NOW, NOW, true, &s, None, None);
            assert_eq!(w.end, cuts.end, "{stop:?} ends where Cuts does");
            assert!((w.end - (NOW + lead())).abs() < 1e-9);
            assert!(w.start < NOW && NOW < w.end, "now sits inside");
        }
        // A playhead that lags now a little while pinned changes nothing.
        let w = strip_window(StripZoom::Day, NOW, NOW - 3.0, true, &s, None, None);
        assert_eq!(w.end, NOW + lead());
    }

    #[wasm_bindgen_test]
    fn detached_near_now_clamps_so_nothing_past_the_ghost_room_shows() {
        let s = starts(NOW - 100_000.0, NOW);
        // Half an hour back: centered would reach an hour past now.
        let w = strip_window(
            StripZoom::Volumes,
            NOW,
            NOW - 1_800.0,
            false,
            &s,
            None,
            None,
        );
        assert_eq!(w.end, NOW + lead());
        assert_eq!(w.span(), VOLUMES_SPAN_SECS);
        assert!(w.start < NOW - 1_800.0, "the playhead stays in view");
        // Two hours back is far enough to center.
        let w = strip_window(
            StripZoom::Volumes,
            NOW,
            NOW - 7_200.0,
            false,
            &s,
            None,
            None,
        );
        assert_eq!((w.start + w.end) / 2.0, NOW - 7_200.0);
        assert!(w.end < NOW);
        // At Day, six hours back still clamps.
        let w = strip_window(
            StripZoom::Day,
            NOW,
            NOW - 6.0 * 3600.0,
            false,
            &s,
            None,
            None,
        );
        assert_eq!(w.end, NOW + lead());
    }

    #[wasm_bindgen_test]
    fn the_ghost_room_follows_the_volume_length() {
        // VCP 35-ish: 10-minute volumes leave twice the room.
        let slow: Vec<f64> = (0..20).map(|i| NOW - 12_000.0 + i as f64 * 600.0).collect();
        let w = strip_window(StripZoom::Day, NOW, NOW, true, &slow, None, None);
        assert!((w.end - (NOW + MINI_VOLUMES * 600.0 * MINI_LEAD_SHARE)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn landscape_keeps_the_spans_and_gets_more_pixels_per_volume() {
        // The window has no width in it; a wider lane is more px per unit.
        let s = starts(NOW - 100_000.0, NOW);
        let w = strip_window(StripZoom::Day, NOW, NOW, true, &s, None, None);
        let px_per_volume = |lane_px: f64| lane_px / w.span() * DEFAULT_VOLUME_SECS;
        let portrait = px_per_volume(330.0);
        let landscape = px_per_volume(780.0);
        assert!(portrait > 1.0 && portrait < 2.0, "{portrait}");
        assert!(landscape > 2.0 * portrait);
        let v = strip_window(StripZoom::Volumes, NOW, NOW, true, &s, None, None);
        assert_eq!(v.span(), VOLUMES_SPAN_SECS);
        assert!((330.0 / v.span() * DEFAULT_VOLUME_SECS - 9.17).abs() < 0.01);
    }

    #[wasm_bindgen_test]
    fn the_zoomed_out_stops_ask_for_the_days_listing() {
        let w = MiniWindow {
            start: 0.0,
            end: DAY_SPAN_SECS,
        };
        assert_eq!(
            activity_window(StripZoom::Day, w, 330.0),
            Some((0.0, DAY_SPAN_SECS, 330.0))
        );
        assert_eq!(
            activity_window(StripZoom::Volumes, w, 330.0),
            Some((0.0, DAY_SPAN_SECS, 330.0))
        );
        // Cuts asks for the three hours around its middle, as 3 H would.
        let cuts = MiniWindow {
            start: 10_000.0,
            end: 11_000.0,
        };
        assert_eq!(
            activity_window(StripZoom::Cuts, cuts, 330.0),
            Some((
                10_500.0 - VOLUMES_SPAN_SECS / 2.0,
                10_500.0 + VOLUMES_SPAN_SECS / 2.0,
                330.0
            ))
        );
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
    fn a_volume_without_known_cuts_lands_just_inside_its_start() {
        let u = StripUnits::from_containers(&[ScanContainer {
            is_available: true,
            ..container(0.0, 300.0, Vec::new())
        }]);
        assert_eq!(
            snap_scrub(StripZoom::Volumes, 200.0, &u, 10_000.0),
            VOLUME_LAND_SECS
        );
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
        assert_eq!(snap_scrub(StripZoom::Day, 5_100.0, &u, 10_000.0), 5_010.0);
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

    fn scan(start: f64, end: f64, level: f32) -> ScanActivity {
        ScanActivity {
            start,
            end,
            bytes: 1,
            level,
        }
    }

    fn sizes(scans: Vec<ScanActivity>) -> ScanSizes {
        ScanSizes {
            scans,
            known_days: vec![0],
        }
    }

    #[wasm_bindgen_test]
    fn volumes_draws_one_bar_per_volume_from_the_listing() {
        let sz = sizes(vec![
            scan(0.0, 300.0, 0.2),
            scan(300.0, 600.0, 0.9),
            scan(600.0, 900.0, 0.1),
            scan(20_000.0, 20_300.0, 0.5),
        ]);
        let w = MiniWindow {
            start: 0.0,
            end: VOLUMES_SPAN_SECS,
        };
        let bars = activity_bars(StripZoom::Volumes, w, &sz);
        assert_eq!(
            bars.iter().map(|b| (b.start, b.end)).collect::<Vec<_>>(),
            vec![(0.0, 300.0), (300.0, 600.0), (600.0, 900.0)],
            "one per volume in the window"
        );
        assert!(bars[1].frac > bars[0].frac && bars[0].frac >= bars[2].frac);
        assert!(activity_bars(StripZoom::Cuts, w, &sz).is_empty());
    }

    #[wasm_bindgen_test]
    fn day_draws_one_bar_per_half_hour_as_tall_as_its_stormiest_volume() {
        let sz = sizes(vec![
            scan(0.0, 300.0, 0.1),
            scan(300.0, 600.0, 0.8),
            scan(1_800.0, 2_100.0, 0.2),
            scan(2_100.0, 2_400.0, 0.2),
            scan(7_200.0, 7_500.0, 0.3),
        ]);
        let w = MiniWindow {
            start: 0.0,
            end: DAY_SPAN_SECS,
        };
        let bars = activity_bars(StripZoom::Day, w, &sz);
        assert_eq!(
            bars.iter().map(|b| (b.start, b.end)).collect::<Vec<_>>(),
            vec![(0.0, 600.0), (1_800.0, 2_400.0), (7_200.0, 7_500.0)],
            "a bucket ends at its last volume"
        );
        let scale = ViewScale::from_levels([0.8, 0.2, 0.3]);
        assert_eq!(bars[0].frac, scale.height_frac(0.8));
        assert_eq!(bars[1].frac, scale.height_frac(0.2));
        assert!(bars[0].frac > bars[2].frac && bars[2].frac >= bars[1].frac);
    }

    #[wasm_bindgen_test]
    fn detached_the_window_holds_while_the_playhead_stays_inside() {
        let s = starts(NOW - 200_000.0, NOW);
        let first = strip_window(
            StripZoom::Volumes,
            NOW,
            NOW - 50_000.0,
            false,
            &s,
            None,
            None,
        );
        // Scrubbing a little either way leaves the window where it is.
        for ph in [NOW - 49_000.0, NOW - 51_000.0] {
            assert_eq!(
                strip_window(StripZoom::Volumes, NOW, ph, false, &s, Some(first), None),
                first
            );
        }
        // Into the right margin: the window slides to keep the margin.
        let margin = VOLUMES_SPAN_SECS * EDGE_SHARE;
        let ph = first.end - margin / 2.0;
        let w = strip_window(StripZoom::Volumes, NOW, ph, false, &s, Some(first), None);
        assert!((w.end - (ph + margin)).abs() < 1e-6);
        assert_eq!(w.span(), VOLUMES_SPAN_SECS);
        // Into the left margin, likewise.
        let ph = first.start + margin / 2.0;
        let w = strip_window(StripZoom::Volumes, NOW, ph, false, &s, Some(first), None);
        assert!((w.start - (ph - margin)).abs() < 1e-6);
        // Off the window (a typed time): centred on it.
        let ph = NOW - 150_000.0;
        let w = strip_window(StripZoom::Volumes, NOW, ph, false, &s, Some(first), None);
        assert_eq!((w.start + w.end) / 2.0, ph);
        // A window of another stop's span is not held.
        let w = strip_window(
            StripZoom::Day,
            NOW,
            NOW - 50_000.0,
            false,
            &s,
            Some(first),
            None,
        );
        assert_eq!((w.start + w.end) / 2.0, NOW - 50_000.0);
    }

    #[wasm_bindgen_test]
    fn tapping_back_from_live_keeps_the_window_and_pinned_slides_with_now() {
        let s = starts(NOW - 100_000.0, NOW);
        let pinned = strip_window(StripZoom::Volumes, NOW, NOW, true, &s, None, None);
        assert!((pinned.end - (NOW + lead())).abs() < 1e-9);
        // A tap on a volume an hour back detaches; the strip does not jump.
        let w = strip_window(
            StripZoom::Volumes,
            NOW,
            NOW - 3_600.0,
            false,
            &s,
            Some(pinned),
            None,
        );
        assert_eq!(w, pinned);
        // Pinned, the window ignores the last one and follows now.
        let later = strip_window(
            StripZoom::Volumes,
            NOW + 300.0,
            NOW + 300.0,
            true,
            &s,
            Some(pinned),
            None,
        );
        assert!((later.end - pinned.end - 300.0).abs() < 1e-9);
        // Detached, it never reaches past now's ghost room.
        let w = strip_window(
            StripZoom::Day,
            NOW,
            NOW - 60.0,
            false,
            &s,
            Some(pinned),
            None,
        );
        assert!(w.end <= NOW + lead() + 1e-9);
    }

    // ── 15 MIN is 3 H closer in ───────────────────────────────────────

    fn shadow(start: f64, end: f64, state: FrameCellState) -> ScanContainer {
        ScanContainer {
            is_available: true,
            vcp: 0,
            sweep_spans: Vec::new(),
            // The one placeholder cell for the tilt, spanning the volume.
            ..container(start, end, vec![cell(start, end, state)])
        }
    }

    /// A held volume, a volume not on the device, the live volume with one
    /// cut in and its next cut projected, and the next volume's ghost.
    fn cuts_strip() -> Vec<ScanContainer> {
        let mut live = container(
            600.0,
            900.0,
            vec![
                cell(600.0, 630.0, FrameCellState::Cached),
                cell(700.0, 730.0, FrameCellState::Projected),
                cell(800.0, 830.0, FrameCellState::Projected),
            ],
        );
        live.is_live = true;
        vec![
            container(
                0.0,
                300.0,
                vec![
                    cell(0.0, 30.0, FrameCellState::Cached),
                    cell(150.0, 180.0, FrameCellState::Cached),
                ],
            ),
            shadow(300.0, 600.0, FrameCellState::Available),
            live,
            ScanContainer {
                is_projected: true,
                ..container(
                    900.0,
                    1_200.0,
                    vec![cell(900.0, 930.0, FrameCellState::Projected)],
                )
            },
        ]
    }

    #[wasm_bindgen_test]
    fn a_placeholder_cell_is_not_a_cut_and_its_volume_lands_on_its_start() {
        let u = StripUnits::from_containers(&cuts_strip());
        assert_eq!(
            u.cuts.iter().map(|c| c.land).collect::<Vec<_>>(),
            vec![30.0, 180.0, 630.0]
        );
        let shadow_volume = u.volumes.iter().find(|v| v.start == 300.0).unwrap();
        assert_eq!(
            shadow_volume.land,
            300.0 + VOLUME_LAND_SECS,
            "just inside its start, not its placeholder's end"
        );
    }

    #[wasm_bindgen_test]
    fn cuts_snaps_per_cut_where_known_and_per_volume_elsewhere() {
        let u = StripUnits::from_containers(&cuts_strip());
        // Inside the held volume: cut by cut.
        assert_eq!(snap_scrub(StripZoom::Cuts, 10.0, &u, 10_000.0), 30.0);
        assert_eq!(snap_scrub(StripZoom::Cuts, 160.0, &u, 10_000.0), 180.0);
        // Anywhere over the volume not on the device: just past its start, the same
        // spot all the way across (no hop to the next volume).
        for ts in [305.0, 450.0, 595.0] {
            assert_eq!(snap_scrub(StripZoom::Cuts, ts, &u, 10_000.0), 310.0);
        }
        // The live volume's cut.
        assert_eq!(snap_scrub(StripZoom::Cuts, 615.0, &u, 10_000.0), 630.0);
    }

    #[wasm_bindgen_test]
    fn a_tap_lands_on_the_volumes_first_cut_at_every_stop() {
        let u = StripUnits::from_containers(&cuts_strip());
        // Anywhere over the held volume: its first cut, not the one under
        // the finger.
        for ts in [10.0, 160.0, 290.0] {
            assert_eq!(tap_landing(ts, &u, 10_000.0), 30.0);
        }
        // The volume not on the device: just inside its start.
        assert_eq!(tap_landing(450.0, &u, 10_000.0), 310.0);
    }

    #[wasm_bindgen_test]
    fn tapping_the_oldest_block_from_live_holds_the_now_window() {
        // Volumes every 7 minutes, the live one 5 minutes in.
        let s: Vec<f64> = (0..20)
            .map(|k| NOW - 300.0 - 420.0 * k as f64)
            .rev()
            .collect();
        let pinned = strip_window(StripZoom::Cuts, NOW, NOW, true, &s, None, None);
        let oldest = s.iter().copied().find(|&t| t >= pinned.start).unwrap();
        let tapped = volume_landing(oldest, oldest + 420.0);
        let w = strip_window(StripZoom::Cuts, NOW, tapped, false, &s, Some(pinned), None);
        assert!(w.end >= NOW, "{w:?} lost now (pinned {pinned:?})");
        assert!(w.start <= oldest);
        // A minute later now is still in view and the tapped block too.
        let later = NOW + 60.0;
        let w2 = strip_window(StripZoom::Cuts, later, tapped, false, &s, Some(w), None);
        assert!(w2.end >= later, "{w2:?} lost now");
        assert!(w2.start < tapped, "{w2:?} lost the tapped block");
    }

    #[wasm_bindgen_test]
    fn a_four_volume_loop_to_now_shows_its_first_block_whole_with_room() {
        // Volumes every 7 minutes, the live one 5 minutes in; four frames
        // pinned to now start 20 s into the fourth volume back.
        let s: Vec<f64> = (0..20)
            .map(|k| NOW - 300.0 - 420.0 * k as f64)
            .rev()
            .collect();
        let first = NOW - 300.0 - 3.0 * 420.0;
        let range = (first + 20.0, NOW);
        // The playhead looping anywhere in it, and any of the near-now
        // blocks, change nothing.
        for playhead in [range.0, first + 500.0, NOW - 100.0] {
            let w = strip_window(StripZoom::Cuts, NOW, playhead, true, &s, None, Some(range));
            assert!(
                first - w.start >= RANGE_EDGE_SHARE * w.span() - 1e-6,
                "{w:?} clips the block at {first}"
            );
            assert!(w.end > NOW);
        }
    }

    #[wasm_bindgen_test]
    fn a_still_finger_does_not_scrub_again() {
        let mut t = StripTouch::default();
        t.step(touch(true, 100.0, 0.0, None));
        assert!(t.scrub_moved(100.0), "the first scrub of a press");
        assert!(!t.scrub_moved(100.0), "units arriving under a still finger");
        assert!(!t.scrub_moved(100.2));
        assert!(t.scrub_moved(104.0));
        // A new press starts fresh.
        t.step(touch(false, 0.0, 0.5, None));
        t.step(touch(true, 104.0, 1.0, None));
        assert!(t.scrub_moved(104.0));
    }

    #[wasm_bindgen_test]
    fn cuts_far_back_holds_its_window_while_the_finger_scrubs() {
        let s = starts(NOW - 50_000.0, NOW);
        let playhead = NOW - 20_000.0;
        let first = strip_window(StripZoom::Cuts, NOW, playhead, false, &s, None, None);
        assert_eq!(
            first,
            mini_window(NOW, playhead, false, &s),
            "lands centred"
        );
        // A hop of one volume inside it: the strip stays put.
        let next = strip_window(
            StripZoom::Cuts,
            NOW,
            playhead + 300.0,
            false,
            &s,
            Some(first),
            None,
        );
        assert_eq!(next, first);
        // Near the right edge it slides just enough.
        let edge = first.end - 10.0;
        let slid = strip_window(StripZoom::Cuts, NOW, edge, false, &s, Some(first), None);
        assert!(slid.start > first.start);
        assert!((slid.end - (edge + first.span() * EDGE_SHARE)).abs() < 1e-6);
        // A window from another stop is not held.
        let day = strip_window(StripZoom::Day, NOW, playhead, false, &s, None, None);
        assert_eq!(
            strip_window(StripZoom::Cuts, NOW, playhead, false, &s, Some(day), None),
            first
        );
    }
}
