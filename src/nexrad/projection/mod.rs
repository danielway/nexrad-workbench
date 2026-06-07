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

mod archive_adapter;
mod cached_sweeps;
mod engine;
mod inventory;
mod status;

// Re-exported as the module's public names. The engine is wired (see
// `subsystem::live`); a few helpers are still consumed only within this module,
// hence the `allow(unused_imports)` until every consumer converges.
#[allow(unused_imports)]
pub use archive_adapter::scan_to_projection;
#[allow(unused_imports)]
pub use cached_sweeps::CachedSweepSet;
#[allow(unused_imports)]
pub use engine::{ObservedSweepInputs, ProjectionEngine};
#[allow(unused_imports)]
pub use inventory::{ChunkCoord, KnownChunk, KnownChunkInventory};
#[allow(unused_imports)]
pub use status::{
    build_sweeps, cascade_current_sweeps, derive_sweep_status, CascadeInputs, SweepBounds,
    SweepBuildCtx,
};

use super::streaming_plan::StreamingPlan;
use super::ChunkProjectionInfo;
use std::cell::RefCell;
use std::rc::Rc;

/// Main-thread-shared projection engine. The streaming loop holds a clone and
/// feeds it observations/listings while reading sleep targets; UI consumers read
/// the same instance. Sound because the streaming loop runs on the main thread
/// (`spawn_local`), so borrows never cross threads — and, per the engine's
/// invariant, never span an `.await`.
pub type SharedProjectionEngine = Rc<RefCell<ProjectionEngine>>;

/// Construct a fresh shared engine.
pub fn new_shared_engine() -> SharedProjectionEngine {
    Rc::new(RefCell::new(ProjectionEngine::new()))
}

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

/// How a sweep's time bounds were derived (bound *provenance*, orthogonal to
/// acquisition `status`). Mirrors the old `state::vcp_position::SweepTiming`
/// with an explicit `Projected` variant for the library-forecast path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Consumed as surfaces migrate.
pub enum SweepTimingProvenance {
    /// Actual observed radial timestamps.
    Observed,
    /// Anchored to actual chunk data / a known predecessor.
    Anchored,
    /// Library physics forecast (collection-time projection).
    Projected,
    /// Purely VCP-weighted estimate.
    Estimated,
}

/// Source-agnostic availability the timeline renders by. Adds `Available`
/// (published in S3 but not downloaded by us) to the old three.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Consumed as surfaces migrate.
pub enum SweepAvailability {
    /// Persisted/renderable — collected by us (archive download or live flush).
    Cached,
    /// Actively being received now.
    Collecting,
    /// Published in S3 but not downloaded by us.
    Available,
    /// Forecast only; not present yet.
    Projected,
}

/// A single chunk's time + azimuth span within a sweep (live in-progress only).
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct ChunkSpan {
    pub start: f64,
    pub end: f64,
    pub first_azimuth: f32,
    pub last_azimuth: f32,
    pub radial_count: u32,
}

/// State for extrapolating the live sweep-line azimuth between radials. The
/// rate comes from the projection; `last_radial_*` are filled per frame by the
/// live model.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct ExtrapolationState {
    pub last_radial_azimuth: f32,
    pub last_radial_time: f64,
    /// Degrees per second for the current sweep (360 / sweep_duration).
    pub degrees_per_sec: f64,
}

/// One projected sweep — the universal per-sweep render type every consumer
/// reads, carrying the COLLECTION-time span the timeline / VCP panel / sweep
/// line render from.
#[derive(Clone, Debug)]
pub struct SweepProjection {
    /// 1-based elevation number.
    pub elevation_number: u8,
    /// Elevation angle in degrees.
    pub elevation_angle: f32,
    /// Which scan (current vs. next) this sweep belongs to.
    pub scan_role: ProjectionScanRole,
    /// Acquisition/display status.
    pub status: SweepProjectionStatus,
    /// How the time bounds were derived (provenance; orthogonal to `status`).
    pub timing: SweepTimingProvenance,
    /// COLLECTION-time span (radar physically scans) — drives timeline / VCP
    /// panel / sweep line.
    pub collection_start_secs: f64,
    pub collection_end_secs: f64,
    /// Chunks expected in the sweep (0 when known only from the cache).
    pub chunks_in_sweep: usize,
    /// Chunks received so far (live in-progress).
    pub chunks_received: u32,
    /// Radials received so far (live in-progress).
    pub radials_received: u32,
    /// Azimuth rotation rate (deg/s) for the sweep-line extrapolation.
    pub azimuth_rate_dps: f64,
    /// Per-chunk azimuth spans (live in-progress only; empty otherwise).
    pub chunks: Vec<ChunkSpan>,
}

#[allow(dead_code)] // Query helpers come online as consumers migrate.
impl SweepProjection {
    /// We have this cut cached locally.
    pub fn is_complete(&self) -> bool {
        self.status == SweepProjectionStatus::CollectedByUs
    }
    /// This cut is currently being received.
    pub fn is_in_progress(&self) -> bool {
        self.status == SweepProjectionStatus::InProgress
    }
    /// This cut hasn't started and isn't published yet.
    pub fn is_future(&self) -> bool {
        self.status == SweepProjectionStatus::FutureExpected
    }
    /// Whether the bounds are observed (not estimated/projected).
    pub fn is_observed(&self) -> bool {
        self.timing == SweepTimingProvenance::Observed
    }
    /// COLLECTION-span duration in seconds.
    pub fn duration(&self) -> f64 {
        self.collection_end_secs - self.collection_start_secs
    }
    /// Source-agnostic availability for the timeline.
    pub fn availability(&self) -> SweepAvailability {
        match self.status {
            SweepProjectionStatus::CollectedByUs => SweepAvailability::Cached,
            SweepProjectionStatus::InProgress => SweepAvailability::Collecting,
            SweepProjectionStatus::AvailableNotCollected => SweepAvailability::Available,
            SweepProjectionStatus::FutureExpected => SweepAvailability::Projected,
        }
    }
}

/// A whole scan's per-sweep projection — the `VcpPositionModel` replacement.
/// Carries the current scan's sweeps plus the next-scan ghost and the live
/// extrapolation state, with the per-frame query methods consumers call.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Consumed as surfaces migrate (Step 6).
pub struct ScanProjection {
    pub vcp_number: u16,
    pub volume_start: f64,
    pub volume_end: f64,
    pub complete: bool,
    pub scan_key: Option<String>,
    /// Current-scan sweeps (`scan_role == CurrentInProgress`).
    pub sweeps: Vec<SweepProjection>,
    pub extrapolation: Option<ExtrapolationState>,
    /// Faded next-scan ghost (its `sweeps` are `scan_role == NextScan`).
    pub next_scan_ghost: Option<Box<ScanProjection>>,
}

#[allow(dead_code)] // Query methods come online as consumers migrate (Step 6).
impl ScanProjection {
    /// Sweep whose COLLECTION span contains `ts`.
    pub fn sweep_at(&self, ts: f64) -> Option<&SweepProjection> {
        self.sweeps
            .iter()
            .find(|s| ts >= s.collection_start_secs && ts <= s.collection_end_secs)
    }

    /// Estimated sweep-line azimuth at `ts` — live extrapolation, else archive
    /// interpolation within the containing sweep.
    pub fn estimated_azimuth_at(&self, ts: f64) -> Option<f32> {
        if let Some(ref ext) = self.extrapolation {
            let dt = ts - ext.last_radial_time;
            if !(0.0..=120.0).contains(&dt) {
                return None;
            }
            let estimated = ext.last_radial_azimuth as f64 + dt * ext.degrees_per_sec;
            return Some(((estimated % 360.0 + 360.0) % 360.0) as f32);
        }
        let sweep = self.sweep_at(ts)?;
        let duration = sweep.collection_end_secs - sweep.collection_start_secs;
        if duration <= 0.0 {
            return None;
        }
        let progress = (ts - sweep.collection_start_secs) / duration;
        Some((progress * 360.0 % 360.0) as f32)
    }

    /// Volume progress 0.0..1.0 at `ts`.
    pub fn progress_at(&self, ts: f64) -> f32 {
        let duration = self.volume_end - self.volume_start;
        if duration <= 0.0 {
            return 0.0;
        }
        ((ts - self.volume_start) / duration).clamp(0.0, 1.0) as f32
    }

    /// Estimated 0-based elevation index at `ts` (containment, else next
    /// not-yet-ended, else last).
    pub fn elevation_index_at(&self, ts: f64) -> Option<usize> {
        for (i, s) in self.sweeps.iter().enumerate() {
            if ts >= s.collection_start_secs && ts <= s.collection_end_secs {
                return Some(i);
            }
        }
        for (i, s) in self.sweeps.iter().enumerate() {
            if ts < s.collection_end_secs {
                return Some(i);
            }
        }
        if self.sweeps.is_empty() {
            None
        } else {
            Some(self.sweeps.len() - 1)
        }
    }

    /// Count of cached (collected-by-us) sweeps.
    pub fn completed_count(&self) -> usize {
        self.sweeps.iter().filter(|s| s.is_complete()).count()
    }
}

/// Assemble the live current-scan container from a flat per-sweep list: the
/// `CurrentInProgress` sweeps become the scan body; the `NextScan` sweeps become
/// a faded ghost. `extrapolation` is left `None` — the live model fills it per
/// frame from the last radial + the current sweep's rate.
#[allow(dead_code)] // Consumed by the engine (Step 5) + LiveRadarModel (Step 6).
pub fn assemble_live_scan(
    sweeps: &[SweepProjection],
    vcp_number: u16,
    volume_start: f64,
    volume_end: f64,
    scan_key: Option<String>,
) -> ScanProjection {
    let current: Vec<SweepProjection> = sweeps
        .iter()
        .filter(|s| matches!(s.scan_role, ProjectionScanRole::CurrentInProgress))
        .cloned()
        .collect();
    let next: Vec<SweepProjection> = sweeps
        .iter()
        .filter(|s| matches!(s.scan_role, ProjectionScanRole::NextScan))
        .cloned()
        .collect();
    let next_scan_ghost = if next.is_empty() {
        None
    } else {
        let gs = next
            .iter()
            .map(|s| s.collection_start_secs)
            .fold(f64::MAX, f64::min);
        let ge = next
            .iter()
            .map(|s| s.collection_end_secs)
            .fold(f64::MIN, f64::max);
        Some(Box::new(ScanProjection {
            vcp_number,
            volume_start: gs,
            volume_end: ge,
            complete: false,
            scan_key: None,
            sweeps: next,
            extrapolation: None,
            next_scan_ghost: None,
        }))
    };
    ScanProjection {
        vcp_number,
        volume_start,
        volume_end,
        complete: false,
        scan_key,
        sweeps: current,
        extrapolation: None,
        next_scan_ghost,
    }
}

/// The unified forward-looking projection emitted by the engine and read by all
/// consumers. Pairs the per-chunk math carrier (`plan`, read by the acquisition
/// loop) with the assembled per-sweep display container (`live_scan`).
#[derive(Clone, Debug)]
pub struct Projection {
    /// The wrapped producer output — the math carrier (per-chunk forecasts)
    /// the acquisition loop reads for `next_target` and sleep targets.
    pub plan: StreamingPlan,
    /// Mirror of `plan.revision` — bumped by the projector on every build, for
    /// consumers to skip redraws / detect a fresher projection.
    #[allow(dead_code)] // Change-detection mirror; no consumer reads it yet.
    pub revision: u64,
    /// The live current-scan container the display consumers read (current-scan
    /// sweeps + next-scan ghost). `None` until the engine assembles it.
    pub live_scan: Option<ScanProjection>,
}

// Read-delegators to the wrapped `plan`, exposing the consumer-facing API on
// `Projection`. Not every delegator has a caller yet (some are read off the
// `plan` directly today); `allow(dead_code)` until consumers converge on them.
#[allow(dead_code)]
impl Projection {
    /// Wrap a plan together with its assembled live-scan container.
    pub fn from_parts(plan: StreamingPlan, live_scan: Option<ScanProjection>) -> Self {
        let revision = plan.revision;
        Self {
            plan,
            revision,
            live_scan,
        }
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
    fn from_parts_mirrors_revision_and_delegates() {
        let plan = StreamingPlan::empty_for_test(StreamingFilter::All, 42);
        let projection = Projection::from_parts(plan, None);
        assert_eq!(projection.revision, 42);
        // Delegating accessors resolve against the wrapped plan.
        assert!(projection.next_target().is_none());
        assert!(!projection.next_target_in_next_volume());
        assert!(projection.current_volume_chunks().is_empty());
    }
}
