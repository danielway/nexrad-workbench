//! The phone strip's zoom stops (phone timeline §1): 15 MIN, 3 H and DAY.
//!
//! Continuous zoom suits a wheel, not a thumb, so the phone's mini timeline
//! has three stops, each with its own window rule and scrub unit:
//!
//! - **Cuts** (pill "15 MIN") — 3 H closer in: the last three volumes
//!   ([`crate::core::phone::mini_window`]), each on the device as its cut
//!   cells and each not on it as its storm bar ([`cut_volumes`]). A scrub
//!   snaps per cut where cuts are known and per volume elsewhere
//!   ([`StripUnits::landing_units`]). A selected range always shows whole:
//!   the window widens past three volumes when the range needs it
//!   ([`strip_window`]'s `range`).
//! - **Volumes** (pill "3 H") — three hours of storm-activity bars, one per
//!   volume, as tall as the weather the volume held ([`activity_bars`]).
//! - **Day** (pill "DAY") — 24 hours of the same bars, one per half-hour
//!   bucket.
//!
//! The zoomed-out stops draw what the desktop overview draws zoomed in to
//! volumes: bars from the volumes' sizes in the day's listing, a slate
//! underline under the volumes already on the device ([`on_device_spans`]),
//! and stretches with no sizes yet as a quiet wash ([`quiet_spans`]). A scrub
//! there snaps to volumes.
//!
//! Every stop keeps the same marks: the periwinkle ring on the frame on
//! screen ([`ring_center`]), a dashed ring where the playhead's frame will
//! land while it is not there yet ([`pending_mark`]), the white playhead line when detached, the now
//! line, and a small tab under it ([`strip_cap`]): filled red only for LIVE,
//! outlined for REJOIN and GO LIVE, and an outlined edge tab at the right
//! (`GO LIVE ›`, `REJOIN ›`) when now is off the window. Ruler labels give
//! way to the tab and step aside for range handles ([`ruler_label_xs`]).
//!
//! Pinned, the window ends just past now and slides with it. Detached, it
//! centres on the playhead when the playhead arrives from off the window,
//! holds still while the finger works inside it, and slides once the
//! playhead nears an edge ([`strip_window`]). Landscape keeps the spans and
//! gets more pixels per unit.
//!
//! A pinch moves one stop and settles on release ([`PinchTrack`]); the zoom
//! pill names what the strip spans and taps through the stops
//! ([`zoom_pill_label`], [`next_stop`]). Zoom and selection are independent:
//! zooming never touches the playhead or the pin, and going live keeps the
//! zoom ([`strip_zoom_after`]).
//!
//! All pure; `ui::timeline::mini` draws what these return.

use crate::core::phone::{
    mini_lead_secs, mini_near_now, mini_window, typical_volume_secs, MiniWindow, PressOut,
    PressTrack, MINI_VOLUMES,
};
use crate::core::phone_range::RangeEnd;
use crate::core::session::NowCap;
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
    /// The last three volumes, every cut ("15 MIN").
    #[default]
    Cuts,
    /// Three hours of storm bars, one per volume ("3 H").
    Volumes,
    /// 24 hours of storm bars, one per half-hour ("DAY").
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

    /// Whether the strip draws storm bars rather than cuts, and so snaps a
    /// scrub to volumes.
    pub(crate) fn per_volume(self) -> bool {
        self != StripZoom::Cuts
    }
}

/// The zoom pill's words for a stop: what the strip spans.
pub(crate) fn zoom_pill_label(stop: StripZoom) -> &'static str {
    match stop {
        StripZoom::Cuts => "15 MIN",
        StripZoom::Volumes => "3 H",
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
/// Near now (the range ends within two volumes of now, as a range pinned to
/// now always does) it is [`mini_window`] anchored to now, ending at now
/// plus the ghost room; when the range starts before that window's left
/// edge (four frames at a tilt are four volumes), the left edge moves out
/// to [`RANGE_EDGE_SHARE`] of the new span before the range's start.
/// Further back it centres on the range, spanning the three volumes or the
/// range plus that room either side, whichever is wider, and never reaches
/// past now plus the ghost room. Either way the window follows only the
/// range, now and the volumes, never the playhead looping inside the range,
/// so it holds still while the loop plays and slides only with now.
fn cuts_range_window(now: f64, (start, end): (f64, f64), volume_starts: &[f64]) -> MiniWindow {
    let m = RANGE_EDGE_SHARE;
    if mini_near_now(now, end, false, volume_starts) {
        let base = mini_window(now, end, true, volume_starts);
        if start >= base.start {
            return base;
        }
        // start = range start − m · (end − start), solved for start.
        return MiniWindow {
            start: (start - m * base.end) / (1.0 - m),
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

    /// The volume holding `ts`: the newest that starts by then and has not
    /// ended.
    fn volume_at(&self, ts: f64) -> Option<&SnapUnit> {
        self.volumes
            .iter()
            .filter(|v| v.start <= ts + 1.0 && ts < v.end.max(v.start + 1.0))
            .max_by(|a, b| a.start.total_cmp(&b.start))
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

/// Where the strip marks a playhead whose frame has not arrived (the volume
/// under it is downloading, or nothing is on screen yet), so a scrub has a
/// visible result: the landing cut under the playhead at Cuts (else the
/// playhead itself), the volume at Volumes, the half-hour at Day. `None`
/// while pinned, when the frame on screen (`active_ts`, the displayed
/// scan's time) is in the playhead's volume, or when no volume holds the
/// playhead.
pub(crate) fn pending_mark(
    stop: StripZoom,
    playhead: f64,
    pinned: bool,
    active_ts: Option<f64>,
    units: &StripUnits,
) -> Option<f64> {
    if pinned {
        return None;
    }
    let v = units.volume_at(playhead)?;
    if active_ts.is_some_and(|ts| ts >= v.start - 1.0 && ts < v.end.max(v.start + 1.0)) {
        return None;
    }
    let mid = |a: f64, b: f64| (a + b) / 2.0;
    Some(match stop {
        StripZoom::Cuts => units
            .cuts
            .iter()
            .filter(|c| c.start >= v.start - 1.0 && c.start < v.end)
            .find(|c| playhead >= c.start - 1.0 && playhead <= c.end + 1.0)
            .map_or(playhead, |c| mid(c.start, c.end)),
        StripZoom::Volumes => mid(v.start, v.end),
        StripZoom::Day => {
            (playhead / DAY_BUCKET_SECS).floor() * DAY_BUCKET_SECS + DAY_BUCKET_SECS / 2.0
        }
    })
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

/// The strip's touch state between frames: one press, one pinch, and the
/// range handle the press began on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StripTouch {
    pub press: Option<PressTrack>,
    pub pinch: Option<PinchTrack>,
    /// The press began inside this handle's touch target: a drag moves the
    /// range's end, never the playhead.
    pub grabbed: Option<RangeEnd>,
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
    /// The range handle whose touch target holds the finger
    /// ([`crate::core::phone_range::handle_at`]); read when a press begins.
    pub handle: Option<RangeEnd>,
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
    /// long-press nor taken over by a pinch, and is not moving a handle.
    pub may_seek: bool,
    /// A range handle is moving with the finger this frame.
    pub moving: Option<RangeEnd>,
    /// A moving handle was let go this frame: drop it.
    pub dropped: Option<RangeEnd>,
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
    ///
    /// A press that begins inside a range handle's target belongs to the
    /// handle: once it moves it moves that end and never scrubs; let go, it
    /// drops. Held still it is still a tap (a seek) or a long-press (the
    /// sheet), so the handle's target costs neither.
    pub(crate) fn step(&mut self, i: StripTouchInput) -> StripTouchOut {
        let was = self.press;
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
        if was.is_none() && press.is_some() {
            self.grabbed = i.handle;
        }
        let dragging = |p: Option<PressTrack>| p.is_some_and(|p| p.scrubbing() && p.seeks());
        let moving = self.grabbed.filter(|_| dragging(press));
        let dropped = self
            .grabbed
            .filter(|_| press.is_none() && dragging(was) && !pinch_out.claimed);
        if press.is_none() {
            self.grabbed = None;
            self.scrub_x = None;
        }
        let may_seek = !pinch_out.claimed
            && !press_out.swallow_release
            && press_out.long_press.is_none()
            && press.is_none_or(|p| p.seeks())
            && moving.is_none()
            && dropped.is_none();
        StripTouchOut {
            press: press_out,
            claimed: pinch_out.claimed,
            settle: pinch_out.settle,
            may_seek,
            moving,
            dropped,
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

/// How far before a container's start a listed volume's start may fall and
/// still be that container (a volume's key time can lead its first sweep).
const KEY_SLACK_SECS: f64 = 30.0;

/// How one volume looks at the Cuts stop.
#[derive(Clone, Debug)]
pub(crate) enum CutLook {
    /// On the device (or the live volume): its cut cells, a filled cell
    /// being a frame on the device. The live volume's last cell may be the
    /// ghost of its next cut (`Projected`), the only cell past now.
    Cells(Vec<FrameCell>),
    /// Not on the device: the 3 H stop's storm bar (`frac`, as a fraction
    /// of the lane), or a quiet wash when its size isn't known; `fetching`
    /// while it is queued or downloading.
    Bar { frac: Option<f32>, fetching: bool },
}

/// One volume on the Cuts stop.
#[derive(Clone, Debug)]
pub(crate) struct CutVolume {
    pub start: f64,
    /// Where its box ends: the volume's end, or for the live volume now
    /// (or its last cut, if later), so nothing but the ghost draws past
    /// now.
    pub end: f64,
    pub look: CutLook,
}

/// What the Cuts stop draws (15 MIN is 3 H closer in): each volume on the
/// device as its cut cells, each volume that is not as the storm bar 3 H
/// draws for it (from `sizes`, scaled over [`sizes_span`] so it is as tall
/// as at 3 H; a quiet wash when unknown), with no tilt or VCP words and no
/// dashed shell. Volumes the day's listing knows that no container covers
/// draw as bars too. The next volume's ghost is not drawn; the live
/// volume carries only the ghost of its next cut (else the ghost volume's
/// first), and its box stops at now. Oldest first.
pub(crate) fn cut_volumes(
    containers: &[ScanContainer],
    sizes: &ScanSizes,
    window: MiniWindow,
    now: f64,
) -> Vec<CutVolume> {
    let (lo, hi) = sizes_span(StripZoom::Cuts, window);
    let scale = ViewScale::from_levels(
        sizes
            .scans
            .iter()
            .filter(|s| s.end >= lo && s.start <= hi)
            .map(|s| s.level),
    );
    let frac_at = |start: f64, end: f64| {
        sizes
            .at((start + end) / 2.0)
            .map(|s| scale.height_frac(s.level))
    };
    let next_ghost_volume = containers
        .iter()
        .filter(|c| c.is_projected)
        .min_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
    let mut out: Vec<CutVolume> = Vec::new();
    for c in containers.iter().filter(|c| !c.is_projected) {
        if c.is_available {
            out.push(CutVolume {
                start: c.start_secs,
                end: c.end_secs.max(c.start_secs),
                look: CutLook::Bar {
                    frac: frac_at(c.start_secs, c.end_secs),
                    fetching: c.cells.iter().any(|cell| {
                        matches!(
                            cell.state,
                            FrameCellState::InFlight | FrameCellState::Queued
                        )
                    }),
                },
            });
            continue;
        }
        let mut cells: Vec<FrameCell> = c
            .cells
            .iter()
            .filter(|cell| cell.state != FrameCellState::Projected)
            .cloned()
            .collect();
        let mut end = c.end_secs.max(c.start_secs);
        if c.is_live {
            let last = cells.iter().map(|cell| cell.end_secs).fold(now, f64::max);
            end = end.min(last).max(c.start_secs);
            let ghost = c
                .cells
                .iter()
                .filter(|cell| cell.state == FrameCellState::Projected)
                .min_by(|a, b| a.start_secs.total_cmp(&b.start_secs))
                .or_else(|| next_ghost_volume.and_then(|g| g.cells.first()));
            if let Some(g) = ghost {
                cells.push(g.clone());
            }
        }
        out.push(CutVolume {
            start: c.start_secs,
            end,
            look: CutLook::Cells(cells),
        });
    }
    let covered = |ts: f64| {
        containers
            .iter()
            .any(|c| ts >= c.start_secs - KEY_SLACK_SECS && ts < c.end_secs.max(c.start_secs + 1.0))
    };
    out.extend(
        sizes
            .scans
            .iter()
            .filter(|s| s.end >= window.start && s.start <= window.end && s.start <= now)
            .filter(|s| !covered(s.start))
            .map(|s| CutVolume {
                start: s.start,
                end: s.end.max(s.start),
                look: CutLook::Bar {
                    frac: Some(scale.height_frac(s.level)),
                    fetching: false,
                },
            }),
    );
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// The strip's volumes (containers) that no volume in `sizes` covers: the
/// stretches a zoomed-out stop draws as a quiet wash, as the desktop draws
/// days whose sizes aren't known yet, rather than as boxes. A volume newer
/// than the day's listing (the live one, say) is one of them.
pub(crate) fn quiet_spans(containers: &[ScanContainer], sizes: &ScanSizes) -> Vec<(f64, f64)> {
    containers
        .iter()
        .filter(|c| !c.is_projected && c.end_secs > c.start_secs)
        .filter(|c| sizes.at((c.start_secs + c.end_secs) / 2.0).is_none())
        .map(|c| (c.start_secs, c.end_secs))
        .collect()
}

/// The on-device underline at a zoomed-out stop: the volumes with any sweep
/// on this device, oldest first. Runs closer than `join_secs` merge into one
/// stretch (at Day a volume is a pixel or two wide); at Volumes pass 0 and
/// each volume keeps its own piece.
pub(crate) fn on_device_spans(containers: &[ScanContainer], join_secs: f64) -> Vec<(f64, f64)> {
    let mut held: Vec<(f64, f64)> = containers
        .iter()
        .filter(|c| !c.is_projected && !c.is_available)
        .map(|c| (c.start_secs, c.end_secs.max(c.start_secs)))
        .collect();
    held.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (s, e) in held {
        match out.last_mut() {
            Some(last) if join_secs > 0.0 && s - last.1 <= join_secs => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Where the periwinkle ring sits (Unix seconds, its centre): on the frame
/// on screen. At Cuts that is the active cell (else the cell of the volume
/// holding `active_ts`, the displayed scan's time); at Volumes the volume
/// with the active cell, else the one holding `active_ts`, or the bar that
/// does when the strip has no container for it; at Day the half-hour bucket
/// holding it. `None` when nothing is on screen.
pub(crate) fn ring_center(
    stop: StripZoom,
    active_ts: Option<f64>,
    containers: &[ScanContainer],
    bars: &[StripBar],
) -> Option<f64> {
    let mid = |a: f64, b: f64| (a + b) / 2.0;
    let active = containers.iter().find_map(|c| {
        c.cells
            .iter()
            .find(|cell| cell.is_active)
            .map(|cell| (c, cell))
    });
    // The volume holding the displayed scan's time: the newest one that
    // starts by then, while it is still within a volume's reach (a live
    // volume's display end can trail the frame on screen).
    let holding = |ts: f64| {
        containers
            .iter()
            .filter(|c| !c.is_projected && c.start_secs <= ts + 1.0)
            .max_by(|a, b| a.start_secs.total_cmp(&b.start_secs))
            .filter(|c| ts < c.end_secs.max(c.start_secs + MAX_RING_REACH_SECS))
    };
    match stop {
        StripZoom::Cuts => {
            if let Some((_, cell)) = active {
                return Some(mid(cell.start_secs, cell.end_secs));
            }
            let ts = active_ts?;
            let c = holding(ts)?;
            c.cells
                .iter()
                .filter(|cell| cell.state != FrameCellState::Projected)
                .find(|cell| ts >= cell.start_secs - 1.0 && ts < cell.end_secs)
                .or_else(|| c.cells.first())
                .map(|cell| mid(cell.start_secs, cell.end_secs))
        }
        StripZoom::Volumes => {
            if let Some((c, _)) = active {
                return Some(mid(c.start_secs, c.end_secs));
            }
            let ts = active_ts?;
            holding(ts)
                .map(|c| mid(c.start_secs, c.end_secs))
                .or_else(|| {
                    bars.iter()
                        .find(|b| ts >= b.start - 1.0 && ts < b.end)
                        .map(|b| mid(b.start, b.end))
                })
        }
        StripZoom::Day => {
            let ts = active_ts?;
            let b = (ts / DAY_BUCKET_SECS).floor() * DAY_BUCKET_SECS;
            Some(b + DAY_BUCKET_SECS / 2.0)
        }
    }
}

/// How long after a volume's start the displayed scan's time still counts
/// as that volume's, for the ring, when the volume's display end is
/// shorter (the live volume).
const MAX_RING_REACH_SECS: f64 = 900.0;

/// The now tab under the now line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StripCap {
    /// The word: LIVE, REJOIN or GO LIVE, with ` ›` on the edge tab.
    pub label: String,
    /// Filled red (LIVE) or outlined (REJOIN, GO LIVE).
    pub filled: bool,
    /// The tab's left edge, in points.
    pub x0: f32,
    /// Its width, in points.
    pub width: f32,
    /// Now is off the window and the tab sits at the lane's right edge.
    pub edge: bool,
}

/// The tab's padding around its word, in points (both sides together).
pub(crate) const CAP_PAD_X: f32 = 8.0;

/// How far the tab keeps from the lane's right end, in points.
pub(crate) const CAP_EDGE_GAP: f32 = 4.0;

/// The now tab. `now_x` is the now line's x when now is in the window
/// (`None` when it is off it); `lane_right` the lane's right end;
/// `measure` a word's width in points. The tab hangs just right of the now
/// line, or just left of it when the lane ends first, so it never covers
/// the line; off the window it is an edge tab at the right end pointing at
/// now. Filled only for LIVE: red means
/// now, and REJOIN and GO LIVE are invitations, outlined.
pub(crate) fn strip_cap(
    cap: NowCap,
    now_x: Option<f32>,
    lane_right: f32,
    measure: &dyn Fn(&str) -> f32,
) -> StripCap {
    let edge = now_x.is_none();
    let label = if edge {
        format!("{} \u{203a}", cap.label())
    } else {
        cap.label().to_string()
    };
    let width = measure(&label) + CAP_PAD_X;
    let right_most = lane_right - CAP_EDGE_GAP - width;
    let x0 = match now_x {
        Some(x) if x + 3.0 <= right_most => x + 3.0,
        Some(x) => x - 3.0 - width,
        None => right_most,
    };
    StripCap {
        label,
        filled: cap == NowCap::Live && !edge,
        x0,
        width,
        edge,
    }
}

/// What a tap on the now tab does. LIVE does nothing (already live);
/// REJOIN re-pins to the running stream; GO LIVE starts one. The edge tab
/// first brings now into view (`CenterTimelineOnNow`, for the desktop
/// timeline the same session shares).
pub(crate) fn cap_intents(cap: NowCap, edge: bool) -> Vec<Intent> {
    let attach = match cap {
        NowCap::Live => return Vec::new(),
        NowCap::Rejoin => Intent::ReturnToLive,
        NowCap::GoLive => Intent::GoLive,
    };
    if edge {
        vec![Intent::CenterTimelineOnNow, attach]
    } else {
        vec![attach]
    }
}

/// Where each ruler label draws, as its left x, or `None` when it does
/// not. `labels` are each label's natural left x and width; `tab` is the
/// now tab's span and `handles` the range handles' tap targets in its row
/// (left, right). A label draws at its natural x when that fits inside
/// `lane` (left, right), clears the tab, every handle and the label before
/// it that drew. A label under the tab gives way. A label blocked only by a
/// handle steps aside to the nearest clear place just left or right of a
/// handle (inside the lane, clear of the tab, the handles and the label
/// before it), and gives way only when there is none.
pub(crate) fn ruler_label_xs(
    labels: &[(f32, f32)],
    lane: (f32, f32),
    tab: Option<(f32, f32)>,
    handles: &[(f32, f32)],
) -> Vec<Option<f32>> {
    const GAP: f32 = 3.0;
    let clear_of = |x: f32, w: f32, (l, r): (f32, f32)| x + w + GAP <= l || x >= r + GAP;
    let mut last_right = f32::NEG_INFINITY;
    labels
        .iter()
        .map(|&(x, w)| {
            let ok = |x: f32| {
                x >= lane.0
                    && x + w <= lane.1
                    && x >= last_right + GAP
                    && tab.is_none_or(|t| clear_of(x, w, t))
                    && handles.iter().all(|&h| clear_of(x, w, h))
            };
            let under_tab = tab.is_some_and(|t| !clear_of(x, w, t));
            let blocked_by_handle = handles.iter().any(|&h| !clear_of(x, w, h));
            let at = if ok(x) {
                Some(x)
            } else if blocked_by_handle && !under_tab {
                handles
                    .iter()
                    .flat_map(|&(l, r)| [l - GAP - w, r + GAP])
                    .filter(|&c| ok(c))
                    .min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs()))
            } else {
                None
            };
            if let Some(at) = at {
                last_right = at + w;
            }
            at
        })
        .collect()
}

/// One label on the strip's ruler.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RulerTick {
    pub ts: f64,
    pub label: String,
}

/// The ruler under the strip, in the chosen zone (`tz_offset_secs` east of
/// UTC; 0 for UTC): each volume's start (`HH:MM`) at Cuts, every hour
/// (`HH:00`) at Volumes, and every hour at Day, labelled every six hours
/// (`00 06 12 18`; the other hours are unlabelled ticks, `label` empty).
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
        StripZoom::Day => every(3600.0, &|h, _| {
            if h % 6 == 0 {
                format!("{h:02}")
            } else {
                String::new()
            }
        }),
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
        assert_eq!(zoom_pill_label(StripZoom::Cuts), "15 MIN");
        assert_eq!(zoom_pill_label(StripZoom::Volumes), "3 H");
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
        // About 5 % of the span before the range's start, and still ending
        // at now plus the ghost room.
        assert!((range.0 - w.start - RANGE_EDGE_SHARE * w.span()).abs() < 1e-6);
        assert_eq!(w.end, NOW + lead());
        assert!(w.span() > base.span());
    }

    #[wasm_bindgen_test]
    fn cuts_keeps_its_window_for_a_range_that_already_fits() {
        let s = live_volumes();
        let base = mini_window(NOW, NOW, true, &s);
        for range in [last_frames(3.0), last_frames(2.0)] {
            let w = strip_window(StripZoom::Cuts, NOW, range.0, true, &s, None, Some(range));
            assert_eq!(w, base);
            assert_range_inside(w, range);
        }
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
            handle: None,
        }
    }

    fn on_handle(i: StripTouchInput, end: RangeEnd) -> StripTouchInput {
        StripTouchInput {
            handle: Some(end),
            ..i
        }
    }

    #[wasm_bindgen_test]
    fn a_press_on_a_handle_moves_the_end_and_never_scrubs() {
        let mut t = StripTouch::default();
        // Lands inside the start handle's target.
        let out = t.step(on_handle(touch(true, 100.0, 0.0, None), RangeEnd::Start));
        assert_eq!(t.grabbed, Some(RangeEnd::Start));
        assert_eq!(out.moving, None, "not moving until it moves");
        // It moves past the slop: the handle moves, the playhead does not.
        let out = t.step(touch(true, 120.0, 0.1, None));
        assert_eq!(out.moving, Some(RangeEnd::Start));
        assert!(!out.may_seek);
        let out = t.step(touch(true, 160.0, 0.2, None));
        assert_eq!(out.moving, Some(RangeEnd::Start));
        assert!(!out.may_seek);
        // Let go: it drops, and that is not a tap.
        let out = t.step(touch(false, 0.0, 0.3, None));
        assert_eq!(out.dropped, Some(RangeEnd::Start));
        assert!(!out.may_seek);
        assert_eq!(t.grabbed, None);
        // The next press, off the handles, scrubs as usual.
        t.step(touch(true, 200.0, 1.0, None));
        let out = t.step(touch(true, 230.0, 1.1, None));
        assert_eq!(out.moving, None);
        assert!(out.may_seek);
    }

    #[wasm_bindgen_test]
    fn only_where_the_press_began_decides_the_handle() {
        let mut t = StripTouch::default();
        // A scrub that passes over a handle stays a scrub.
        t.step(touch(true, 50.0, 0.0, None));
        let out = t.step(on_handle(touch(true, 100.0, 0.1, None), RangeEnd::End));
        assert_eq!(out.moving, None);
        assert!(out.may_seek);
    }

    #[wasm_bindgen_test]
    fn a_still_press_on_a_handle_is_still_a_tap_or_a_long_press() {
        let mut t = StripTouch::default();
        t.step(on_handle(touch(true, 100.0, 0.0, None), RangeEnd::End));
        let out = t.step(touch(false, 0.0, 0.1, None));
        assert!(out.may_seek, "a tap seeks");
        assert_eq!(out.dropped, None);

        t.step(on_handle(touch(true, 100.0, 1.0, None), RangeEnd::End));
        let out = t.step(touch(true, 101.0, 1.6, None));
        assert!(out.press.long_press.is_some(), "the sheet opens");
        let out = t.step(touch(false, 0.0, 1.7, None));
        assert_eq!(out.dropped, None);
        assert!(!out.may_seek);
    }

    #[wasm_bindgen_test]
    fn a_pinch_takes_a_moving_handle_and_drops_nothing() {
        let mut t = StripTouch::default();
        t.step(on_handle(touch(true, 100.0, 0.0, None), RangeEnd::Start));
        assert_eq!(
            t.step(touch(true, 130.0, 0.1, None)).moving,
            Some(RangeEnd::Start)
        );
        let out = t.step(touch(true, 140.0, 0.2, pinch(1.3, true)));
        assert_eq!(out.moving, None);
        t.step(touch(true, 140.0, 0.3, None));
        let out = t.step(touch(false, 0.0, 0.4, None));
        assert_eq!(out.dropped, None);
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
    fn volumes_without_sizes_are_quiet_and_the_ghost_is_not_drawn() {
        let sz = sizes(vec![scan(0.0, 300.0, 0.2)]);
        let live = container(
            300.0,
            600.0,
            vec![cell(310.0, 330.0, FrameCellState::Cached)],
        );
        let ghost = ScanContainer {
            is_projected: true,
            ..container(600.0, 900.0, Vec::new())
        };
        let listed = container(0.0, 300.0, Vec::new());
        assert_eq!(
            quiet_spans(&[listed, live, ghost], &sz),
            vec![(300.0, 600.0)]
        );
    }

    #[wasm_bindgen_test]
    fn the_underline_marks_held_volumes_and_merges_runs_at_day() {
        let held = |s: f64| {
            container(
                s,
                s + 300.0,
                vec![cell(s, s + 20.0, FrameCellState::Cached)],
            )
        };
        let server = ScanContainer {
            is_available: true,
            ..container(600.0, 900.0, Vec::new())
        };
        let cs = vec![held(0.0), held(300.0), server, held(900.0)];
        assert_eq!(
            on_device_spans(&cs, 0.0),
            vec![(0.0, 300.0), (300.0, 600.0), (900.0, 1_200.0)]
        );
        assert_eq!(
            on_device_spans(&cs, 1.0),
            vec![(0.0, 600.0), (900.0, 1_200.0)]
        );
        assert_eq!(on_device_spans(&cs, 400.0), vec![(0.0, 1_200.0)]);
    }

    #[wasm_bindgen_test]
    fn the_ring_sits_on_the_frame_on_screen_at_every_stop() {
        let mut c = container(
            0.0,
            300.0,
            vec![
                cell(10.0, 40.0, FrameCellState::Cached),
                cell(150.0, 180.0, FrameCellState::Cached),
            ],
        );
        c.cells[1].is_active = true;
        let cs = vec![c];
        assert_eq!(
            ring_center(StripZoom::Cuts, Some(0.0), &cs, &[]),
            Some(165.0)
        );
        assert_eq!(
            ring_center(StripZoom::Volumes, Some(0.0), &cs, &[]),
            Some(150.0)
        );
        // A volume the strip holds no container for: the bar's centre.
        let bars = [StripBar {
            start: 900.0,
            end: 1_200.0,
            frac: 0.5,
        }];
        assert_eq!(
            ring_center(StripZoom::Volumes, Some(900.0), &[], &bars),
            Some(1_050.0)
        );
        assert_eq!(
            ring_center(StripZoom::Day, Some(2_000.0), &cs, &[]),
            Some(2_700.0)
        );
        // No cell flagged (the live volume, its display end short of the
        // frame): the volume that started last before the frame.
        let live = vec![container(
            0.0,
            30.0,
            vec![cell(5.0, 20.0, FrameCellState::Cached)],
        )];
        assert_eq!(
            ring_center(StripZoom::Volumes, Some(100.0), &live, &[]),
            Some(15.0)
        );
        assert_eq!(
            ring_center(StripZoom::Cuts, Some(100.0), &live, &[]),
            Some(12.5)
        );
        assert_eq!(
            ring_center(StripZoom::Volumes, Some(5_000.0), &live, &[]),
            None
        );
        // Nothing on screen, no ring.
        assert_eq!(ring_center(StripZoom::Day, None, &cs, &[]), None);
        assert_eq!(ring_center(StripZoom::Cuts, None, &[], &[]), None);
    }

    fn measure(s: &str) -> f32 {
        s.chars().count() as f32 * 6.0
    }

    #[wasm_bindgen_test]
    fn the_cap_is_a_small_tab_filled_only_for_live() {
        let live = strip_cap(NowCap::Live, Some(200.0), 380.0, &measure);
        assert!(live.filled && !live.edge);
        assert_eq!(live.label, "LIVE");
        assert_eq!(live.x0, 203.0, "just right of the now line");
        assert_eq!(live.width, 24.0 + CAP_PAD_X);
        for cap in [NowCap::Rejoin, NowCap::GoLive] {
            let c = strip_cap(cap, Some(200.0), 380.0, &measure);
            assert!(!c.filled, "{cap:?} is outlined");
            assert_eq!(c.label, cap.label());
        }
        // Near the right end the tab hangs left of the line instead, clear
        // of it.
        let c = strip_cap(NowCap::Live, Some(375.0), 380.0, &measure);
        assert_eq!(c.x0 + c.width, 372.0);
    }

    #[wasm_bindgen_test]
    fn now_off_the_window_is_an_outlined_edge_tab_pointing_at_it() {
        let go = strip_cap(NowCap::GoLive, None, 380.0, &measure);
        assert!(go.edge && !go.filled);
        assert_eq!(go.label, "GO LIVE \u{203a}");
        assert_eq!(go.x0 + go.width, 380.0 - CAP_EDGE_GAP);
        assert_eq!(
            strip_cap(NowCap::Rejoin, None, 380.0, &measure).label,
            "REJOIN \u{203a}"
        );
        // Even a live selection's edge tab is outlined: it is not at now.
        assert!(!strip_cap(NowCap::Live, None, 380.0, &measure).filled);
    }

    #[wasm_bindgen_test]
    fn the_cap_rejoins_or_goes_live_and_live_does_nothing() {
        assert!(cap_intents(NowCap::Live, false).is_empty());
        assert!(matches!(
            cap_intents(NowCap::Rejoin, false)[..],
            [Intent::ReturnToLive]
        ));
        assert!(matches!(
            cap_intents(NowCap::GoLive, false)[..],
            [Intent::GoLive]
        ));
        assert!(matches!(
            cap_intents(NowCap::GoLive, true)[..],
            [Intent::CenterTimelineOnNow, Intent::GoLive]
        ));
        assert!(matches!(
            cap_intents(NowCap::Rejoin, true)[..],
            [Intent::CenterTimelineOnNow, Intent::ReturnToLive]
        ));
    }

    #[wasm_bindgen_test]
    fn ruler_labels_give_way_to_the_cap_the_edge_and_each_other() {
        let labels = [
            (60.0, 27.0),
            (150.0, 27.0),
            (240.0, 27.0),
            (330.0, 27.0),
            (360.0, 27.0),
        ];
        let tab = Some((320.0, 360.0));
        // The last one would cross the lane's end; the one under the cap
        // gives way.
        assert_eq!(
            ruler_label_xs(&labels, (58.0, 380.0), tab, &[]),
            vec![Some(60.0), Some(150.0), Some(240.0), None, None]
        );
        assert_eq!(
            ruler_label_xs(&labels, (58.0, 380.0), None, &[]),
            vec![Some(60.0), Some(150.0), Some(240.0), Some(330.0), None]
        );
        // Two labels too close: the later one gives way.
        assert_eq!(
            ruler_label_xs(&[(60.0, 27.0), (80.0, 27.0)], (0.0, 400.0), None, &[]),
            vec![Some(60.0), None]
        );
    }

    #[wasm_bindgen_test]
    fn a_label_under_a_handle_steps_to_the_nearest_clear_side() {
        let lane = (58.0, 380.0);
        let tab = Some((320.0, 360.0));
        // A handle's target over the second label: it steps right, the
        // nearer side (left would be 124, 26 away; right is 169, 19 away).
        assert_eq!(
            ruler_label_xs(
                &[(60.0, 27.0), (150.0, 27.0), (240.0, 27.0)],
                lane,
                tab,
                &[(142.0, 166.0)]
            ),
            vec![Some(60.0), Some(169.0), Some(240.0)]
        );
        // Nearer on the left: it steps left.
        assert_eq!(
            ruler_label_xs(&[(150.0, 27.0)], lane, tab, &[(170.0, 194.0)]),
            vec![Some(140.0)]
        );
        // The tab is still a hard rule: a label under it gives way even
        // when a handle is there too.
        assert_eq!(
            ruler_label_xs(&[(330.0, 27.0)], lane, tab, &[(318.0, 342.0)]),
            vec![None]
        );
        // A side that would cross the lane, the tab or the label before is
        // not clear; with neither side clear the label gives way.
        assert_eq!(
            ruler_label_xs(&[(60.0, 27.0)], lane, None, &[(52.0, 76.0)]),
            vec![Some(79.0)]
        );
        assert_eq!(
            ruler_label_xs(&[(290.0, 27.0)], lane, tab, &[(282.0, 306.0)]),
            vec![Some(252.0)]
        );
        assert_eq!(
            ruler_label_xs(
                &[(60.0, 27.0), (100.0, 27.0)],
                (58.0, 140.0),
                None,
                &[(92.0, 116.0)]
            ),
            vec![Some(60.0), None]
        );
    }

    #[wasm_bindgen_test]
    fn fifteen_minutes_with_a_range_keeps_every_label() {
        // The 15 MIN case: handles sit on the only ticks (label x = tick +
        // 2, target = tick +- 12). Every label still draws, beside its
        // handle, without overlapping another.
        let ticks = [90.0_f32, 200.0, 310.0];
        let labels: Vec<_> = ticks.iter().map(|t| (t + 2.0, 32.0)).collect();
        let handles = [(90.0 - 12.0, 90.0 + 12.0), (310.0 - 12.0, 310.0 + 12.0)];
        let xs = ruler_label_xs(&labels, (58.0, 380.0), Some((350.0, 380.0)), &handles);
        assert_eq!(xs, vec![Some(105.0), Some(202.0), Some(263.0)]);
        // Two handles on the only two ticks: both labels still draw, each
        // on the clear side nearest its tick.
        let handles = [(78.0, 102.0), (188.0, 212.0)];
        let xs = ruler_label_xs(&labels[..2], (58.0, 380.0), None, &handles);
        assert_eq!(xs, vec![Some(105.0), Some(215.0)]);
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
        let labelled = |ticks: &[RulerTick]| {
            ticks
                .iter()
                .filter(|t| !t.label.is_empty())
                .map(|t| t.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(labelled(&day), vec!["00", "06", "12", "18"]);
        // Every hour has a tick.
        assert_eq!(day.len(), 24);
        assert!(day.iter().all(|t| t.ts.rem_euclid(3600.0) == 0.0));

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
        assert_eq!(labelled(&cdt), vec!["18", "00", "06", "12"]);
        let midnight = cdt.iter().find(|t| t.label == "00").unwrap();
        assert_eq!((midnight.ts + (-5.0 * 3600.0)).rem_euclid(86_400.0), 0.0);
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
    fn a_volume_not_on_the_device_is_a_storm_bar_with_no_cells() {
        let sz = sizes(vec![
            scan(0.0, 300.0, 0.2),
            scan(300.0, 600.0, 0.9),
            scan(600.0, 900.0, 0.1),
        ]);
        let w = MiniWindow {
            start: 0.0,
            end: 1_000.0,
        };
        let v = cut_volumes(&cuts_strip(), &sz, w, 640.0);
        assert_eq!(v.len(), 3, "the next volume's ghost is not drawn");
        assert!(matches!(v[0].look, CutLook::Cells(ref c) if c.len() == 2));
        match v[1].look {
            CutLook::Bar { frac, fetching } => {
                let scale = ViewScale::from_levels([0.2, 0.9, 0.1]);
                assert_eq!(frac, Some(scale.height_frac(0.9)), "as tall as at 3 H");
                assert!(!fetching);
            }
            _ => panic!("a volume not on the device is a bar"),
        }
        // Unknown size: a quiet wash.
        let v = cut_volumes(&cuts_strip(), &ScanSizes::default(), w, 640.0);
        assert!(matches!(
            v[1].look,
            CutLook::Bar {
                frac: None,
                fetching: false
            }
        ));
        // Downloading: still a bar, marked as on its way.
        let mut cs = cuts_strip();
        cs[1] = shadow(300.0, 600.0, FrameCellState::InFlight);
        let v = cut_volumes(&cs, &ScanSizes::default(), w, 640.0);
        assert!(matches!(v[1].look, CutLook::Bar { fetching: true, .. }));
    }

    #[wasm_bindgen_test]
    fn the_live_volume_shows_one_ghost_and_stops_at_now() {
        let w = MiniWindow {
            start: 0.0,
            end: 1_000.0,
        };
        let v = cut_volumes(&cuts_strip(), &ScanSizes::default(), w, 640.0);
        let live = &v[2];
        assert_eq!(live.end, 640.0, "the box stops at now");
        let CutLook::Cells(cells) = &live.look else {
            panic!("the live volume draws cells");
        };
        let states: Vec<_> = cells.iter().map(|c| (c.start_secs, c.state)).collect();
        assert_eq!(
            states,
            vec![
                (600.0, FrameCellState::Cached),
                (700.0, FrameCellState::Projected)
            ],
            "only the next cut is a ghost"
        );
        // Its cuts all in: the next volume's first cut is the ghost.
        let mut cs = cuts_strip();
        cs[2].cells.truncate(1);
        let v = cut_volumes(&cs, &ScanSizes::default(), w, 640.0);
        let CutLook::Cells(cells) = &v[2].look else {
            panic!()
        };
        assert_eq!(cells.last().unwrap().start_secs, 900.0);
        assert_eq!(cells.len(), 2);
    }

    #[wasm_bindgen_test]
    fn listed_volumes_with_no_container_are_bars_too() {
        let sz = sizes(vec![
            scan(-300.0, 0.0, 0.5),
            scan(0.0, 300.0, 0.5),
            scan(2_000.0, 2_300.0, 0.5),
        ]);
        let w = MiniWindow {
            start: -400.0,
            end: 1_000.0,
        };
        let v = cut_volumes(&cuts_strip(), &sz, w, 640.0);
        assert_eq!(v[0].start, -300.0);
        assert!(matches!(v[0].look, CutLook::Bar { frac: Some(_), .. }));
        assert_eq!(
            v.len(),
            4,
            "covered and out-of-window volumes are not added"
        );
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
    fn once_a_volume_arrives_its_cuts_appear_around_where_the_playhead_landed() {
        let mut cs = cuts_strip();
        let before = StripUnits::from_containers(&cs);
        let landed = snap_scrub(StripZoom::Cuts, 450.0, &before, 10_000.0);
        assert_eq!(landed, 310.0);
        // The volume downloads: its cuts are there now, and the first one
        // holds the playhead where it already is.
        cs[1] = container(
            300.0,
            600.0,
            vec![
                cell(300.0, 330.0, FrameCellState::Cached),
                cell(450.0, 480.0, FrameCellState::Cached),
            ],
        );
        let after = StripUnits::from_containers(&cs);
        assert_eq!(
            pending_mark(StripZoom::Cuts, landed, false, None, &after),
            Some(315.0),
            "the landing cut, until its frame is on screen"
        );
        assert_eq!(
            pending_mark(StripZoom::Cuts, landed, false, Some(300.0), &after),
            None
        );
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
    fn the_strip_marks_where_a_pending_frame_will_land() {
        let u = StripUnits::from_containers(&cuts_strip());
        // Scrubbed into the volume not on the device, the frame on screen
        // still the held volume's: a mark at the playhead.
        assert_eq!(
            pending_mark(StripZoom::Cuts, 300.0, false, Some(0.0), &u),
            Some(300.0)
        );
        assert_eq!(
            pending_mark(StripZoom::Volumes, 300.0, false, Some(0.0), &u),
            Some(450.0)
        );
        assert_eq!(
            pending_mark(StripZoom::Day, 300.0, false, None, &u),
            Some(900.0)
        );
        // Pinned, or the frame already there: no mark.
        assert_eq!(pending_mark(StripZoom::Cuts, 300.0, true, None, &u), None);
        assert_eq!(
            pending_mark(StripZoom::Cuts, 180.0, false, Some(0.0), &u),
            None
        );
        // Nothing holds the playhead: nothing to mark.
        assert_eq!(
            pending_mark(StripZoom::Cuts, 5_000.0, false, None, &u),
            None
        );
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
