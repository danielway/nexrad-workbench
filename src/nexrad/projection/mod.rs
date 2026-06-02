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

mod engine;
mod inventory;

// Re-exported as the module's public names; constructed at the Phase 4
// ownership flip, so unused in the engine-less build until then.
#[allow(unused_imports)]
pub use engine::ProjectionEngine;
#[allow(unused_imports)]
pub use inventory::{ChunkCoord, KnownChunk, KnownChunkInventory};

use super::streaming_plan::StreamingPlan;
use super::ChunkProjectionInfo;

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
}

#[allow(dead_code)] // Accessors come online as consumers migrate (Phase 5).
impl Projection {
    /// Wrap a freshly built [`StreamingPlan`].
    pub fn from_plan(plan: StreamingPlan) -> Self {
        let revision = plan.revision;
        Self { plan, revision }
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
