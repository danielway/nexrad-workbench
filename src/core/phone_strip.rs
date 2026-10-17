//! The phone strip's zoom stops (phone timeline §1): 15 MIN, 3 H and DAY.
//!
//! Continuous zoom suits a wheel, not a thumb, so the phone's mini timeline
//! has three stops, each with its own window rule and scrub unit:
//!
//! - **Cuts** (pill "15 MIN") — today's strip
//!   ([`crate::core::phone::mini_window`]): the last three volumes and the
//!   ghost of the next. A scrub snaps to cuts.
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
//! screen ([`ring_center`]), the white playhead line when detached, the now
//! line, and a small tab under it ([`strip_cap`]): filled red only for LIVE,
//! outlined for REJOIN and GO LIVE, and an outlined edge tab at the right
//! (`GO LIVE ›`, `REJOIN ›`) when now is off the window. Ruler labels give
//! way to the tab ([`ruler_labels_shown`]).
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

use crate::core::phone::{mini_lead_secs, mini_window, MiniWindow, PressOut, PressTrack};
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

/// The strip's window at a stop, in Unix seconds. `volume_starts` are the
/// containers' starts, oldest first; `prev` is the window the strip drew
/// last frame.
///
/// Cuts is [`mini_window`]. At Volumes and Day, pinned, the window ends at
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
pub(crate) fn strip_window(
    stop: StripZoom,
    now: f64,
    playhead: f64,
    pinned: bool,
    volume_starts: &[f64],
    prev: Option<MiniWindow>,
) -> MiniWindow {
    let span = match stop {
        StripZoom::Cuts => return mini_window(now, playhead, pinned, volume_starts),
        StripZoom::Volumes => VOLUMES_SPAN_SECS,
        StripZoom::Day => DAY_SPAN_SECS,
    };
    let limit = now + mini_lead_secs(volume_starts);
    let end = if pinned {
        limit
    } else {
        match prev
            .filter(|p| (p.span() - span).abs() < 1.0 && playhead >= p.start && playhead <= p.end)
        {
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

/// The storm-activity listing the phone strip asks for: at the zoomed-out
/// stops, the window in view and the lane's width, which decide (through
/// [`crate::core::storm_activity::needs_scans`]) that its days are listed
/// for their volumes' sizes, as the desktop overview does zoomed in (days
/// the nightly index covers have no per-volume sizes). `None` at Cuts,
/// which draws the strip's own cells.
pub(crate) fn activity_window(
    stop: StripZoom,
    window: MiniWindow,
    lane_px: f32,
) -> Option<(f64, f64, f64)> {
    (stop.per_volume() && lane_px > 0.0).then_some((window.start, window.end, lane_px as f64))
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
    /// (the zoomed-out stops' bars reach volumes the strip has no container for).
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

/// Which ruler labels draw. `labels` are each label's left x and width;
/// a label draws when it fits inside `lane` (left, right), clears the now
/// tab's span `cap` (left, right) and the label before it that drew.
pub(crate) fn ruler_labels_shown(
    labels: &[(f32, f32)],
    lane: (f32, f32),
    cap: Option<(f32, f32)>,
) -> Vec<bool> {
    const GAP: f32 = 3.0;
    let mut last_right = f32::NEG_INFINITY;
    labels
        .iter()
        .map(|&(x, w)| {
            let fits = x >= lane.0 && x + w <= lane.1;
            let clear_of_cap = cap.is_none_or(|(l, r)| x + w + GAP <= l || x >= r + GAP);
            let show = fits && clear_of_cap && x >= last_right + GAP;
            if show {
                last_right = x + w;
            }
            show
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
                strip_window(StripZoom::Cuts, NOW, playhead, pinned, &s, None),
                mini_window(NOW, playhead, pinned, &s)
            );
        }
    }

    #[wasm_bindgen_test]
    fn far_back_volumes_and_day_center_on_the_playhead() {
        let s = starts(NOW - 200_000.0, NOW);
        let playhead = NOW - 150_000.0;
        let v = strip_window(StripZoom::Volumes, NOW, playhead, false, &s, None);
        assert_eq!(v.span(), VOLUMES_SPAN_SECS);
        assert_eq!((v.start + v.end) / 2.0, playhead);
        let d = strip_window(StripZoom::Day, NOW, playhead, false, &s, None);
        assert_eq!(d.span(), DAY_SPAN_SECS);
        assert_eq!((d.start + d.end) / 2.0, playhead);
    }

    #[wasm_bindgen_test]
    fn with_no_last_window_detached_centres_on_the_playhead() {
        let s = starts(NOW - 200_000.0, NOW);
        let a = strip_window(StripZoom::Volumes, NOW, NOW - 50_000.0, false, &s, None);
        let b = strip_window(StripZoom::Volumes, NOW, NOW - 49_000.0, false, &s, None);
        assert_eq!(b.start - a.start, 1_000.0);
        assert_eq!(b.end - a.end, 1_000.0);
    }

    #[wasm_bindgen_test]
    fn pinned_the_window_ends_at_cuts_ghost_room() {
        let s = starts(NOW - 100_000.0, NOW);
        let cuts = strip_window(StripZoom::Cuts, NOW, NOW, true, &s, None);
        for stop in [StripZoom::Volumes, StripZoom::Day] {
            let w = strip_window(stop, NOW, NOW, true, &s, None);
            assert_eq!(w.end, cuts.end, "{stop:?} ends where Cuts does");
            assert!((w.end - (NOW + lead())).abs() < 1e-9);
            assert!(w.start < NOW && NOW < w.end, "now sits inside");
        }
        // A playhead that lags now a little while pinned changes nothing.
        let w = strip_window(StripZoom::Day, NOW, NOW - 3.0, true, &s, None);
        assert_eq!(w.end, NOW + lead());
    }

    #[wasm_bindgen_test]
    fn detached_near_now_clamps_so_nothing_past_the_ghost_room_shows() {
        let s = starts(NOW - 100_000.0, NOW);
        // Half an hour back: centered would reach an hour past now.
        let w = strip_window(StripZoom::Volumes, NOW, NOW - 1_800.0, false, &s, None);
        assert_eq!(w.end, NOW + lead());
        assert_eq!(w.span(), VOLUMES_SPAN_SECS);
        assert!(w.start < NOW - 1_800.0, "the playhead stays in view");
        // Two hours back is far enough to center.
        let w = strip_window(StripZoom::Volumes, NOW, NOW - 7_200.0, false, &s, None);
        assert_eq!((w.start + w.end) / 2.0, NOW - 7_200.0);
        assert!(w.end < NOW);
        // At Day, six hours back still clamps.
        let w = strip_window(StripZoom::Day, NOW, NOW - 6.0 * 3600.0, false, &s, None);
        assert_eq!(w.end, NOW + lead());
    }

    #[wasm_bindgen_test]
    fn the_ghost_room_follows_the_volume_length() {
        // VCP 35-ish: 10-minute volumes leave twice the room.
        let slow: Vec<f64> = (0..20).map(|i| NOW - 12_000.0 + i as f64 * 600.0).collect();
        let w = strip_window(StripZoom::Day, NOW, NOW, true, &slow, None);
        assert!((w.end - (NOW + MINI_VOLUMES * 600.0 * MINI_LEAD_SHARE)).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn landscape_keeps_the_spans_and_gets_more_pixels_per_volume() {
        // The window has no width in it; a wider lane is more px per unit.
        let s = starts(NOW - 100_000.0, NOW);
        let w = strip_window(StripZoom::Day, NOW, NOW, true, &s, None);
        let px_per_volume = |lane_px: f64| lane_px / w.span() * DEFAULT_VOLUME_SECS;
        let portrait = px_per_volume(330.0);
        let landscape = px_per_volume(780.0);
        assert!(portrait > 1.0 && portrait < 2.0, "{portrait}");
        assert!(landscape > 2.0 * portrait);
        let v = strip_window(StripZoom::Volumes, NOW, NOW, true, &s, None);
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
        // The last one would cross the lane's end; the one under the cap
        // gives way.
        assert_eq!(
            ruler_labels_shown(&labels, (58.0, 380.0), Some((320.0, 360.0))),
            vec![true, true, true, false, false]
        );
        assert_eq!(
            ruler_labels_shown(&labels, (58.0, 380.0), None),
            vec![true, true, true, true, false]
        );
        // Two labels too close: the later one gives way.
        assert_eq!(
            ruler_labels_shown(&[(60.0, 27.0), (80.0, 27.0)], (0.0, 400.0), None),
            vec![true, false]
        );
    }

    #[wasm_bindgen_test]
    fn detached_the_window_holds_while_the_playhead_stays_inside() {
        let s = starts(NOW - 200_000.0, NOW);
        let first = strip_window(StripZoom::Volumes, NOW, NOW - 50_000.0, false, &s, None);
        // Scrubbing a little either way leaves the window where it is.
        for ph in [NOW - 49_000.0, NOW - 51_000.0] {
            assert_eq!(
                strip_window(StripZoom::Volumes, NOW, ph, false, &s, Some(first)),
                first
            );
        }
        // Into the right margin: the window slides to keep the margin.
        let margin = VOLUMES_SPAN_SECS * EDGE_SHARE;
        let ph = first.end - margin / 2.0;
        let w = strip_window(StripZoom::Volumes, NOW, ph, false, &s, Some(first));
        assert!((w.end - (ph + margin)).abs() < 1e-6);
        assert_eq!(w.span(), VOLUMES_SPAN_SECS);
        // Into the left margin, likewise.
        let ph = first.start + margin / 2.0;
        let w = strip_window(StripZoom::Volumes, NOW, ph, false, &s, Some(first));
        assert!((w.start - (ph - margin)).abs() < 1e-6);
        // Off the window (a typed time): centred on it.
        let ph = NOW - 150_000.0;
        let w = strip_window(StripZoom::Volumes, NOW, ph, false, &s, Some(first));
        assert_eq!((w.start + w.end) / 2.0, ph);
        // A window of another stop's span is not held.
        let w = strip_window(StripZoom::Day, NOW, NOW - 50_000.0, false, &s, Some(first));
        assert_eq!((w.start + w.end) / 2.0, NOW - 50_000.0);
    }

    #[wasm_bindgen_test]
    fn tapping_back_from_live_keeps_the_window_and_pinned_slides_with_now() {
        let s = starts(NOW - 100_000.0, NOW);
        let pinned = strip_window(StripZoom::Volumes, NOW, NOW, true, &s, None);
        assert!((pinned.end - (NOW + lead())).abs() < 1e-9);
        // A tap on a volume an hour back detaches; the strip does not jump.
        let w = strip_window(
            StripZoom::Volumes,
            NOW,
            NOW - 3_600.0,
            false,
            &s,
            Some(pinned),
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
        );
        assert!((later.end - pinned.end - 300.0).abs() < 1e-9);
        // Detached, it never reaches past now's ghost room.
        let w = strip_window(StripZoom::Day, NOW, NOW - 60.0, false, &s, Some(pinned));
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
}
