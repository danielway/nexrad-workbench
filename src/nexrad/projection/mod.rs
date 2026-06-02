//! Unified projection architecture.
//!
//! This module is the single owner of forward-looking radar timing. It is being
//! built incrementally (see the plan): the eventual `ProjectionEngine` collects
//! every projection input — VCP, rolling timing stats, the collection anchor, a
//! known-available-chunks inventory, cached sweeps (possibly sparse), archive
//! boundaries, and the active filter — and emits one [`Projection`] that every
//! consumer (timeline, VCP panel, sweep line, acquisition loop) reads.
//!
//! Phase 0 introduces only [`Projection`], a thin wrapper that *contains*
//! today's [`StreamingPlan`]. Later phases enrich it with per-sweep status on
//! both the collection and availability axes and migrate consumers onto it; the
//! wrapped `plan` is retained as the math carrier until those migrations land.

mod cached_sweeps;
mod engine;
mod inventory;
mod status;

// Re-exported as the module's public names; constructed at the Phase 4
// ownership flip, so unused in the engine-less build until then.
#[allow(unused_imports)]
pub use cached_sweeps::CachedSweepSet;
#[allow(unused_imports)]
pub use engine::ProjectionEngine;
#[allow(unused_imports)]
pub use inventory::{ChunkCoord, KnownChunk, KnownChunkInventory};
#[allow(unused_imports)]
pub use status::{build_sweeps, derive_sweep_status, SweepBuildCtx};

use super::streaming_plan::StreamingPlan;
use super::ChunkProjectionInfo;

/// Where a projected sweep sits relative to the streaming anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Consumed as surfaces migrate (Phase 5).
pub enum ProjectionScanRole {
    /// A sweep of the volume currently being received.
    CurrentInProgress,
    /// A sweep of the *next* volume, projected one scan ahead.
    NextScan,
}

/// Acquisition/display status of a single projected sweep.
///
/// Precedence when deriving: `CollectedByUs` > `InProgress` > `AvailableNotCollected`
/// > `FutureExpected`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Consumed as surfaces migrate (Phase 5).
pub enum SweepProjectionStatus {
    /// We have this sweep cached locally (possibly sparse coverage).
    CollectedByUs,
    /// Published in S3 (per the inventory) but not downloaded by us.
    AvailableNotCollected,
    /// Currently being received.
    InProgress,
    /// Neither available nor cached yet — purely projected.
    FutureExpected,
}

/// One projected sweep on both the COLLECTION and AVAILABILITY axes.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Consumed as surfaces migrate (Phase 5).
pub struct SweepProjection {
    /// 1-based elevation number.
    pub elevation_number: u8,
    /// Which scan (current vs. next) this sweep belongs to.
    pub scan_role: ProjectionScanRole,
    /// Acquisition/display status.
    pub status: SweepProjectionStatus,
    /// COLLECTION-time span (radar physically scans) — drives timeline / VCP
    /// panel / sweep line.
    pub collection_start_secs: f64,
    pub collection_end_secs: f64,
    /// AVAILABILITY time (latest chunk of the sweep appears on S3) — drives
    /// acquisition. Equals `collection_end` for already-collected cuts.
    pub available_at_secs: f64,
    /// Chunks expected in the sweep (0 when known only from the cache).
    pub chunks_in_sweep: usize,
    /// Azimuth rotation rate (deg/s) for the sweep-line extrapolation.
    pub azimuth_rate_dps: f64,
}

/// The unified forward-looking projection emitted by the engine and read by all
/// consumers.
///
/// Phase 0: a wrapper around [`StreamingPlan`] that mirrors its `revision` for
/// cheap change-detection and re-exposes the accessors consumers need, so
/// surfaces can begin targeting `Projection` before the richer per-sweep view
/// (status + dual time axes) lands. `plan` stays the authoritative producer
/// output throughout the migration.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Consumed once the engine is constructed (Phase 4+).
pub struct Projection {
    /// The wrapped producer output. Remains the math carrier (per-chunk
    /// forecasts) until consumers migrate to the per-sweep view.
    pub plan: StreamingPlan,
    /// Mirror of `plan.revision` — bumped by the projector on every build, used
    /// by consumers to skip redraws / detect a fresher projection.
    pub revision: u64,
    /// Per-sweep projection for the current + next scan, each tagged with
    /// status on both the collection and availability axes. Includes cached
    /// (`CollectedByUs`) sweeps for the display view; acquisition consumers
    /// filter those out via [`Self::acquisition_sweeps`].
    pub sweeps: Vec<SweepProjection>,
}

#[allow(dead_code)] // Accessors come online as consumers migrate (Phase 5).
impl Projection {
    /// Wrap a freshly built [`StreamingPlan`] with no per-sweep view (the
    /// Phase 0 shape — used until the engine populates `sweeps`).
    pub fn from_plan(plan: StreamingPlan) -> Self {
        let revision = plan.revision;
        Self {
            plan,
            revision,
            sweeps: Vec::new(),
        }
    }

    /// Wrap a plan together with its per-sweep projection.
    pub fn from_plan_with_sweeps(plan: StreamingPlan, sweeps: Vec<SweepProjection>) -> Self {
        let revision = plan.revision;
        Self {
            plan,
            revision,
            sweeps,
        }
    }

    /// All projected sweeps, including cached (`CollectedByUs`) cuts — the
    /// DISPLAY view (timeline, VCP panel, sweep line).
    pub fn display_sweeps(&self) -> &[SweepProjection] {
        &self.sweeps
    }

    /// Sweeps the acquisition loop still needs — everything except the cuts we
    /// already have cached.
    pub fn acquisition_sweeps(&self) -> impl Iterator<Item = &SweepProjection> {
        self.sweeps
            .iter()
            .filter(|s| s.status != SweepProjectionStatus::CollectedByUs)
    }

    /// The immediate next chunk the streaming loop plans to download.
    pub fn next_target(&self) -> Option<&ChunkProjectionInfo> {
        self.plan.next_target()
    }

    /// Whether the immediate next download target falls in the *next* volume.
    pub fn next_target_in_next_volume(&self) -> bool {
        self.plan.next_target_in_next_volume()
    }

    /// Elevation number (1-based) of the immediate next download target.
    pub fn next_target_elevation(&self) -> Option<u8> {
        self.plan.next_target_elevation()
    }

    /// Seconds from `now_secs` until the next target becomes available in S3
    /// (drives the UI countdown).
    pub fn next_available_in_secs(&self, now_secs: f64) -> Option<f64> {
        self.plan.next_available_in_secs(now_secs)
    }

    /// Per-chunk info for the current in-progress volume.
    pub fn current_volume_chunks(&self) -> &[ChunkProjectionInfo] {
        &self.plan.current_volume_chunks
    }

    /// Per-chunk info for the next volume, when the projection extends into it.
    pub fn next_volume_chunks(&self) -> Option<&[ChunkProjectionInfo]> {
        self.plan.next_volume_chunks.as_deref()
    }

    /// COLLECTION time the radar finishes the current volume's final chunk.
    pub fn current_volume_end_collection_secs(&self) -> Option<f64> {
        self.plan.current_volume_end_collection_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexrad::streaming_filter::StreamingFilter;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn from_plan_mirrors_revision_and_delegates() {
        let plan = StreamingPlan::empty_for_test(StreamingFilter::All, 42);
        let projection = Projection::from_plan(plan);
        assert_eq!(projection.revision, 42);
        // Delegating accessors resolve against the wrapped plan.
        assert!(projection.next_target().is_none());
        assert!(!projection.next_target_in_next_volume());
        assert!(projection.current_volume_chunks().is_empty());
    }
}
