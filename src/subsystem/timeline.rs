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

use crate::core::{
    commit_chunk_ingest, commit_timeline_snapshot, reset_timeline, ChunkIngestResult,
    RadarTimeline, ScanBoundary, TimelineCommit, TimelineRevision, TimelineSnapshotCommit,
};

/// Owner of the timeline (real + shadowed) scan inventory.
#[derive(Default)]
pub(crate) struct Timeline {
    /// Scans currently loaded into memory plus their sweep listings.
    /// Populated by [`crate::app::persistence_manager::PersistenceManager`]'s refresh and
    /// the worker ingest callback.
    pub scans: RadarTimeline,
    /// Per-scan time boundaries derived from the archive listing for the
    /// current site/date. Rendered as subtle markers on the timeline so
    /// users see where scans exist before they're downloaded; cleared on
    /// site change.
    pub shadow_scan_boundaries: Vec<ScanBoundary>,
    revision: TimelineRevision,
    commits: Vec<TimelineCommit>,
}

impl Timeline {
    /// Revision token to capture when dispatching an asynchronous cache load.
    pub(crate) fn revision(&self) -> TimelineRevision {
        self.revision
    }

    /// Immediately commit metadata confirmed by a completed worker chunk.
    pub(crate) fn commit_chunk_ingest(&mut self, result: &ChunkIngestResult) {
        commit_chunk_ingest(
            &mut self.scans,
            &mut self.revision,
            &mut self.commits,
            result,
        );
    }

    /// Commit a cache snapshot using the revision captured at load dispatch.
    pub(crate) fn commit_snapshot(
        &mut self,
        dispatched_at: TimelineRevision,
        snapshot: RadarTimeline,
    ) -> TimelineSnapshotCommit {
        commit_timeline_snapshot(
            &mut self.scans,
            &mut self.revision,
            &mut self.commits,
            dispatched_at,
            snapshot,
        )
    }

    /// Apply an explicit cache wipe as an authoritative empty inventory.
    pub(crate) fn reset(&mut self) {
        reset_timeline(&mut self.scans, &mut self.revision, &mut self.commits);
    }
}
