//! Per-frame wall-clock capture.

/// Wall-clock "now" (Unix seconds), captured once at the top of each
/// `update()` (in `apply_frame_setup`) and read by every frame-path
/// consumer — staleness, countdowns, the live tick, timeline rendering —
/// so they can't drift against each other within a frame.
///
/// Event-time stamping (download/ingest records, error timestamps, async
/// tasks off the frame loop, the streaming worker) intentionally keeps
/// calling `TimeModel::wall_clock_time()`: those record when an event
/// happened, not when the frame rendered.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameNow(pub f64);

impl FrameNow {
    /// Capture the current wall clock. One call site per frame.
    pub fn capture() -> Self {
        Self(crate::state::TimeModel::wall_clock_time())
    }

    /// Unix seconds.
    pub fn secs(&self) -> f64 {
        self.0
    }

    /// Unix milliseconds (JS `Date.now()` convention).
    pub fn millis(&self) -> f64 {
        self.0 * 1000.0
    }
}
