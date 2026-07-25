//! Timeline subsystem: scan inventory and shadow boundaries.
//!
//! Owns the live in-memory record of which scans (and their sweeps)
//! the app knows about, plus the per-site/date scan-boundary hints
//! produced by the archive listing. UI panels that render the timeline,
//! resolve a playback timestamp to a scan, or test whether a point is
//! covered by historical data all read from one type — [`Timeline`].
//!
//! Before this subsystem existed, `radar_timeline` and
//! `shadow_scan_boundaries` were two unrelated fields on
//! [`AppState`](crate::state::AppState); pairing them up makes the
//! "what scans exist near this time?" question answerable through a
//! single handle and gives a place for future timeline-derivation
//! helpers (e.g. matching-completion lookups, sweep accessors) to land.

use crate::core::RadarTimeline;
use crate::nexrad::ScanBoundary;

/// Owner of the timeline (real + shadowed) scan inventory.
#[derive(Default)]
pub struct Timeline {
    /// Scans currently loaded into memory plus their sweep listings.
    /// Populated by [`crate::app::persistence_manager::PersistenceManager`]'s refresh and
    /// the worker ingest callback.
    pub scans: RadarTimeline,
    /// Per-scan time boundaries derived from the archive listing for the
    /// current site/date. Rendered as subtle markers on the timeline so
    /// users see where scans exist before they're downloaded; cleared on
    /// site change.
    pub shadow_scan_boundaries: Vec<ScanBoundary>,
}
