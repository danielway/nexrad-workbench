//! Acquisition subsystem: owns the download pipeline + acquisition state.
//!
//! Before this subsystem existed, acquisition responsibilities were split
//! across two objects that had to be kept in sync by every caller:
//!
//! - [`crate::state::AcquisitionState`] (on [`crate::state::AppState`]) —
//!   the per-operation tracking (queue, operations, latencies, drawer
//!   tab/expansion state) that UI panels read and write.
//! - [`crate::nexrad::AcquisitionCoordinator`] (on
//!   [`crate::WorkbenchApp`]) — the I/O pipeline (download channels,
//!   cache loader, archive index, data facade, pending-download token).
//!
//! Folding them into one owner — [`Acquisition`] — collapses the
//! "two objects in sync" hazard and gives UI/code a single typed handle
//! through which all acquisition state and I/O is reached.
//!
//! The two existing types are kept as composing fields (`state` and
//! `coordinator`) so call sites read as
//! `acquisition.state.X` / `acquisition.coordinator.X`. Future
//! refactors may collapse one into the other; this seam is intentionally
//! left visible so the move can happen incrementally.

use crate::data::DataFacade;
use crate::nexrad::AcquisitionCoordinator;
use crate::state::AcquisitionState;

/// Owner of all acquisition state and I/O.
pub struct Acquisition {
    /// Per-operation tracking + UI drawer state.
    pub state: AcquisitionState,
    /// Download channels, cache loader, archive index, data facade.
    pub coordinator: AcquisitionCoordinator,
    /// Debounce/idempotency state for reactive (implicit) prefetch.
    pub prefetch_settle: PrefetchSettle,
    /// Wall-clock ms before which the lookback backfill pump should not
    /// recompute (a light 1 Hz throttle; the enqueue is idempotent anyway).
    pub lookback_backfill_next_ms: f64,
    /// Wall-clock ms before which the visible-range listing pump may not
    /// issue another S3 LIST (rate limit: one new listing per interval).
    pub visible_listing_next_ms: f64,
    /// Per-(site, date) wall-clock ms before which a failed listing may be
    /// retried — keeps a failing day from being re-LISTed at the pump rate.
    pub listing_backoff: std::collections::HashMap<(String, chrono::NaiveDate), f64>,
    /// A timeline range the user explicitly selected for bulk download (the
    /// "selection = the fetch" contract). `None` when idle. Set at selection
    /// finalization (or via the confirm modal for long spans) and drained by
    /// `pump_selection_fetch`, which fetches every scan in the range.
    pub selection_fetch_target: Option<SelectionFetchTarget>,
}

/// A committed bulk-download request for a selected timeline range.
///
/// `armed_at_secs` records when the request was armed so the pump can disarm
/// after [`crate::SELECTION_FETCH_DEADLINE_SECS`] if a listing never arrives —
/// the hard backstop that keeps the pump from staying armed forever.
#[derive(Clone, Copy, Debug)]
pub struct SelectionFetchTarget {
    /// Selected `(start, end)` in seconds-since-epoch (normalized start <= end).
    pub range: (f64, f64),
    /// Wall-clock "now" (seconds) when this request was armed.
    pub armed_at_secs: f64,
}

impl Acquisition {
    /// Construct a fresh subsystem.
    ///
    /// `data_facade` is shared with whatever else needs to talk to
    /// IndexedDB (workers, eviction tasks); the coordinator clones it
    /// per request as it dispatches downloads.
    pub fn new(data_facade: DataFacade) -> Self {
        Self {
            state: AcquisitionState::default(),
            coordinator: AcquisitionCoordinator::new(data_facade),
            prefetch_settle: PrefetchSettle::default(),
            lookback_backfill_next_ms: 0.0,
            visible_listing_next_ms: 0.0,
            listing_backoff: std::collections::HashMap::new(),
            selection_fetch_target: None,
        }
    }
}

/// Debounce + idempotency state for reactive prefetch.
///
/// Prefetch must not fire while the user is actively scrubbing or zooming —
/// the view has to settle first (PRODUCT.md §5.1). This tracks the last
/// "what should we prefetch" signature and when it last changed; the pump
/// only acts once the signature has been stable for the debounce window
/// (which collapses to zero during playback so prefetch tracks the advancing
/// cursor continuously). `resolved_signature` suppresses redundant
/// re-evaluation once a settled view has been fully handled.
#[derive(Default)]
pub struct PrefetchSettle {
    last_signature: u64,
    settled_since_ms: Option<f64>,
    resolved_signature: Option<u64>,
}

impl PrefetchSettle {
    /// Record this frame's signature and report whether the view has been
    /// settled for at least `settle_ms`. A changed signature resets the timer
    /// and clears the resolved marker.
    pub fn poll(&mut self, signature: u64, now_ms: f64, settle_ms: f64) -> bool {
        if signature != self.last_signature {
            self.last_signature = signature;
            self.settled_since_ms = Some(now_ms);
            self.resolved_signature = None;
        }
        self.settled_since_ms
            .is_some_and(|since| now_ms - since >= settle_ms)
    }

    /// Whether the current signature has already been fully handled (nothing
    /// left to enqueue, no listing pending), so re-evaluation can be skipped.
    pub fn already_resolved(&self) -> bool {
        self.resolved_signature == Some(self.last_signature)
    }

    /// Mark the current signature as fully handled.
    pub fn mark_resolved(&mut self) {
        self.resolved_signature = Some(self.last_signature);
    }
}

#[cfg(test)]
mod tests {
    use super::PrefetchSettle;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn settle_waits_for_stability_then_resets_on_change() {
        let mut s = PrefetchSettle::default();
        // First sighting starts the timer; not settled yet.
        assert!(!s.poll(42, 1000.0, 300.0));
        // Still inside the debounce window.
        assert!(!s.poll(42, 1200.0, 300.0));
        // Past the window → settled.
        assert!(s.poll(42, 1300.0, 300.0));
        // A new signature resets the timer (e.g. the user scrubbed elsewhere).
        assert!(!s.poll(99, 1300.0, 300.0));
        assert!(s.poll(99, 1600.0, 300.0));
    }

    #[wasm_bindgen_test]
    fn settle_zero_window_fires_immediately() {
        // Playback passes settle_ms = 0: a stable signature fires at once.
        let mut s = PrefetchSettle::default();
        assert!(s.poll(7, 500.0, 0.0));
    }

    #[wasm_bindgen_test]
    fn resolved_marker_clears_when_signature_changes() {
        let mut s = PrefetchSettle::default();
        assert!(s.poll(1, 0.0, 0.0));
        assert!(!s.already_resolved());
        s.mark_resolved();
        assert!(s.already_resolved());
        // Moving the view (new signature) clears the resolved marker so the
        // pump re-evaluates.
        s.poll(2, 0.0, 0.0);
        assert!(!s.already_resolved());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::PrefetchSettle;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn default_is_not_resolved() {
        let s = PrefetchSettle::default();
        assert!(!s.already_resolved());
    }

    #[wasm_bindgen_test]
    fn poll_at_exact_settle_boundary_fires() {
        let mut s = PrefetchSettle::default();
        // Use a non-zero signature so the timer starts at now=1000.
        assert!(!s.poll(5, 1000.0, 500.0));
        // Exactly settle_ms later: 1500 - 1000 = 500 >= 500 → settled.
        assert!(s.poll(5, 1500.0, 500.0));
    }

    #[wasm_bindgen_test]
    fn signature_zero_collides_with_default_and_never_starts_timer() {
        // The default last_signature is 0, so the first poll of signature 0 sees
        // "no change", never sets settled_since_ms, and reports unsettled even
        // with a zero window. (Real signatures are hashes, so 0 is vanishingly
        // unlikely — this pins the documented edge.)
        let mut s = PrefetchSettle::default();
        assert!(!s.poll(0, 500.0, 0.0));
        assert!(!s.poll(0, 9999.0, 0.0));
    }

    #[wasm_bindgen_test]
    fn resolved_marker_survives_repolling_same_signature() {
        let mut s = PrefetchSettle::default();
        s.poll(11, 0.0, 0.0);
        s.mark_resolved();
        assert!(s.already_resolved());
        // Re-polling the SAME signature must not clear the resolved marker.
        s.poll(11, 100.0, 0.0);
        assert!(s.already_resolved());
    }

    #[wasm_bindgen_test]
    fn resolved_marker_is_per_signature() {
        let mut s = PrefetchSettle::default();
        s.poll(1, 0.0, 0.0);
        s.mark_resolved();
        // Different signature → not resolved; back to the first → also not
        // resolved (the marker tracks only the latest signature).
        s.poll(2, 0.0, 0.0);
        assert!(!s.already_resolved());
        s.poll(1, 0.0, 0.0);
        assert!(!s.already_resolved());
    }
}
