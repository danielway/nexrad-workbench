//! Pure timeline-strip interaction decisions.
//!
//! The strip shares ONE egui response across seek, scrub, range-selection,
//! inspect and pan, so "which gesture owns the pointer this frame" is a real
//! decision with real precedence — and it was previously spread across a dozen
//! interleaved boolean guards inside the painter, where it could not be tested.
//! This module owns that decision as a pure function ([`resolve_gesture`]) plus
//! the view-position clamp every pan path shares ([`clamp_view_start`]).
//!
//! The egui layer's job shrinks to: collect raw pointer facts into
//! [`StripInput`], ask which gesture won, and perform the corresponding
//! mutation. See `crate::ui::timeline::interaction`.

/// Fraction of the visible span a pan may overscroll past either end of the
/// addressable range. At `0.5` the extremes are "archive start centered" and
/// "now centered", which is the natural stopping point in both directions.
const PAN_OVERSCROLL_FRAC: f64 = 0.5;

/// Start of the NEXRAD Level II archive era: 1991-06-05 00:00:00 UTC.
///
/// The left bound for timeline panning and the reference for the Archive
/// tier's era keyline. Coverage before the mid-1990s is sparse and
/// site-dependent, so this is the *addressable* floor, not a promise that data
/// exists there.
pub(crate) const NEXRAD_ARCHIVE_START_SECS: f64 = 676_080_000.0;

/// Which pointer gesture owns the timeline strip this frame.
///
/// Exactly one wins. Precedence is `Pan > Select > Inspect > Seek`, and the
/// reasons are behavioral, not arbitrary:
/// - **Pan** is modeless navigation; it must never also move the playhead, or
///   the gesture that exists to stop accidental seeks would cause them.
/// - **Select** outranks Seek because a selection already in progress owns the
///   drag it started, regardless of which button is still down.
/// - **Inspect** is a discrete secondary *click*; a secondary *drag* is a
///   selection, so the two are disjoint by construction.
/// - **Seek** is the fallback for a plain primary press/drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StripGesture {
    /// Nothing this frame — or the pointer landed on a control that owns its
    /// own clicks (loop handle, failed-cell tick, now cap).
    None,
    /// Pan the view: middle-drag, or ctrl/cmd + primary drag.
    Pan,
    /// Create or extend the loop/selection range.
    Select,
    /// Open the scan inspector for the scan under the pointer.
    Inspect,
    /// Move the playhead (press-seek or drag-scrub).
    Seek,
}

/// Raw per-frame pointer facts the strip collects from egui, with no egui types
/// so the decision stays testable.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StripInput {
    /// Primary button went down this frame, over the strip.
    pub primary_pressed: bool,
    /// A primary-button drag began this frame.
    pub primary_drag_started: bool,
    /// A primary-button drag is ongoing this frame.
    pub primary_dragging: bool,
    /// The secondary button is down or its drag just began.
    pub secondary_down: bool,
    /// A clean secondary click (a press that did not become a drag).
    pub secondary_clicked: bool,
    /// A middle-button drag is ongoing this frame.
    pub middle_dragging: bool,
    /// A primary click completed this frame.
    pub clicked: bool,
    /// Shift or alt is held — the range-selection modifier.
    pub selection_mod: bool,
    /// Ctrl or cmd is held — the pan modifier.
    pub pan_mod: bool,
    /// A selection drag is already in progress and owns the pointer.
    pub selection_in_progress: bool,
    /// The press landed inside a rect belonging to another control.
    pub on_suppressed_rect: bool,
    /// This drag *began* inside a suppressed rect, so the other control owns
    /// it for the drag's whole lifetime.
    pub scrub_suppressed: bool,
    /// The Archive tier is a navigator only — no playhead movement (the
    /// calendar's day cells own clicks there, via tap-to-zoom).
    pub archive_tier: bool,
}

/// Decide which gesture owns the strip this frame. Pure; see [`StripGesture`]
/// for the precedence rationale.
pub(crate) fn resolve_gesture(input: &StripInput) -> StripGesture {
    // Pan first, unconditionally. A middle-drag or ctrl-drag is navigation and
    // must not fall through to a seek even if the primary button is also down.
    if input.middle_dragging || (input.pan_mod && (input.primary_dragging || input.primary_pressed))
    {
        return StripGesture::Pan;
    }

    // A selection already in progress owns the pointer until the drag ends,
    // whichever button started it.
    if input.selection_in_progress {
        return StripGesture::Select;
    }

    // Fresh selection: modifier + primary (click or drag), or a secondary drag.
    if (input.selection_mod && (input.clicked || input.primary_drag_started))
        || input.secondary_down
    {
        return StripGesture::Select;
    }

    // A clean secondary click opens the inspector. Disjoint from Select above:
    // a secondary *drag* sets `secondary_down` and already returned.
    if input.secondary_clicked {
        return StripGesture::Inspect;
    }

    // Seek/scrub last, and never from a control's own hit rect.
    if input.archive_tier {
        return StripGesture::None;
    }
    if input.primary_dragging && !input.scrub_suppressed {
        return StripGesture::Seek;
    }
    if input.primary_pressed && !input.on_suppressed_rect {
        return StripGesture::Seek;
    }

    StripGesture::None
}

/// Clamp a desired timeline view start so the visible window stays anchored to
/// the addressable range `[era_start, now]`, allowing a
/// [`PAN_OVERSCROLL_FRAC`] overscroll past either end.
///
/// Without this, pan writes `timeline_view_start` raw and the view can be
/// scrolled to arbitrary time — a single trackpad flick at the widest zoom
/// moves years. Degenerate spans pass through unchanged, and an inverted range
/// (clock skew putting `now` before the era start) collapses to the low bound
/// rather than panicking on an inverted `clamp`.
pub(crate) fn clamp_view_start(desired: f64, span: f64, era_start: f64, now: f64) -> f64 {
    if !span.is_finite() || span <= 0.0 || !desired.is_finite() {
        return desired;
    }
    let overscroll = span * PAN_OVERSCROLL_FRAC;
    let lo = era_start - overscroll;
    // At the right extreme the window's END reaches `now + overscroll`.
    let hi = now + overscroll - span;
    desired.clamp(lo, hi.max(lo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    // ---- resolve_gesture: pan wins ---------------------------------------

    #[wasm_bindgen_test]
    fn middle_drag_is_pan() {
        let input = StripInput {
            middle_dragging: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Pan);
    }

    #[wasm_bindgen_test]
    fn ctrl_primary_drag_is_pan() {
        let input = StripInput {
            pan_mod: true,
            primary_dragging: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Pan);
    }

    #[wasm_bindgen_test]
    fn ctrl_primary_press_is_pan_not_seek() {
        // The press that STARTS a ctrl-drag must not seek on its first frame —
        // that was the whole point of adding a modeless pan.
        let input = StripInput {
            pan_mod: true,
            primary_pressed: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Pan);
    }

    #[wasm_bindgen_test]
    fn pan_outranks_an_in_progress_selection() {
        let input = StripInput {
            middle_dragging: true,
            selection_in_progress: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Pan);
    }

    // ---- resolve_gesture: selection --------------------------------------

    #[wasm_bindgen_test]
    fn in_progress_selection_owns_the_pointer() {
        // Even a plain primary drag resolves to Select while a selection is
        // live — it started as one and keeps ownership until drag end.
        let input = StripInput {
            selection_in_progress: true,
            primary_dragging: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Select);
    }

    #[wasm_bindgen_test]
    fn shift_click_and_shift_drag_both_select() {
        let click = StripInput {
            selection_mod: true,
            clicked: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&click) == StripGesture::Select);
        let drag = StripInput {
            selection_mod: true,
            primary_drag_started: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&drag) == StripGesture::Select);
    }

    #[wasm_bindgen_test]
    fn secondary_drag_selects_and_secondary_click_inspects() {
        // The two secondary gestures are disjoint: down => drag => Select,
        // clean click => Inspect.
        let drag = StripInput {
            secondary_down: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&drag) == StripGesture::Select);
        let click = StripInput {
            secondary_clicked: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&click) == StripGesture::Inspect);
    }

    #[wasm_bindgen_test]
    fn secondary_drag_beats_secondary_click_when_both_set() {
        // egui can report both on the frame a drag ends; the drag owns it, so
        // an aborted range-drag never falls through to opening the inspector.
        let input = StripInput {
            secondary_down: true,
            secondary_clicked: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Select);
    }

    #[wasm_bindgen_test]
    fn selection_mod_press_never_seeks() {
        let input = StripInput {
            selection_mod: true,
            primary_pressed: true,
            primary_drag_started: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Select);
    }

    // ---- resolve_gesture: seek -------------------------------------------

    #[wasm_bindgen_test]
    fn plain_primary_press_seeks() {
        let input = StripInput {
            primary_pressed: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Seek);
    }

    #[wasm_bindgen_test]
    fn plain_primary_drag_seeks() {
        let input = StripInput {
            primary_dragging: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::Seek);
    }

    #[wasm_bindgen_test]
    fn press_on_a_suppressed_rect_does_not_seek() {
        let input = StripInput {
            primary_pressed: true,
            on_suppressed_rect: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::None);
    }

    #[wasm_bindgen_test]
    fn drag_begun_on_a_suppressed_rect_does_not_scrub() {
        // A loop handle whose 44px hit rect reaches up into the strip owns its
        // drag for the drag's whole lifetime.
        let input = StripInput {
            primary_dragging: true,
            scrub_suppressed: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::None);
    }

    #[wasm_bindgen_test]
    fn archive_tier_is_a_navigator_only() {
        // No playhead movement at Archive; the calendar's day cells own clicks.
        let press = StripInput {
            primary_pressed: true,
            archive_tier: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&press) == StripGesture::None);
        let drag = StripInput {
            primary_dragging: true,
            archive_tier: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&drag) == StripGesture::None);
    }

    #[wasm_bindgen_test]
    fn archive_tier_still_pans_selects_and_inspects() {
        // Only seek is suppressed at Archive — navigation and inspection stay.
        let pan = StripInput {
            middle_dragging: true,
            archive_tier: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&pan) == StripGesture::Pan);
        let inspect = StripInput {
            secondary_clicked: true,
            archive_tier: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&inspect) == StripGesture::Inspect);
    }

    #[wasm_bindgen_test]
    fn idle_frame_is_none() {
        assert!(resolve_gesture(&StripInput::default()) == StripGesture::None);
    }

    // ---- clamp_view_start -------------------------------------------------

    const ERA: f64 = 1_000_000.0;
    const NOW: f64 = 2_000_000.0;

    #[wasm_bindgen_test]
    fn view_inside_the_range_is_untouched() {
        let span = 3600.0;
        assert!(approx(
            clamp_view_start(1_500_000.0, span, ERA, NOW),
            1_500_000.0
        ));
    }

    #[wasm_bindgen_test]
    fn panning_far_left_stops_half_a_span_before_the_era() {
        let span = 3600.0;
        let out = clamp_view_start(-9e12, span, ERA, NOW);
        assert!(approx(out, ERA - span * 0.5));
    }

    #[wasm_bindgen_test]
    fn panning_far_right_stops_with_now_half_a_span_from_the_end() {
        let span = 3600.0;
        let out = clamp_view_start(9e12, span, ERA, NOW);
        // Window end reaches now + half a span.
        assert!(approx(out + span, NOW + span * 0.5));
    }

    #[wasm_bindgen_test]
    fn a_span_wider_than_the_range_keeps_the_whole_range_visible() {
        // 100x the addressable range. There is nowhere meaningful to pan, so
        // the clamp parks the window such that BOTH the era start and now stay
        // inside it — the wide Archive view must never show empty space with
        // the data scrolled off one side.
        let span = (NOW - ERA) * 100.0;
        let out = clamp_view_start(1_500_000.0, span, ERA, NOW);
        assert!(out.is_finite());
        assert!(out <= ERA);
        assert!(out + span >= NOW);
    }

    #[wasm_bindgen_test]
    fn inverted_clock_does_not_panic() {
        // now before the era start (bad system clock) — lo > hi.
        let out = clamp_view_start(0.0, 3600.0, NOW, ERA);
        assert!(out.is_finite());
    }

    #[wasm_bindgen_test]
    fn degenerate_span_passes_through() {
        assert!(approx(clamp_view_start(42.0, 0.0, ERA, NOW), 42.0));
        assert!(approx(clamp_view_start(42.0, -1.0, ERA, NOW), 42.0));
        assert!(clamp_view_start(42.0, f64::NAN, ERA, NOW) == 42.0);
    }

    #[wasm_bindgen_test]
    fn non_finite_desired_passes_through() {
        assert!(clamp_view_start(f64::NAN, 3600.0, ERA, NOW).is_nan());
    }

    #[wasm_bindgen_test]
    fn archive_era_constant_is_mid_1991() {
        // 1991-06-05T00:00:00Z. Guards against an accidental edit to the
        // constant the pan clamp and the Archive era keyline both key off.
        let dt = chrono::DateTime::from_timestamp(NEXRAD_ARCHIVE_START_SECS as i64, 0).unwrap();
        assert!(dt.format("%Y-%m-%d").to_string() == "1991-06-05");
    }
}
