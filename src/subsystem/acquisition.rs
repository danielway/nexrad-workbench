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
        }
    }
}
