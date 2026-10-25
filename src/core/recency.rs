//! One recency rule (one-instrument §7 "One recency threshold").
//!
//! The canvas used to carry four recency thresholds — 60 s and 300 s age
//! colors, 900 s for live overlays and the display lookback, 3600 s for an
//! "ARCHIVE DATA" banner — and they disagreed between 15 and 60 minutes
//! behind. One threshold replaces them all: [`RECENCY_SECS`].
//!
//! It answers two questions, both "is this close enough in time to still
//! mean the same moment":
//!
//! - **Display lookback** — a cached scan may stand for the playhead when it
//!   started at most [`RECENCY_SECS`] before it ([`is_recent`]).
//! - **Live overlays** — warnings and the national mosaic show *current*
//!   conditions, so they draw only while what is shown is within
//!   [`RECENCY_SECS`] of now ([`live_overlays_off`]):
//!   - **Paused, or a point selection** (live or not): per playhead — they
//!     apply while the playhead is within [`RECENCY_SECS`] of now.
//!   - **A range selection while it plays**: decided for the whole loop,
//!     not per playhead, so they cannot pop on and off as the playhead
//!     crosses the edge mid-loop ([`PlayingLoop`]). They apply when the
//!     range is pinned to now (a live range is a live view, also before its
//!     bounds resolve) or when the whole range lies within the window (its
//!     start is within [`RECENCY_SECS`] of now); otherwise they stay off for
//!     the whole loop ([`LiveOverlaysOff::LoopReachesBack`] when the loop
//!     crosses the edge, [`LiveOverlaysOff::Behind`] when all of it is
//!     older). The answer does not depend on the playhead, so a fixed loop
//!     that starts just inside the window turns them off once, when its
//!     start ages out, and not again on every cycle.
//!
//! Frame age is never a color; it is a number in the age pill
//! ([`crate::core::session::format_age`]).

use crate::core::Selection;

/// The one recency threshold: 15 minutes (a full VCP cycle with margin).
pub(crate) const RECENCY_SECS: f64 = 15.0 * 60.0;

/// Whether `ts_secs` is recent enough to stand for `reference_secs`: at most
/// [`RECENCY_SECS`] before it. A time after the reference is recent.
pub(crate) fn is_recent(reference_secs: f64, ts_secs: f64) -> bool {
    reference_secs - ts_secs <= RECENCY_SECS
}

/// A range selection while it plays, as the live-overlay rule sees it: the
/// loop is judged as a whole instead of at each playhead.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PlayingLoop {
    /// Pinned to now (a live range, its bounds resolved or not): a live view.
    Pinned,
    /// A fixed loop over `[start_secs, end_secs]`.
    Fixed { start_secs: f64, end_secs: f64 },
}

impl PlayingLoop {
    /// The loop the live-overlay rule judges as a whole: a range selection
    /// that is playing. `None` while paused and for a point selection, which
    /// keep the per-playhead rule — and for a fixed range without bounds,
    /// which [`crate::core::domain::playback::RangeSelection`] rules out.
    pub(crate) fn of(playing: bool, selection: &Selection) -> Option<Self> {
        if !playing {
            return None;
        }
        let range = selection.range()?;
        if range.live {
            return Some(Self::Pinned);
        }
        range.bounds.map(|(start_secs, end_secs)| Self::Fixed {
            start_secs,
            end_secs,
        })
    }
}

/// Why live-only overlays are off (what the Layers note says).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LiveOverlaysOff {
    /// What is shown is more than [`RECENCY_SECS`] behind now: the playhead,
    /// or all of a playing fixed loop.
    Behind,
    /// A playing fixed loop crosses the edge: it ends within
    /// [`RECENCY_SECS`] of now but starts further back, so the overlays stay
    /// off for the whole loop, even while the playhead is recent.
    LoopReachesBack,
}

/// Whether live-only overlays (warnings, the national mosaic) are off, and
/// why; `None` when they apply. See the module docs for the rule. A time
/// ahead of now (a projected region) is recent.
pub(crate) fn live_overlays_off(
    now_secs: f64,
    playhead_secs: f64,
    playing_loop: Option<PlayingLoop>,
) -> Option<LiveOverlaysOff> {
    match playing_loop {
        Some(PlayingLoop::Pinned) => None,
        Some(PlayingLoop::Fixed {
            start_secs,
            end_secs,
        }) => {
            if is_recent(now_secs, start_secs) {
                None
            } else if is_recent(now_secs, end_secs) {
                Some(LiveOverlaysOff::LoopReachesBack)
            } else {
                Some(LiveOverlaysOff::Behind)
            }
        }
        None => (!is_recent(now_secs, playhead_secs)).then_some(LiveOverlaysOff::Behind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::playback::RangeSelection;
    use crate::core::LoopBasis;
    use wasm_bindgen_test::wasm_bindgen_test;

    const MIN: f64 = 60.0;
    /// 11:20, a minute after the end of the reported 11:00 -> 11:19 loop.
    const NOW: f64 = 11.0 * 3_600.0 + 20.0 * MIN;

    fn at(hh: f64, mm: f64) -> f64 {
        hh * 3_600.0 + mm * MIN
    }

    fn range(bounds: Option<(f64, f64)>, live: bool) -> Selection {
        let span = bounds.map_or(30.0 * MIN, |(s, e)| e - s);
        Selection::Range(RangeSelection {
            bounds,
            live,
            basis: LoopBasis::Duration(span),
        })
    }

    /// Whether the live-only overlays apply (what `Derived::data_is_live`
    /// answers).
    fn live_overlays_apply(now: f64, playhead: f64, playing_loop: Option<PlayingLoop>) -> bool {
        live_overlays_off(now, playhead, playing_loop).is_none()
    }

    /// The rule as `Derived::data_is_live` reads it.
    fn applies(playing: bool, selection: &Selection, playhead: f64) -> bool {
        off(playing, selection, playhead).is_none()
    }

    /// Why the overlays are off, read as [`applies`] reads it.
    fn off(playing: bool, selection: &Selection, playhead: f64) -> Option<LiveOverlaysOff> {
        live_overlays_off(NOW, playhead, PlayingLoop::of(playing, selection))
    }

    /// Playheads every 30 s across `[start, end]`, both ends included.
    fn playheads(start: f64, end: f64) -> impl Iterator<Item = f64> {
        let steps = ((end - start) / 30.0).ceil() as usize;
        (0..=steps).map(move |i| (start + i as f64 * 30.0).min(end))
    }

    #[wasm_bindgen_test]
    fn the_threshold_is_fifteen_minutes() {
        assert_eq!(RECENCY_SECS, 900.0);
    }

    #[wasm_bindgen_test]
    fn recent_is_inclusive_at_the_threshold() {
        assert!(is_recent(1_000.0, 1_000.0));
        assert!(is_recent(1_000.0, 1_000.0 - RECENCY_SECS));
        assert!(!is_recent(1_000.0, 1_000.0 - RECENCY_SECS - 0.001));
    }

    #[wasm_bindgen_test]
    fn a_time_after_the_reference_is_recent() {
        assert!(is_recent(1_000.0, 5_000.0));
    }

    #[wasm_bindgen_test]
    fn live_overlays_follow_the_same_threshold() {
        let now = 10_000.0;
        assert!(live_overlays_apply(now, now, None));
        assert!(live_overlays_apply(now, now - 14.0 * MIN, None));
        assert!(live_overlays_apply(now, now - RECENCY_SECS, None));
        assert!(!live_overlays_apply(now, now - 16.0 * MIN, None));
        // A year behind: archive, no live overlays.
        assert!(!live_overlays_apply(now, now - 365.0 * 86_400.0, None));
        // A playing fixed loop uses the same threshold, on its start.
        let fixed = |start_secs| {
            Some(PlayingLoop::Fixed {
                start_secs,
                end_secs: now,
            })
        };
        assert!(live_overlays_apply(now, now, fixed(now - RECENCY_SECS)));
        assert!(!live_overlays_apply(
            now,
            now,
            fixed(now - RECENCY_SECS - 0.001)
        ));
    }

    #[wasm_bindgen_test]
    fn a_fixed_loop_inside_the_window_applies_at_every_playhead() {
        let (start, end) = (at(11.0, 6.0), at(11.0, 19.0));
        let sel = range(Some((start, end)), false);
        for p in playheads(start, end) {
            assert!(applies(true, &sel, p), "playhead {p}");
        }
        // Starting exactly at the edge still counts as inside.
        let sel = range(Some((NOW - RECENCY_SECS, end)), false);
        for p in playheads(NOW - RECENCY_SECS, end) {
            assert!(applies(true, &sel, p), "playhead {p}");
        }
    }

    #[wasm_bindgen_test]
    fn a_fixed_loop_across_the_edge_is_off_for_the_whole_loop_while_playing() {
        // The report: a fixed 11:00 -> 11:19 loop at 11:20 drew the mosaic
        // only once the playhead passed 11:05.
        let (start, end) = (at(11.0, 0.0), at(11.0, 19.0));
        let sel = range(Some((start, end)), false);
        for p in playheads(start, end) {
            assert!(!applies(true, &sel, p), "playhead {p}");
            assert_eq!(
                off(true, &sel, p),
                Some(LiveOverlaysOff::LoopReachesBack),
                "playhead {p}"
            );
        }
        // Paused on the same range, the playhead decides again.
        assert!(applies(false, &sel, end));
        assert!(!applies(false, &sel, start));
    }

    #[wasm_bindgen_test]
    fn a_pinned_loop_applies_throughout() {
        // A live "last hour" loop reaches well past the window but is a live
        // view: on at every playhead while it plays.
        let (start, end) = (NOW - 60.0 * MIN, NOW);
        let sel = range(Some((start, end)), true);
        for p in playheads(start, end) {
            assert!(applies(true, &sel, p), "playhead {p}");
        }
    }

    #[wasm_bindgen_test]
    fn an_unresolved_live_range_counts_as_pinned() {
        let sel = range(None, true);
        assert_eq!(PlayingLoop::of(true, &sel), Some(PlayingLoop::Pinned));
        // Its seeded playhead (the oldest cached frame) may be far back.
        assert!(applies(true, &sel, NOW - 60.0 * MIN));
        assert!(applies(true, &sel, NOW));
    }

    #[wasm_bindgen_test]
    fn a_paused_range_keeps_the_per_playhead_rule() {
        for live in [false, true] {
            let sel = range(Some((NOW - 60.0 * MIN, NOW)), live);
            assert_eq!(PlayingLoop::of(false, &sel), None);
            assert!(applies(false, &sel, NOW - 10.0 * MIN));
            assert!(!applies(false, &sel, NOW - 20.0 * MIN));
            assert_eq!(
                off(false, &sel, NOW - 20.0 * MIN),
                Some(LiveOverlaysOff::Behind)
            );
        }
    }

    #[wasm_bindgen_test]
    fn point_selections_keep_the_per_playhead_rule() {
        for live in [false, true] {
            let sel = Selection::Point { live };
            for playing in [false, true] {
                assert_eq!(PlayingLoop::of(playing, &sel), None);
                assert!(applies(playing, &sel, NOW));
                assert!(applies(playing, &sel, NOW - RECENCY_SECS));
                assert!(!applies(playing, &sel, NOW - 16.0 * MIN));
            }
        }
    }

    #[wasm_bindgen_test]
    fn a_loop_wholly_older_than_the_window_is_behind_playing_or_paused() {
        // Ends an hour before now: nothing in it is recent, so the note says
        // the same thing whether it plays or not.
        let (start, end) = (at(10.0, 0.0), at(10.0, 20.0));
        let sel = range(Some((start, end)), false);
        for p in playheads(start, end) {
            for playing in [true, false] {
                assert_eq!(
                    off(playing, &sel, p),
                    Some(LiveOverlaysOff::Behind),
                    "playhead {p}, playing {playing}"
                );
            }
        }
        // The loop that crosses the edge still says so while it plays.
        let sel = range(Some((at(11.0, 0.0), at(11.0, 19.0))), false);
        assert_eq!(
            off(true, &sel, at(11.0, 19.0)),
            Some(LiveOverlaysOff::LoopReachesBack)
        );
    }
}
