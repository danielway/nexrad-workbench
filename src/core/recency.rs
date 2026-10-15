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
//!   conditions, so they draw only while the playhead is within
//!   [`RECENCY_SECS`] of now ([`live_overlays_apply`]).
//!
//! Frame age is never a color; it is a number in the age pill
//! ([`crate::core::session::format_age`]).

/// The one recency threshold: 15 minutes (a full VCP cycle with margin).
pub(crate) const RECENCY_SECS: f64 = 15.0 * 60.0;

/// Whether `ts_secs` is recent enough to stand for `reference_secs`: at most
/// [`RECENCY_SECS`] before it. A time after the reference is recent.
pub(crate) fn is_recent(reference_secs: f64, ts_secs: f64) -> bool {
    reference_secs - ts_secs <= RECENCY_SECS
}

/// Whether live-only overlays (warnings, the national mosaic) apply at the
/// playhead: it sits within [`RECENCY_SECS`] of now. A playhead ahead of now
/// (a projected region) still applies.
pub(crate) fn live_overlays_apply(now_secs: f64, playhead_secs: f64) -> bool {
    is_recent(now_secs, playhead_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

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
        assert!(live_overlays_apply(now, now));
        assert!(live_overlays_apply(now, now - 14.0 * 60.0));
        assert!(live_overlays_apply(now, now - RECENCY_SECS));
        assert!(!live_overlays_apply(now, now - 16.0 * 60.0));
        // A year behind: archive, no live overlays.
        assert!(!live_overlays_apply(now, now - 365.0 * 86_400.0));
    }
}
