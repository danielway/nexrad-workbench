//! Per-frame wall-clock capture — the shell half of [`FrameNow`].
//!
//! The `FrameNow` type itself is pure domain vocabulary
//! (`crate::core::FrameNow`); this module holds the one constructor that
//! touches the browser clock.

use crate::core::FrameNow;

impl FrameNow {
    /// Capture the current wall clock. One call site per frame.
    pub fn capture() -> Self {
        Self(crate::state::TimeModel::wall_clock_time())
    }
}
