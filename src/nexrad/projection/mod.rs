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
mod observations;
mod status;

// Re-exported as the module's public names. The engine is wired (see
// `subsystem::live`); a few helpers are still consumed only within this module,
// hence the `allow(unused_imports)` until every consumer converges.
#[allow(unused_imports)]
pub(crate) use archive_adapter::scan_to_projection;
#[allow(unused_imports)]
pub(crate) use cached_sweeps::CachedSweepSet;
#[allow(unused_imports)]
pub(crate) use engine::ProjectionEngine;
#[allow(unused_imports)]
pub(crate) use inventory::{ChunkCoord, KnownChunk, KnownChunkInventory};
#[allow(unused_imports)]
pub(crate) use observations::VolumeObservations;
#[allow(unused_imports)]
pub(crate) use status::{
    build_sweeps, cascade_current_sweeps, derive_sweep_status, CascadeInputs, SweepBounds,
    SweepBuildCtx,
};

use super::live::streaming_plan::StreamingPlan;
use super::ChunkProjectionInfo;
use std::cell::RefCell;
use std::rc::Rc;

/// Main-thread-shared projection engine. The streaming loop holds a clone and
/// feeds it observations/listings while reading sleep targets; UI consumers read
/// the same instance. Sound because the streaming loop runs on the main thread
/// (`spawn_local`), so borrows never cross threads — and, per the engine's
/// invariant, never span an `.await`.
pub(crate) type SharedProjectionEngine = Rc<RefCell<ProjectionEngine>>;

/// Construct a fresh shared engine.
pub(crate) fn new_shared_engine() -> SharedProjectionEngine {
    Rc::new(RefCell::new(ProjectionEngine::new()))
}

/// Where a projected sweep sits relative to the streaming anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionScanRole {
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
pub(crate) enum SweepProjectionStatus {
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
pub(crate) enum SweepTimingProvenance {
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
pub(crate) enum SweepAvailability {
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
pub(crate) struct ChunkSpan {
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
pub(crate) struct ExtrapolationState {
    pub last_radial_azimuth: f32,
    pub last_radial_time: f64,
    /// Degrees per second for the current sweep (360 / sweep_duration).
    pub degrees_per_sec: f64,
}

/// One projected sweep — the universal per-sweep render type every consumer
/// reads, carrying the COLLECTION-time span the timeline / VCP panel / sweep
/// line render from.
#[derive(Clone, Debug)]
pub(crate) struct SweepProjection {
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

impl SweepProjection {
    /// We have this cut cached locally.
    pub(crate) fn is_complete(&self) -> bool {
        self.status == SweepProjectionStatus::CollectedByUs
    }
    /// This cut is currently being received.
    pub(crate) fn is_in_progress(&self) -> bool {
        self.status == SweepProjectionStatus::InProgress
    }
    /// This cut hasn't started and isn't published yet.
    #[cfg(test)]
    pub(crate) fn is_future(&self) -> bool {
        self.status == SweepProjectionStatus::FutureExpected
    }
    /// Whether the bounds are observed (not estimated/projected).
    pub(crate) fn is_observed(&self) -> bool {
        self.timing == SweepTimingProvenance::Observed
    }
    /// COLLECTION-span duration in seconds.
    pub(crate) fn duration(&self) -> f64 {
        self.collection_end_secs - self.collection_start_secs
    }
    /// Source-agnostic availability for the timeline.
    pub(crate) fn availability(&self) -> SweepAvailability {
        match self.status {
            SweepProjectionStatus::CollectedByUs => SweepAvailability::Cached,
            SweepProjectionStatus::InProgress => SweepAvailability::Collecting,
            SweepProjectionStatus::AvailableNotCollected => SweepAvailability::Available,
            SweepProjectionStatus::FutureExpected => SweepAvailability::Projected,
        }
    }
}

/// A whole scan's per-sweep projection — the unified live + archive display
/// container. Carries the current scan's sweeps plus the next-scan ghost, the
/// live extrapolation state, and the VCP pattern + elevation roster the panels
/// read, with the per-frame query methods consumers call.
#[derive(Clone, Debug)]
pub(crate) struct ScanProjection {
    pub vcp_number: u16,
    /// Full VCP pattern for elevation-angle lookups (display); `None` pre-VCP.
    pub vcp_pattern: Option<crate::data::keys::ExtractedVcp>,
    /// Expected-vs-received elevation roster for the in-progress volume.
    pub roster: crate::core::VolumeElevationRoster,
    /// Elevation currently being received (`None` between cuts). Mirrors the
    /// engine's in-progress input exactly — independent of per-sweep status.
    pub in_progress_elevation: Option<u8>,
    /// Radials received so far for the in-progress cut (`None` when idle).
    pub in_progress_radials: Option<u32>,
    pub volume_start: f64,
    pub volume_end: f64,
    /// Current-scan sweeps (`scan_role == CurrentInProgress`).
    pub sweeps: Vec<SweepProjection>,
    pub extrapolation: Option<ExtrapolationState>,
    /// Faded next-scan ghost (its `sweeps` are `scan_role == NextScan`).
    pub next_scan_ghost: Option<Box<ScanProjection>>,
}

impl ScanProjection {
    /// Sweep whose COLLECTION span contains `ts`.
    pub(crate) fn sweep_at(&self, ts: f64) -> Option<&SweepProjection> {
        self.sweeps
            .iter()
            .find(|s| ts >= s.collection_start_secs && ts <= s.collection_end_secs)
    }

    /// Estimated sweep-line azimuth at `ts` — live extrapolation, else archive
    /// interpolation within the containing sweep.
    pub(crate) fn estimated_azimuth_at(&self, ts: f64) -> Option<f32> {
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
    pub(crate) fn progress_at(&self, ts: f64) -> f32 {
        let duration = self.volume_end - self.volume_start;
        if duration <= 0.0 {
            return 0.0;
        }
        ((ts - self.volume_start) / duration).clamp(0.0, 1.0) as f32
    }

    /// Estimated 0-based elevation index at `ts` (containment, else next
    /// not-yet-ended, else last).
    pub(crate) fn elevation_index_at(&self, ts: f64) -> Option<usize> {
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
    pub(crate) fn completed_count(&self) -> usize {
        self.sweeps.iter().filter(|s| s.is_complete()).count()
    }
}

/// Assemble the live current-scan container from a flat per-sweep list: the
/// `CurrentInProgress` sweeps become the scan body; the `NextScan` sweeps become
/// a faded ghost. `extrapolation` is left `None` — the live model fills it per
/// frame from the last radial + the current sweep's rate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn assemble_live_scan(
    sweeps: &[SweepProjection],
    vcp_number: u16,
    vcp_pattern: Option<crate::data::keys::ExtractedVcp>,
    roster: crate::core::VolumeElevationRoster,
    in_progress_elevation: Option<u8>,
    in_progress_radials: Option<u32>,
    volume_start: f64,
    volume_end: f64,
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
            vcp_pattern: vcp_pattern.clone(),
            roster: crate::core::VolumeElevationRoster::default(),
            in_progress_elevation: None,
            in_progress_radials: None,
            volume_start: gs,
            volume_end: ge,
            sweeps: next,
            extrapolation: None,
            next_scan_ghost: None,
        }))
    };
    ScanProjection {
        vcp_number,
        vcp_pattern,
        roster,
        in_progress_elevation,
        in_progress_radials,
        volume_start,
        volume_end,
        sweeps: current,
        extrapolation: None,
        next_scan_ghost,
    }
}

/// The unified forward-looking projection emitted by the engine and read by all
/// consumers. Pairs the per-chunk math carrier (`plan`, read by the acquisition
/// loop) with the assembled per-sweep display container (`live_scan`).
#[derive(Clone, Debug)]
pub(crate) struct Projection {
    /// The wrapped producer output — the math carrier (per-chunk forecasts)
    /// the acquisition loop reads for `next_target` and sleep targets.
    pub plan: StreamingPlan,
    /// The live current-scan container the display consumers read (current-scan
    /// sweeps + next-scan ghost). `None` until the engine assembles it.
    pub live_scan: Option<ScanProjection>,
}

// Read-delegators to the wrapped `plan` — the consumer-facing API on `Projection`.
impl Projection {
    /// Wrap a plan together with its assembled live-scan container.
    pub(crate) fn from_parts(plan: StreamingPlan, live_scan: Option<ScanProjection>) -> Self {
        Self { plan, live_scan }
    }

    /// The immediate next chunk the streaming loop plans to download.
    pub(crate) fn next_target(&self) -> Option<&ChunkProjectionInfo> {
        self.plan.next_target()
    }

    /// Whether the immediate next download target falls in the *next* volume.
    #[cfg(test)]
    pub(crate) fn next_target_in_next_volume(&self) -> bool {
        self.plan.next_target_in_next_volume()
    }

    /// Seconds from `now_secs` until the next target becomes available in S3
    /// (drives the UI countdown).
    pub(crate) fn next_available_in_secs(&self, now_secs: f64) -> Option<f64> {
        self.plan.next_available_in_secs(now_secs)
    }

    /// Per-chunk info for the current in-progress volume.
    pub(crate) fn current_volume_chunks(&self) -> &[ChunkProjectionInfo] {
        &self.plan.current_volume_chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexrad::live::streaming_filter::StreamingFilter;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn from_parts_delegates_to_the_wrapped_plan() {
        let plan = StreamingPlan::empty_for_test(StreamingFilter::All, 42);
        let projection = Projection::from_parts(plan, None);
        assert_eq!(projection.plan.revision, 42);
        // Delegating accessors resolve against the wrapped plan.
        assert!(projection.next_target().is_none());
        assert!(!projection.next_target_in_next_volume());
        assert!(projection.current_volume_chunks().is_empty());
    }

    // ── ScanProjection per-frame query methods ──────────────────────────────

    fn sweep(elev: u8, start: f64, end: f64) -> SweepProjection {
        SweepProjection {
            elevation_number: elev,
            elevation_angle: 0.5 * elev as f32,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status: SweepProjectionStatus::FutureExpected,
            timing: SweepTimingProvenance::Estimated,
            collection_start_secs: start,
            collection_end_secs: end,
            chunks_in_sweep: 0,
            chunks_received: 0,
            radials_received: 0,
            azimuth_rate_dps: 20.0,
            chunks: Vec::new(),
        }
    }

    fn scan(sweeps: Vec<SweepProjection>, vol_start: f64, vol_end: f64) -> ScanProjection {
        ScanProjection {
            vcp_number: 0,
            vcp_pattern: None,
            roster: crate::core::VolumeElevationRoster::default(),
            in_progress_elevation: None,
            in_progress_radials: None,
            volume_start: vol_start,
            volume_end: vol_end,
            sweeps,
            extrapolation: None,
            next_scan_ghost: None,
        }
    }

    /// With live extrapolation present, azimuth is `last_az + dt*rate` wrapped to
    /// [0,360); outside the (0..=120s) dt window it returns None.
    #[wasm_bindgen_test]
    fn estimated_azimuth_extrapolation_window_and_wrap() {
        let mut sp = scan(vec![sweep(1, 1000.0, 1010.0)], 1000.0, 1010.0);
        sp.extrapolation = Some(ExtrapolationState {
            last_radial_azimuth: 350.0,
            last_radial_time: 1000.0,
            degrees_per_sec: 20.0,
        });
        // dt = 1.0 → 350 + 20 = 370 → wraps to 10.
        let az = sp.estimated_azimuth_at(1001.0).unwrap();
        assert!((az - 10.0).abs() < 1e-3, "got {az}");

        // dt = 0 (boundary, inclusive) → 350.
        assert!((sp.estimated_azimuth_at(1000.0).unwrap() - 350.0).abs() < 1e-3);
        // dt = 120 (upper boundary, inclusive) → 350 + 2400 = 2750 → 2750%360=230.
        assert!((sp.estimated_azimuth_at(1120.0).unwrap() - 230.0).abs() < 1e-3);

        // dt just outside the window (negative and >120) → None.
        assert!(sp.estimated_azimuth_at(999.0).is_none());
        assert!(sp.estimated_azimuth_at(1121.0).is_none());
    }

    /// Negative arithmetic wraps positively: a backward azimuth never goes
    /// negative (the `(x%360+360)%360` normalization).
    #[wasm_bindgen_test]
    fn estimated_azimuth_negative_wraps_positive() {
        let mut sp = scan(vec![sweep(1, 1000.0, 1010.0)], 1000.0, 1010.0);
        sp.extrapolation = Some(ExtrapolationState {
            last_radial_azimuth: 10.0,
            last_radial_time: 1000.0,
            degrees_per_sec: -20.0, // rotating "backward"
        });
        // dt=1 → 10 - 20 = -10 → wraps to 350.
        let az = sp.estimated_azimuth_at(1001.0).unwrap();
        assert!((az - 350.0).abs() < 1e-3, "got {az}");
    }

    /// Without extrapolation, azimuth interpolates within the containing sweep
    /// by progress fraction — and does NOT extrapolate outside any sweep.
    #[wasm_bindgen_test]
    fn estimated_azimuth_archive_interpolation_no_extrapolation() {
        let sp = scan(vec![sweep(1, 1000.0, 1010.0)], 1000.0, 1010.0);
        // Halfway through the 10s sweep → 180°.
        let az = sp.estimated_azimuth_at(1005.0).unwrap();
        assert!((az - 180.0).abs() < 1e-3, "got {az}");
        // Start of sweep → 0°.
        assert!((sp.estimated_azimuth_at(1000.0).unwrap() - 0.0).abs() < 1e-3);
        // Outside every sweep → None (no extrapolation in the archive path).
        assert!(sp.estimated_azimuth_at(2000.0).is_none());

        // A zero-duration sweep can't interpolate → None.
        let degenerate = scan(vec![sweep(1, 1000.0, 1000.0)], 1000.0, 1000.0);
        assert!(degenerate.estimated_azimuth_at(1000.0).is_none());
    }

    /// `elevation_index_at` four-tier fallback: containment → next not-yet-ended
    /// → last → None for an empty scan.
    #[wasm_bindgen_test]
    fn elevation_index_fallback_tiers() {
        let sweeps = vec![
            sweep(1, 1000.0, 1010.0),
            sweep(2, 1020.0, 1030.0),
            sweep(3, 1040.0, 1050.0),
        ];
        let sp = scan(sweeps, 1000.0, 1050.0);

        // Tier 1 — containment.
        assert_eq!(sp.elevation_index_at(1025.0), Some(1));
        // Tier 1 boundary (end inclusive) still contains.
        assert_eq!(sp.elevation_index_at(1010.0), Some(0));
        // Tier 2 — in the gap before sweep 2 (1015 < sweep2.end), picks the next
        // not-yet-ended sweep, which is index 1 (its end 1030 > 1015).
        assert_eq!(sp.elevation_index_at(1015.0), Some(1));
        // Before everything → first sweep (next not-yet-ended).
        assert_eq!(sp.elevation_index_at(500.0), Some(0));
        // Tier 3 — past the last sweep's end → last index.
        assert_eq!(sp.elevation_index_at(9999.0), Some(2));

        // Tier 4 — empty scan → None.
        let empty = scan(vec![], 1000.0, 1050.0);
        assert_eq!(empty.elevation_index_at(1000.0), None);
    }

    /// `progress_at` is the volume fraction, clamped to [0,1], and 0.0 for a
    /// zero/negative-duration volume.
    #[wasm_bindgen_test]
    fn progress_at_boundaries_and_clamp() {
        let sp = scan(vec![sweep(1, 1000.0, 1100.0)], 1000.0, 1100.0);
        assert_eq!(sp.progress_at(1000.0), 0.0);
        assert_eq!(sp.progress_at(1100.0), 1.0);
        assert!((sp.progress_at(1050.0) - 0.5).abs() < 1e-6);
        // Clamp below and above.
        assert_eq!(sp.progress_at(900.0), 0.0);
        assert_eq!(sp.progress_at(2000.0), 1.0);

        // Zero-duration volume → 0.0 (no divide-by-zero).
        let degenerate = scan(vec![], 1000.0, 1000.0);
        assert_eq!(degenerate.progress_at(1000.0), 0.0);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn make_sweep(
        elev: u8,
        start: f64,
        end: f64,
        role: ProjectionScanRole,
        status: SweepProjectionStatus,
        timing: SweepTimingProvenance,
    ) -> SweepProjection {
        SweepProjection {
            elevation_number: elev,
            elevation_angle: 0.5 * elev as f32,
            scan_role: role,
            status,
            timing,
            collection_start_secs: start,
            collection_end_secs: end,
            chunks_in_sweep: 0,
            chunks_received: 0,
            radials_received: 0,
            azimuth_rate_dps: 20.0,
            chunks: Vec::new(),
        }
    }

    fn make_scan(sweeps: Vec<SweepProjection>, vs: f64, ve: f64) -> ScanProjection {
        ScanProjection {
            vcp_number: 0,
            vcp_pattern: None,
            roster: crate::core::VolumeElevationRoster::default(),
            in_progress_elevation: None,
            in_progress_radials: None,
            volume_start: vs,
            volume_end: ve,
            sweeps,
            extrapolation: None,
            next_scan_ghost: None,
        }
    }

    // ── SweepProjection predicate getters ───────────────────────────────────

    #[wasm_bindgen_test]
    fn sweep_predicate_getters_match_status_and_timing() {
        let collected = make_sweep(
            1,
            0.0,
            10.0,
            ProjectionScanRole::CurrentInProgress,
            SweepProjectionStatus::CollectedByUs,
            SweepTimingProvenance::Observed,
        );
        assert!(collected.is_complete());
        assert!(!collected.is_in_progress());
        assert!(!collected.is_future());
        assert!(collected.is_observed());

        let in_prog = make_sweep(
            1,
            0.0,
            10.0,
            ProjectionScanRole::CurrentInProgress,
            SweepProjectionStatus::InProgress,
            SweepTimingProvenance::Anchored,
        );
        assert!(!in_prog.is_complete());
        assert!(in_prog.is_in_progress());
        assert!(!in_prog.is_future());
        // Anchored is not Observed.
        assert!(!in_prog.is_observed());

        let future = make_sweep(
            1,
            0.0,
            10.0,
            ProjectionScanRole::NextScan,
            SweepProjectionStatus::FutureExpected,
            SweepTimingProvenance::Projected,
        );
        assert!(!future.is_complete());
        assert!(!future.is_in_progress());
        assert!(future.is_future());
        assert!(!future.is_observed());
    }

    #[wasm_bindgen_test]
    fn sweep_duration_is_end_minus_start() {
        let s = make_sweep(
            1,
            1000.0,
            1037.5,
            ProjectionScanRole::CurrentInProgress,
            SweepProjectionStatus::CollectedByUs,
            SweepTimingProvenance::Observed,
        );
        assert!((s.duration() - 37.5).abs() < 1e-9, "got {}", s.duration());

        // Zero-duration sweep → 0.0.
        let z = make_sweep(
            1,
            500.0,
            500.0,
            ProjectionScanRole::CurrentInProgress,
            SweepProjectionStatus::CollectedByUs,
            SweepTimingProvenance::Observed,
        );
        assert_eq!(z.duration(), 0.0);
    }

    #[wasm_bindgen_test]
    fn availability_maps_each_status_variant() {
        let cases = [
            (
                SweepProjectionStatus::CollectedByUs,
                SweepAvailability::Cached,
            ),
            (
                SweepProjectionStatus::InProgress,
                SweepAvailability::Collecting,
            ),
            (
                SweepProjectionStatus::AvailableNotCollected,
                SweepAvailability::Available,
            ),
            (
                SweepProjectionStatus::FutureExpected,
                SweepAvailability::Projected,
            ),
        ];
        for (status, expected) in cases {
            let s = make_sweep(
                1,
                0.0,
                10.0,
                ProjectionScanRole::CurrentInProgress,
                status,
                SweepTimingProvenance::Estimated,
            );
            assert_eq!(s.availability(), expected);
        }
    }

    // ── ScanProjection::sweep_at ────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn sweep_at_finds_containing_sweep_and_boundaries() {
        let sweeps = vec![
            make_sweep(
                1,
                1000.0,
                1010.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::CollectedByUs,
                SweepTimingProvenance::Observed,
            ),
            make_sweep(
                2,
                1020.0,
                1030.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::InProgress,
                SweepTimingProvenance::Anchored,
            ),
        ];
        let sp = make_scan(sweeps, 1000.0, 1030.0);

        // Inside second sweep.
        assert_eq!(sp.sweep_at(1025.0).unwrap().elevation_number, 2);
        // Start boundary (inclusive) of first sweep.
        assert_eq!(sp.sweep_at(1000.0).unwrap().elevation_number, 1);
        // End boundary (inclusive) of first sweep.
        assert_eq!(sp.sweep_at(1010.0).unwrap().elevation_number, 1);
        // In the gap between sweeps → None.
        assert!(sp.sweep_at(1015.0).is_none());
        // Before everything → None.
        assert!(sp.sweep_at(500.0).is_none());
        // After everything → None.
        assert!(sp.sweep_at(2000.0).is_none());
    }

    #[wasm_bindgen_test]
    fn sweep_at_empty_scan_is_none() {
        let sp = make_scan(vec![], 0.0, 100.0);
        assert!(sp.sweep_at(50.0).is_none());
    }

    // ── ScanProjection::completed_count ─────────────────────────────────────

    #[wasm_bindgen_test]
    fn completed_count_counts_only_collected_by_us() {
        let sweeps = vec![
            make_sweep(
                1,
                0.0,
                10.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::CollectedByUs,
                SweepTimingProvenance::Observed,
            ),
            make_sweep(
                2,
                10.0,
                20.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::CollectedByUs,
                SweepTimingProvenance::Observed,
            ),
            make_sweep(
                3,
                20.0,
                30.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::InProgress,
                SweepTimingProvenance::Anchored,
            ),
            make_sweep(
                4,
                30.0,
                40.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::FutureExpected,
                SweepTimingProvenance::Estimated,
            ),
        ];
        let sp = make_scan(sweeps, 0.0, 40.0);
        assert_eq!(sp.completed_count(), 2);

        // No collected sweeps → 0.
        let none_done = make_scan(
            vec![make_sweep(
                1,
                0.0,
                10.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::FutureExpected,
                SweepTimingProvenance::Estimated,
            )],
            0.0,
            10.0,
        );
        assert_eq!(none_done.completed_count(), 0);
    }

    // ── assemble_live_scan ──────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn assemble_live_scan_splits_current_and_next_into_ghost() {
        let sweeps = vec![
            make_sweep(
                1,
                1000.0,
                1010.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::CollectedByUs,
                SweepTimingProvenance::Observed,
            ),
            make_sweep(
                2,
                1010.0,
                1020.0,
                ProjectionScanRole::CurrentInProgress,
                SweepProjectionStatus::InProgress,
                SweepTimingProvenance::Anchored,
            ),
            make_sweep(
                1,
                1300.0,
                1310.0,
                ProjectionScanRole::NextScan,
                SweepProjectionStatus::FutureExpected,
                SweepTimingProvenance::Projected,
            ),
            make_sweep(
                2,
                1290.0,
                1330.0,
                ProjectionScanRole::NextScan,
                SweepProjectionStatus::FutureExpected,
                SweepTimingProvenance::Projected,
            ),
        ];

        let scan = assemble_live_scan(
            &sweeps,
            212,
            None,
            crate::core::VolumeElevationRoster::default(),
            Some(2),
            Some(7),
            1000.0,
            1020.0,
        );

        // Top-level scan keeps only the CurrentInProgress sweeps.
        assert_eq!(scan.sweeps.len(), 2);
        assert!(scan
            .sweeps
            .iter()
            .all(|s| matches!(s.scan_role, ProjectionScanRole::CurrentInProgress)));
        assert_eq!(scan.vcp_number, 212);
        assert_eq!(scan.in_progress_elevation, Some(2));
        assert_eq!(scan.in_progress_radials, Some(7));
        assert!((scan.volume_start - 1000.0).abs() < 1e-9);
        assert!((scan.volume_end - 1020.0).abs() < 1e-9);
        // No extrapolation is filled at assembly time.
        assert!(scan.extrapolation.is_none());

        // The NextScan sweeps become a ghost with min-start / max-end bounds.
        let ghost = scan.next_scan_ghost.expect("ghost present");
        assert_eq!(ghost.sweeps.len(), 2);
        assert!(ghost
            .sweeps
            .iter()
            .all(|s| matches!(s.scan_role, ProjectionScanRole::NextScan)));
        // min start across {1300, 1290} = 1290; max end across {1310, 1330} = 1330.
        assert!((ghost.volume_start - 1290.0).abs() < 1e-9);
        assert!((ghost.volume_end - 1330.0).abs() < 1e-9);
        assert_eq!(ghost.vcp_number, 212);
        // Ghost has no further ghost and no extrapolation.
        assert!(ghost.next_scan_ghost.is_none());
        assert!(ghost.extrapolation.is_none());
        assert!(ghost.in_progress_elevation.is_none());
        assert!(ghost.in_progress_radials.is_none());
    }

    #[wasm_bindgen_test]
    fn assemble_live_scan_no_next_means_no_ghost() {
        let sweeps = vec![make_sweep(
            1,
            1000.0,
            1010.0,
            ProjectionScanRole::CurrentInProgress,
            SweepProjectionStatus::CollectedByUs,
            SweepTimingProvenance::Observed,
        )];
        let scan = assemble_live_scan(
            &sweeps,
            12,
            None,
            crate::core::VolumeElevationRoster::default(),
            None,
            None,
            1000.0,
            1010.0,
        );
        assert_eq!(scan.sweeps.len(), 1);
        assert!(scan.next_scan_ghost.is_none());
    }

    #[wasm_bindgen_test]
    fn assemble_live_scan_only_next_yields_empty_current_with_ghost() {
        let sweeps = vec![make_sweep(
            3,
            2000.0,
            2050.0,
            ProjectionScanRole::NextScan,
            SweepProjectionStatus::FutureExpected,
            SweepTimingProvenance::Projected,
        )];
        let scan = assemble_live_scan(
            &sweeps,
            35,
            None,
            crate::core::VolumeElevationRoster::default(),
            None,
            None,
            2000.0,
            2050.0,
        );
        // No current sweeps, but a ghost exists.
        assert!(scan.sweeps.is_empty());
        let ghost = scan.next_scan_ghost.expect("ghost present");
        assert_eq!(ghost.sweeps.len(), 1);
        assert!((ghost.volume_start - 2000.0).abs() < 1e-9);
        assert!((ghost.volume_end - 2050.0).abs() < 1e-9);
    }
}
