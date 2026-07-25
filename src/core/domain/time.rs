//! Per-frame wall-clock vocabulary.

/// Wall-clock "now" (Unix seconds), captured once at the top of each
/// `update()` (in `apply_frame_setup`) and read by every frame-path
/// consumer — staleness, countdowns, the live tick, timeline rendering —
/// so they can't drift against each other within a frame.
///
/// Event-time stamping (download/ingest records, error timestamps, async
/// tasks off the frame loop, the streaming worker) intentionally keeps
/// calling `TimeModel::wall_clock_time()`: those record when an event
/// happened, not when the frame rendered.
///
/// The wall-clock capture itself (`FrameNow::capture`) lives in the shell
/// layer (`src/state/frame_clock.rs`) — it touches the browser clock.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct FrameNow(pub f64);

impl FrameNow {
    /// Unix seconds.
    pub(crate) fn secs(&self) -> f64 {
        self.0
    }

    /// Unix milliseconds (JS `Date.now()` convention).
    pub(crate) fn millis(&self) -> f64 {
        self.0 * 1000.0
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn secs_returns_inner_value() {
        assert_eq!(FrameNow(123.5).secs(), 123.5);
        assert_eq!(FrameNow(0.0).secs(), 0.0);
        assert_eq!(FrameNow(-4.0).secs(), -4.0);
    }

    #[wasm_bindgen_test]
    fn millis_is_seconds_times_thousand() {
        assert_eq!(FrameNow(2.0).millis(), 2000.0);
        assert_eq!(FrameNow(1.5).millis(), 1500.0);
        assert_eq!(FrameNow(0.0).millis(), 0.0);
    }

    #[wasm_bindgen_test]
    fn default_is_epoch_zero() {
        assert_eq!(FrameNow::default(), FrameNow(0.0));
        assert_eq!(FrameNow::default().secs(), 0.0);
    }

    #[wasm_bindgen_test]
    fn equality_is_value_based() {
        assert_eq!(FrameNow(5.0), FrameNow(5.0));
        assert_ne!(FrameNow(5.0), FrameNow(5.1));
    }
}
