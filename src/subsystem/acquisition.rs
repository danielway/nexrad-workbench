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

use crate::core::acquisition::PrefetchSettle;
use crate::data::DataFacade;
use crate::nexrad::AcquisitionCoordinator;
use crate::state::AcquisitionState;

/// Owner of all acquisition state and I/O.
pub(crate) struct Acquisition {
    /// Per-operation tracking + UI drawer state.
    pub state: AcquisitionState,
    /// Download channels, cache loader, archive index, data facade.
    pub coordinator: AcquisitionCoordinator,
    /// Debounce/idempotency state for reactive (implicit) prefetch (the
    /// state machine itself is a core decision type; see
    /// [`crate::core::acquisition::PrefetchSettle`]).
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
pub(crate) struct SelectionFetchTarget {
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
    pub(crate) fn new(data_facade: DataFacade) -> Self {
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
