//! The selection strip: a thin row above the frames where ranges are made
//! (round-4 design). Dragging on it draws a range; dragging either handle
//! resizes it; dropping the right handle on the live edge pins the range to
//! now. The frames below only move the playhead, so a selection is never a
//! side effect of a modifier key on the scrub surface.
//!
//! Pure decisions only: which part of the strip a press grabs, where a drag
//! point lands, and what a release selects. The egui shell
//! (`ui::timeline::selection_strip`) collects pointer facts, previews the drag
//! through `PlaybackState`'s drag state, and pushes the intent a release maps
//! to.

use crate::core::Intent;

/// How close (px) a press must land to a range edge to grab its handle.
pub(crate) const HANDLE_GRAB_PX: f32 = 8.0;

/// Within this many px of the live edge, a dragged point snaps to now, so the
/// right handle can be dropped onto the live edge without pixel-hunting.
pub(crate) const SNAP_TO_NOW_PX: f32 = 14.0;

/// A released range pins to now only when its right edge sits on the live
/// edge (the drag snaps it there within [`SNAP_TO_NOW_PX`]); this absorbs the
/// clock moving on between the drag's last frame and the release.
const PINNED_END_SLACK_SECS: f64 = 2.0;

/// What a press on the strip takes hold of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StripGrab {
    /// The range's left handle.
    Start,
    /// The range's right handle.
    End,
    /// Empty strip: draw a new range from the press.
    New,
}

/// Which part of the strip a press at `x` grabs. `edges_px` is the current
/// range's `(start_x, end_x)` on screen, if there is one. The nearer handle
/// wins when both are within reach (a range only a few px wide).
pub(crate) fn grab_at(edges_px: Option<(f32, f32)>, x: f32) -> StripGrab {
    let Some((start_x, end_x)) = edges_px else {
        return StripGrab::New;
    };
    let (ds, de) = ((x - start_x).abs(), (x - end_x).abs());
    match (ds <= HANDLE_GRAB_PX, de <= HANDLE_GRAB_PX) {
        (true, true) if ds < de => StripGrab::Start,
        (_, true) => StripGrab::End,
        (true, false) => StripGrab::Start,
        _ => StripGrab::New,
    }
}

/// The edge that stays put while dragging: the opposite handle, or the press
/// itself for a new range. `None` when a handle was grabbed with no range.
pub(crate) fn drag_anchor(
    grab: StripGrab,
    range: Option<(f64, f64)>,
    press_ts: f64,
) -> Option<f64> {
    match grab {
        StripGrab::Start => range.map(|(_, end)| end),
        StripGrab::End => range.map(|(start, _)| start),
        StripGrab::New => Some(press_ts),
    }
}

/// Where the moving edge lands for a pointer at `ts`, `px_from_now` px from
/// the live edge. Never past now (there is nothing to select there); onto now
/// when a stream is running and the pointer is close to the live edge.
pub(crate) fn drag_point(ts: f64, now: f64, px_from_now: f32, stream_active: bool) -> f64 {
    if stream_active && px_from_now.abs() <= SNAP_TO_NOW_PX {
        now
    } else {
        ts.min(now)
    }
}

/// What a release on the strip does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum StripRelease {
    /// Nothing changes (a handle pressed and let go without moving).
    Nothing,
    /// Drop the range: a click on empty strip, or a range dragged shut.
    Clear,
    /// The range spans longer than a selection may (it would queue that many
    /// downloads).
    TooLong,
    /// Select this range; `to_live` pins it to now.
    Select { start: f64, end: f64, to_live: bool },
}

/// Decide a release. `drawn` is the dragged range (`None` when it collapsed
/// to under a second); `moved` is whether the pointer moved since the press.
pub(crate) fn release(
    grab: StripGrab,
    drawn: Option<(f64, f64)>,
    moved: bool,
    now: f64,
    stream_active: bool,
) -> StripRelease {
    if grab != StripGrab::New && !moved {
        return StripRelease::Nothing;
    }
    let Some((start, end)) = drawn else {
        return StripRelease::Clear;
    };
    if !crate::core::selection_span_allowed(start, end) {
        return StripRelease::TooLong;
    }
    StripRelease::Select {
        start,
        end,
        to_live: stream_active && end >= now - PINNED_END_SLACK_SECS,
    }
}

/// The intent a release emits, if any. Clearing with no range selected is a
/// no-op, so it emits nothing (a click on an empty strip while live must not
/// detach anything).
pub(crate) fn intent_for(release: StripRelease, has_range: bool) -> Option<Intent> {
    match release {
        StripRelease::Nothing | StripRelease::TooLong => None,
        StripRelease::Clear => has_range.then_some(Intent::ClearLoop),
        StripRelease::Select {
            start,
            end,
            to_live,
        } => Some(Intent::SelectRange {
            start,
            end,
            to_live,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn press_grabs_the_nearest_handle_or_starts_a_range() {
        let edges = Some((100.0, 300.0));
        assert_eq!(grab_at(edges, 104.0), StripGrab::Start);
        assert_eq!(grab_at(edges, 296.0), StripGrab::End);
        assert_eq!(grab_at(edges, 200.0), StripGrab::New);
        assert_eq!(grab_at(edges, 100.0 - HANDLE_GRAB_PX - 1.0), StripGrab::New);
        assert_eq!(grab_at(None, 100.0), StripGrab::New);
        // A range a few px wide: the nearer handle wins.
        let narrow = Some((100.0, 106.0));
        assert_eq!(grab_at(narrow, 101.0), StripGrab::Start);
        assert_eq!(grab_at(narrow, 105.0), StripGrab::End);
    }

    #[wasm_bindgen_test]
    fn dragging_a_handle_anchors_the_other_edge() {
        let range = Some((1000.0, 2000.0));
        assert_eq!(drag_anchor(StripGrab::Start, range, 1005.0), Some(2000.0));
        assert_eq!(drag_anchor(StripGrab::End, range, 1995.0), Some(1000.0));
        assert_eq!(drag_anchor(StripGrab::New, range, 1500.0), Some(1500.0));
        assert_eq!(drag_anchor(StripGrab::End, None, 1500.0), None);
    }

    #[wasm_bindgen_test]
    fn drag_point_stops_at_now_and_snaps_onto_it_while_streaming() {
        let now = 5000.0;
        assert_eq!(drag_point(4000.0, now, -200.0, true), 4000.0);
        // Past now clamps to now.
        assert_eq!(drag_point(5600.0, now, 60.0, false), now);
        // Near the live edge snaps while streaming, not otherwise.
        assert_eq!(drag_point(4990.0, now, -SNAP_TO_NOW_PX, true), now);
        assert_eq!(drag_point(4990.0, now, -SNAP_TO_NOW_PX, false), 4990.0);
    }

    #[wasm_bindgen_test]
    fn release_selects_pins_or_clears() {
        let now = 10_000.0;
        // A range ending at the live edge while streaming is pinned to now.
        assert_eq!(
            release(StripGrab::End, Some((9000.0, now)), true, now, true),
            StripRelease::Select {
                start: 9000.0,
                end: now,
                to_live: true
            }
        );
        // The same range with no stream is fixed.
        assert_eq!(
            release(StripGrab::New, Some((9000.0, now)), true, now, false),
            StripRelease::Select {
                start: 9000.0,
                end: now,
                to_live: false
            }
        );
        // A minute and a half short of now is a fixed range: only a drop on
        // the live edge pins.
        assert_eq!(
            release(StripGrab::New, Some((9000.0, now - 90.0)), true, now, true),
            StripRelease::Select {
                start: 9000.0,
                end: now - 90.0,
                to_live: false
            }
        );
        // Dragged well off now: fixed.
        assert_eq!(
            release(StripGrab::End, Some((5000.0, 8000.0)), true, now, true),
            StripRelease::Select {
                start: 5000.0,
                end: 8000.0,
                to_live: false
            }
        );
        // A click on empty strip (nothing drawn) clears.
        assert_eq!(
            release(StripGrab::New, None, false, now, true),
            StripRelease::Clear
        );
        // A handle pressed and released in place changes nothing.
        assert_eq!(
            release(StripGrab::Start, Some((9000.0, now)), false, now, true),
            StripRelease::Nothing
        );
        // Longer than a selection may span.
        let too_long = crate::core::MAX_SELECTION_SPAN_SECS + 3600.0;
        assert_eq!(
            release(
                StripGrab::New,
                Some((now - too_long, now)),
                true,
                now,
                false
            ),
            StripRelease::TooLong
        );
    }

    #[wasm_bindgen_test]
    fn releases_map_to_intents() {
        assert_eq!(
            intent_for(
                StripRelease::Select {
                    start: 1.0,
                    end: 9.0,
                    to_live: true
                },
                false
            ),
            Some(Intent::SelectRange {
                start: 1.0,
                end: 9.0,
                to_live: true
            })
        );
        assert_eq!(
            intent_for(StripRelease::Clear, true),
            Some(Intent::ClearLoop)
        );
        // Clearing with no range is a no-op (a click on the strip while live
        // never detaches).
        assert_eq!(intent_for(StripRelease::Clear, false), None);
        assert_eq!(intent_for(StripRelease::Nothing, true), None);
        assert_eq!(intent_for(StripRelease::TooLong, true), None);
    }
}
