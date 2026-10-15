//! Pure timeline-strip interaction decisions.
//!
//! The strip shares ONE egui response across seek, scrub, inspect and pan, so "which gesture owns the pointer this frame" is a real
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

/// The live zoom (px/s): individual sweeps and chunks are visible, and the
/// tier sits comfortably above the Micro-enter threshold.
pub(crate) const LIVE_ZOOM: f64 = 2.0;

/// The archive (macro) default zoom (px/s): about 1.8 hours across 1000 px.
pub(crate) const ARCHIVE_ZOOM: f64 = 0.15;

/// The strip's zoom at boot. A session that boots pinned to live starts at
/// the live zoom (or a link's closer zoom), so going live never jumps the
/// strip; a deep link to an archive time starts at its own zoom, else the
/// macro default.
pub(crate) fn boot_zoom(link_zoom: Option<f64>, boots_live: bool) -> f64 {
    let link_zoom = link_zoom.filter(|z| z.is_finite() && *z > 0.0);
    match (boots_live, link_zoom) {
        (true, Some(z)) => z.max(LIVE_ZOOM),
        (true, None) => LIVE_ZOOM,
        (false, Some(z)) => z,
        (false, None) => ARCHIVE_ZOOM,
    }
}

/// Which pointer gesture owns the timeline strip this frame.
///
/// Exactly one wins. Precedence is `Pan > Inspect > Seek`, and the reasons
/// are behavioral, not arbitrary:
/// - **Pan** is modeless navigation; it must never also move the playhead, or
///   the gesture that exists to stop accidental seeks would cause them.
/// - **Inspect** is a discrete secondary *click*.
/// - **Seek** is the fallback for a plain primary press/drag.
///
/// Ranges are not made here: the selection strip above the frames owns them
/// ([`crate::core::selection_strip`]), so no modifier turns a scrub into a
/// selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StripGesture {
    /// Nothing this frame — or the pointer landed on a control that owns its
    /// own clicks (failed-cell tick, now cap).
    None,
    /// Pan the view: middle-drag, or ctrl/cmd + primary drag.
    Pan,
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
    /// A clean secondary click (a press that did not become a drag).
    pub secondary_clicked: bool,
    /// A middle-button drag is ongoing this frame.
    pub middle_dragging: bool,
    /// Ctrl or cmd is held — the pan modifier.
    pub pan_mod: bool,
    /// The press landed inside a rect belonging to another control.
    pub on_suppressed_rect: bool,
    /// This drag *began* inside a suppressed rect, so the other control owns
    /// it for the drag's whole lifetime.
    pub scrub_suppressed: bool,
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

    // A clean secondary click opens the inspector.
    if input.secondary_clicked {
        return StripGesture::Inspect;
    }

    // Seek/scrub last, and never from a control's own hit rect.
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

    // ---- resolve_gesture: no range selection on the frames -------------

    #[wasm_bindgen_test]
    fn shift_drag_scrubs_and_secondary_drag_does_nothing() {
        // Ranges are made on the selection strip; a modifier no longer turns
        // the frames into a selection surface.
        let drag = StripInput {
            primary_drag_started: true,
            primary_dragging: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&drag) == StripGesture::Seek);
        assert!(resolve_gesture(&StripInput::default()) == StripGesture::None);
    }

    #[wasm_bindgen_test]
    fn secondary_click_inspects() {
        let click = StripInput {
            secondary_clicked: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&click) == StripGesture::Inspect);
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
        // A control under the press (the now cap) owns the drag for its whole
        // lifetime.
        let input = StripInput {
            primary_dragging: true,
            scrub_suppressed: true,
            ..Default::default()
        };
        assert!(resolve_gesture(&input) == StripGesture::None);
    }

    #[wasm_bindgen_test]
    fn idle_frame_is_none() {
        assert!(resolve_gesture(&StripInput::default()) == StripGesture::None);
    }

    // ---- boot_zoom --------------------------------------------------------

    #[wasm_bindgen_test]
    fn booting_live_starts_at_the_live_zoom() {
        assert!(approx(boot_zoom(None, true), LIVE_ZOOM));
        // A link zoomed out further than live would jump on the first frame.
        assert!(approx(boot_zoom(Some(0.15), true), LIVE_ZOOM));
        // A closer link zoom is kept.
        assert!(approx(boot_zoom(Some(8.0), true), 8.0));
    }

    #[wasm_bindgen_test]
    fn an_archive_deep_link_boots_at_its_zoom_or_the_macro_default() {
        assert!(approx(boot_zoom(None, false), ARCHIVE_ZOOM));
        assert!(approx(boot_zoom(Some(0.01), false), 0.01));
        assert!(approx(boot_zoom(Some(f64::NAN), false), ARCHIVE_ZOOM));
        assert!(approx(boot_zoom(Some(-1.0), true), LIVE_ZOOM));
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
