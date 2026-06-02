//! The projection engine: single owner of all projection inputs.
//!
//! `ProjectionEngine` wraps the existing [`Projector`] math kernel and adds an
//! explicit input-setter surface plus a revision-keyed output cache. Inputs are
//! pushed in via setters (each bumps an `input_revision` when it actually
//! changes a value); the output [`Projection`] is recomputed lazily only when an
//! input changed since the last build for the same anchor + whole-second `now`.
//!
//! Phase 1 covers the realtime inputs the `Projector` already had. Later phases
//! attach the known-available-chunks inventory, cached sweeps, and archive
//! boundaries as additional inputs, and flip ownership to a shared
//! `Rc<RefCell<ProjectionEngine>>` on the main thread.

use super::cached_sweeps::CachedSweepSet;
use super::inventory::{KnownChunk, KnownChunkInventory};
use super::status::{build_sweeps, SweepBuildCtx};
use super::Projection;
use crate::data::CachedSweep;
use crate::nexrad::projector::Projector;
use crate::nexrad::streaming_filter::StreamingFilter;
use crate::nexrad::timing::{AnchorSource, ChunkMetadata, ChunkTimingStats};
use chrono::Duration as ChronoDuration;
use nexrad_data::aws::realtime::{ChunkIdentifier, VolumeIndex};
use nexrad_decode::messages::volume_coverage_pattern;

/// Identifies the inputs a cached projection was built from, so a later request
/// with identical inputs can reuse it instead of rebuilding.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CacheKey {
    input_revision: u64,
    anchor_volume: usize,
    anchor_sequence: usize,
    now_bucket_secs: i64,
    /// Whether the collection anchor was overridden to `None` (re-anchor probe
    /// path) vs. using the engine's stored collection anchor.
    collection_override_none: bool,
}

/// Single owner of every projection input; emits one cached [`Projection`].
#[allow(dead_code)] // Constructed and consumed at the Phase 4 ownership flip.
pub struct ProjectionEngine {
    /// The math kernel: VCP, mapper, rolling stats, collection anchor, filter.
    projector: Projector,
    /// Known-available chunks (arrivals + periodic listings). Supplies the
    /// availability anchor and per-sweep availability status.
    inventory: KnownChunkInventory,
    /// Sweeps cached locally (possibly sparse) — drives `CollectedByUs`.
    cached_sweeps: CachedSweepSet,
    /// Whole-second scan-start of the current in-progress volume (cache +
    /// status key). `None` before the first Start chunk.
    current_scan_start_secs: Option<f64>,
    /// `(scan_start, elevation)` currently being received — drives `InProgress`.
    in_progress: Option<(f64, u8)>,
    /// Bumped whenever a setter changes an input value; the cache key.
    input_revision: u64,
    /// Memoized output + the inputs it was built from.
    cached: Option<(CacheKey, Projection)>,
}

#[allow(dead_code)] // Setters/readers come online as the loop + consumers migrate.
impl ProjectionEngine {
    pub fn new() -> Self {
        Self {
            projector: Projector::new(),
            inventory: KnownChunkInventory::default(),
            cached_sweeps: CachedSweepSet::default(),
            current_scan_start_secs: None,
            in_progress: None,
            input_revision: 0,
            cached: None,
        }
    }

    // ── Input setters (bump input_revision iff a value actually changed) ──

    /// Install (or replace) the VCP for the in-progress volume. Always bumps —
    /// a new Start chunk means a new (or re-derived) volume structure.
    pub fn set_vcp(&mut self, vcp: volume_coverage_pattern::Message<'static>) {
        self.projector.set_vcp(vcp);
        self.bump();
    }

    /// Set the active streaming filter. No-op (no bump) when unchanged.
    pub fn set_filter(&mut self, filter: StreamingFilter) {
        if self.projector.filter() != filter {
            self.projector.set_filter(filter);
            self.bump();
        }
    }

    /// Set the ACTUAL collection-end anchor (parsed radial time). No-op when
    /// unchanged.
    pub fn set_collection_anchor_secs(&mut self, secs: f64) {
        if self.projector.latest_chunk_collection_end_secs() != Some(secs) {
            self.projector.record_chunk_collection_end_secs(secs);
            self.bump();
        }
    }

    /// Clear the collection anchor at a volume boundary. No-op when already
    /// cleared.
    pub fn reset_collection_anchor(&mut self) {
        if self.projector.latest_chunk_collection_end_secs().is_some() {
            self.projector.reset_collection_anchor();
            self.bump();
        }
    }

    /// Replace rolling timing stats with a persisted snapshot (warm start).
    pub fn preload_timing_stats(&mut self, stats: ChunkTimingStats) {
        self.projector.preload_timing_stats(stats);
        self.bump();
    }

    /// Record an inter-chunk arrival sample (feeds the blend + retry budget).
    pub fn record_inter_chunk_duration(
        &mut self,
        chunk_id: &ChunkIdentifier,
        duration: ChronoDuration,
        attempts: usize,
    ) {
        self.projector
            .record_inter_chunk_duration(chunk_id, duration, attempts);
        self.bump();
    }

    /// Attach an availability-lag sample (S3 upload − collection) for a chunk.
    pub fn record_availability_lag_for(&mut self, chunk_id: &ChunkIdentifier, lag_secs: f64) {
        self.projector
            .record_availability_lag_for(chunk_id, lag_secs);
        self.bump();
    }

    /// Record a single known-available chunk (an arrival, or one listing entry).
    /// Bumps the input revision only when the availability anchor advanced.
    pub fn observe_known_chunk(&mut self, chunk: KnownChunk) {
        if self.inventory.observe(chunk) {
            self.bump();
        }
    }

    /// Merge a full S3 listing for one volume (periodic probe). Bumps the input
    /// revision only when the availability anchor advanced.
    pub fn observe_listing(&mut self, volume: VolumeIndex, listed: &[ChunkIdentifier]) {
        if self.inventory.observe_listing(volume, listed) {
            self.bump();
        }
    }

    /// Drop inventory volumes outside the current + next window (call on
    /// rollover to bound memory).
    pub fn retain_inventory_from(&mut self, keep: VolumeIndex) {
        self.inventory.retain_from(keep);
    }

    /// The known-available-chunks inventory (read by status derivation).
    pub fn inventory(&self) -> &KnownChunkInventory {
        &self.inventory
    }

    /// Replace the cached cuts recorded for one scan (drives `CollectedByUs`).
    /// Always bumps — the cuts list is the changed input.
    pub fn set_cached_sweeps_for_scan(&mut self, scan_start_secs: f64, sweeps: &[CachedSweep]) {
        self.cached_sweeps.set_for_scan(scan_start_secs, sweeps);
        self.bump();
    }

    /// Set the current in-progress volume's whole-second scan start. No-op when
    /// unchanged.
    pub fn set_current_scan_start_secs(&mut self, secs: f64) {
        if self.current_scan_start_secs != Some(secs) {
            self.current_scan_start_secs = Some(secs);
            self.bump();
        }
    }

    /// Set (or clear) the elevation currently being received for a scan. No-op
    /// when unchanged.
    pub fn set_in_progress_elevation(
        &mut self,
        scan_start_secs: f64,
        elevation_number: Option<u8>,
    ) {
        let next = elevation_number.map(|e| (scan_start_secs, e));
        if self.in_progress != next {
            self.in_progress = next;
            self.bump();
        }
    }

    // ── Output ──

    /// Projection anchored at `anchor`, using the engine's stored collection
    /// anchor. Recomputed only when an input changed since the last build for
    /// the same anchor + whole-second `now`. `None` only in a cold state
    /// (no VCP/mapper yet).
    pub fn projection(&mut self, anchor: &ChunkIdentifier, now_secs: f64) -> Option<&Projection> {
        self.projection_inner(anchor, now_secs, false)
    }

    /// Like [`Self::projection`] but forces the collection anchor to `None`
    /// (used when re-anchoring on a listed-but-not-downloaded chunk, which has
    /// no parsed collection time and may live in a different volume).
    pub fn projection_from_anchor(
        &mut self,
        anchor: &ChunkIdentifier,
        now_secs: f64,
    ) -> Option<&Projection> {
        self.projection_inner(anchor, now_secs, true)
    }

    fn projection_inner(
        &mut self,
        anchor: &ChunkIdentifier,
        now_secs: f64,
        collection_override_none: bool,
    ) -> Option<&Projection> {
        let key = CacheKey {
            input_revision: self.input_revision,
            anchor_volume: anchor.volume().as_number(),
            anchor_sequence: anchor.sequence(),
            now_bucket_secs: now_secs.floor() as i64,
            collection_override_none,
        };
        let hit = matches!(&self.cached, Some((k, _)) if *k == key);
        if !hit {
            let plan = if collection_override_none {
                self.projector
                    .build_plan_with_collection(anchor, now_secs, None)?
            } else {
                self.projector.build_plan(anchor, now_secs)?
            };

            // Whole-second scan start: prefer the explicit input, else the
            // first projected collection time, else `now`.
            let current_scan_start = self.current_scan_start_secs.unwrap_or_else(|| {
                plan.current_volume_chunks
                    .iter()
                    .find_map(|c| c.forecast.as_ref().map(|f| f.collection_time_secs))
                    .unwrap_or(now_secs)
            });
            let current_volume = *anchor.volume();
            let ctx = SweepBuildCtx {
                current_chunks: &plan.current_volume_chunks,
                next_chunks: plan.next_volume_chunks.as_deref(),
                current_scan_start_secs: current_scan_start,
                next_scan_start_secs: None,
                current_volume,
                next_volume: current_volume.next(),
                cached: &self.cached_sweeps,
                inventory: &self.inventory,
                in_progress_elevation: self.in_progress.map(|(_, e)| e),
                next_scan_boundary_start_secs: None,
            };
            let sweeps = build_sweeps(&ctx);
            self.cached = Some((key, Projection::from_plan_with_sweeps(plan, sweeps)));
        }
        self.cached.as_ref().map(|(_, p)| p)
    }

    // ── Passthrough reads (still needed by the loop + diagnostics) ──

    pub fn timing_stats(&self) -> &ChunkTimingStats {
        self.projector.timing_stats()
    }

    pub fn chunk_metadata(&self, sequence: usize) -> Option<&ChunkMetadata> {
        self.projector.chunk_metadata(sequence)
    }

    pub fn mapper_matching_sequences_in_range(
        &self,
        lower: usize,
        upper: usize,
        predicate: impl FnMut(Option<usize>) -> bool,
    ) -> Vec<usize> {
        self.projector
            .mapper_matching_sequences_in_range(lower, upper, predicate)
    }

    pub fn current_anchor_source(&self) -> AnchorSource {
        self.projector.current_anchor_source()
    }

    pub fn collection_anchor_secs(&self) -> Option<f64> {
        self.projector.latest_chunk_collection_end_secs()
    }

    fn bump(&mut self) {
        self.input_revision = self.input_revision.wrapping_add(1);
    }
}

impl Default for ProjectionEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn setters_bump_revision_only_on_change() {
        let mut eng = ProjectionEngine::new();
        assert_eq!(eng.input_revision, 0);

        // First filter set changes the value → bump.
        eng.set_filter(StreamingFilter::Elevation(2));
        assert_eq!(eng.input_revision, 1);
        // Same filter again → no bump.
        eng.set_filter(StreamingFilter::Elevation(2));
        assert_eq!(eng.input_revision, 1);
        // Different filter → bump.
        eng.set_filter(StreamingFilter::All);
        assert_eq!(eng.input_revision, 2);

        // Collection anchor: first set bumps, repeat does not.
        eng.set_collection_anchor_secs(1000.0);
        assert_eq!(eng.input_revision, 3);
        eng.set_collection_anchor_secs(1000.0);
        assert_eq!(eng.input_revision, 3);
        // Reset clears it → bump; reset again is a no-op.
        eng.reset_collection_anchor();
        assert_eq!(eng.input_revision, 4);
        eng.reset_collection_anchor();
        assert_eq!(eng.input_revision, 4);
    }

    #[wasm_bindgen_test]
    fn cold_engine_has_no_projection() {
        // No VCP installed → projector is cold → no projection.
        let mut eng = ProjectionEngine::new();
        let when = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let anchor = ChunkIdentifier::new(
            "KDMX".to_string(),
            nexrad_data::aws::realtime::VolumeIndex::new(1),
            when.naive_utc(),
            1,
            nexrad_data::aws::realtime::ChunkType::Start,
            Some(when),
        );
        assert!(eng.projection(&anchor, 1000.0).is_none());
    }
}
