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
use super::observations::VolumeObservations;
use super::status::{build_sweeps, SweepBuildCtx};
use super::Projection;
use crate::nexrad::projector::Projector;
use crate::nexrad::streaming_filter::StreamingFilter;
use crate::nexrad::timing::{AnchorSource, ChunkTimingStats};
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
}

/// Single owner of every projection input; emits one cached [`Projection`].
/// Constructed and fed via [`super::SharedProjectionEngine`] (see
/// `subsystem::live`).
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
    /// The in-progress volume's observations (roster, VCP, in-progress cut,
    /// per-chunk spans, completed metas), fed directly by the worker pipeline.
    /// The engine owns this — it is no longer copied from `LiveModeState`.
    observed: VolumeObservations,
    /// Available archive scan boundaries, for authoritative next-scan extent.
    archive_boundaries: Vec<crate::nexrad::ScanBoundary>,
    /// Bumped whenever a setter changes an input value; the cache key.
    input_revision: u64,
    /// Memoized output + the inputs it was built from.
    cached: Option<(CacheKey, Projection)>,
}

impl ProjectionEngine {
    pub fn new() -> Self {
        Self {
            projector: Projector::new(),
            inventory: KnownChunkInventory::default(),
            cached_sweeps: CachedSweepSet::default(),
            current_scan_start_secs: None,
            in_progress: None,
            observed: VolumeObservations::default(),
            archive_boundaries: Vec::new(),
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

    /// Stream-side volume boundary, called when the loop ingests a Start
    /// chunk: install the new volume's VCP, clear the collection anchor
    /// (the previous volume's last-radial time no longer applies), bound
    /// inventory memory to the current + next volume, and set the scan
    /// start that anchors the live overlay.
    ///
    /// This is one of the engine's two boundary phases. The other —
    /// [`Self::reset_volume_observations`] — is the *ingest-side* boundary,
    /// intentionally later: the main thread seals the diagnostics record
    /// from the observations before resetting them (seal-before-reset)
    /// once the worker reports the volume complete, or when the session
    /// stops.
    pub fn begin_volume(
        &mut self,
        vcp: volume_coverage_pattern::Message<'static>,
        scan_start_secs: f64,
        keep_inventory_from: VolumeIndex,
    ) {
        self.set_vcp(vcp);
        self.reset_collection_anchor();
        self.retain_inventory_from(keep_inventory_from);
        self.set_current_scan_start_secs(scan_start_secs);
    }

    /// Set the active streaming filter. No-op (no bump) when unchanged.
    pub fn set_filter(&mut self, filter: StreamingFilter) {
        if self.projector.filter() != filter {
            self.projector.set_filter(filter);
            self.bump();
        }
    }

    /// Set the ACTUAL collection-end anchor (parsed radial time) for the
    /// chunk just ingested. No-op when unchanged. The chunk id keys the
    /// COLLECTION-interval stats sample the projector derives from
    /// consecutive anchors.
    pub fn set_collection_anchor(&mut self, chunk_id: &ChunkIdentifier, secs: f64) {
        if self.projector.latest_chunk_collection_end_secs() != Some(secs) {
            self.projector.record_collection_end(chunk_id, secs);
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

    /// Mutable access to the in-progress volume's observations the worker feeds
    /// (the engine owns them). Bumps the input revision — the worker only takes
    /// this handle to record a change.
    pub fn observations_mut(&mut self) -> &mut VolumeObservations {
        self.bump();
        &mut self.observed
    }

    /// Read-only access to the volume observations (status build + diagnostics).
    pub fn observations(&self) -> &VolumeObservations {
        &self.observed
    }

    /// Reset the volume observations at a volume boundary. Bumps.
    pub fn reset_volume_observations(&mut self) {
        self.observed.reset();
        self.bump();
    }

    /// Set the available archive scan boundaries (authoritative next-scan
    /// extent). Replaces; bumps on a length/content change.
    pub fn set_archive_boundaries(&mut self, boundaries: Vec<crate::nexrad::ScanBoundary>) {
        if self.archive_boundaries != boundaries {
            self.archive_boundaries = boundaries;
            self.bump();
        }
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

    /// Refresh the cached cuts recorded for one scan from the volume
    /// observations' completed-sweep metas (drives `CollectedByUs`). Reads the
    /// metas as of now — call before `update_sweep_metas` for this ingest to
    /// preserve the prior-metas semantics. Always bumps.
    pub fn set_cached_sweeps_for_scan(&mut self, scan_start_secs: f64) {
        self.cached_sweeps
            .set_for_scan(scan_start_secs, &self.observed.completed_sweep_metas);
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
    /// anchor and self-anchoring the next volume on the inventory. Recomputed
    /// only when an input changed since the last build for the same anchor +
    /// whole-second `now`. `None` only in a cold state (no VCP/mapper yet).
    pub fn projection(&mut self, anchor: &ChunkIdentifier, now_secs: f64) -> Option<&Projection> {
        self.projection_inner(anchor, now_secs)
    }

    /// The most recently built projection, without rebuilding. Lets the UI read
    /// the latest projection each frame (so re-anchors/listings the loop fed
    /// propagate) without needing the loop's download cursor as an anchor.
    pub fn last_projection(&self) -> Option<&Projection> {
        self.cached.as_ref().map(|(_, p)| p)
    }

    fn projection_inner(&mut self, anchor: &ChunkIdentifier, now_secs: f64) -> Option<&Projection> {
        let key = CacheKey {
            input_revision: self.input_revision,
            anchor_volume: anchor.volume().as_number(),
            anchor_sequence: anchor.sequence(),
            now_bucket_secs: now_secs.floor() as i64,
        };
        let hit = matches!(&self.cached, Some((k, _)) if *k == key);
        if !hit {
            // Self-anchor the next volume on the freshest listed chunk, when
            // known. This pins the cross-volume ghost / target to a real
            // measurement while keeping the volume frame on the cursor's
            // volume (offset 0), so the displayed scan never gets mis-framed.
            let next_vol = anchor.volume().next();
            let next_volume_anchor = match (
                self.inventory.newest_seq_in(next_vol),
                self.inventory.newest_upload_in(next_vol),
            ) {
                (Some(seq), Some(upload)) => Some((seq, upload)),
                _ => None,
            };
            let collection = self.projector.latest_chunk_collection_end_secs();
            let plan = self.projector.build_plan_with_collection(
                anchor,
                now_secs,
                collection,
                next_volume_anchor,
            )?;

            // Whole-second scan start: prefer the explicit input, else the
            // first projected collection time, else `now`.
            let current_scan_start = self.current_scan_start_secs.unwrap_or_else(|| {
                plan.current_volume_chunks
                    .iter()
                    .find_map(|c| c.projected.as_ref().map(|f| f.collection_time_secs))
                    .unwrap_or(now_secs)
            });
            let current_volume = *anchor.volume();
            // Whether a projection was already built — the fallback-durations
            // gate (worker used `frame_projection.is_some()`); once a plan
            // exists the library bounds win and the VCP fallback is unused.
            let had_prior_plan = self.cached.is_some();
            let obs = &self.observed;
            // Derivations the worker used to compute, now produced by the owner.
            let received = obs.received_vec();
            let fallback = obs.fallback_sweep_durations(had_prior_plan);
            let roster = obs.elevation_roster();
            let expected_dur = obs.expected_dur_secs();
            let vcp_number = obs.current_vcp_number.unwrap_or(0);
            // Authoritative next-scan start: the smallest archive boundary start
            // strictly after the current scan, when an archive listing covers it.
            let next_scan_boundary = self
                .archive_boundaries
                .iter()
                .map(|b| b.start as f64)
                .filter(|&s| s > current_scan_start + 1.0)
                .reduce(f64::min);
            // COLLECTION end of the current volume.
            let volume_end = plan
                .current_volume_end_collection_secs
                .unwrap_or(current_scan_start + expected_dur.max(1.0));
            let ctx = SweepBuildCtx {
                current_chunks: &plan.current_volume_chunks,
                next_chunks: plan.next_volume_chunks.as_deref(),
                current_scan_start_secs: current_scan_start,
                current_volume,
                next_volume: current_volume.next(),
                cached: &self.cached_sweeps,
                inventory: &self.inventory,
                in_progress_elevation: self.in_progress.map(|(_, e)| e),
                next_scan_boundary_start_secs: next_scan_boundary,
                expected_count: obs.expected_count(),
                received: &received,
                vcp_number,
                vcp_pattern: obs.current_vcp_pattern.as_ref(),
                vol_start_secs: current_scan_start,
                expected_dur_secs: expected_dur,
                completed_sweep_metas: &obs.completed_sweep_metas,
                chunk_elev_spans: &obs.chunk_elev_spans,
                current_elev_chunks: &obs.current_elev_chunks,
                in_progress_radials: obs.current_in_progress_radials,
                fallback_sweep_durations: &fallback,
            };
            let sweeps = build_sweeps(&ctx);
            let live_scan = super::assemble_live_scan(
                &sweeps,
                vcp_number,
                obs.current_vcp_pattern.clone(),
                roster,
                self.in_progress.map(|(_, e)| e),
                obs.current_in_progress_radials,
                current_scan_start,
                volume_end,
            );
            self.cached = Some((key, Projection::from_parts(plan, Some(live_scan))));
        }
        self.cached.as_ref().map(|(_, p)| p)
    }

    // ── Passthrough reads (still needed by the loop + diagnostics) ──

    pub fn timing_stats(&self) -> &ChunkTimingStats {
        self.projector.timing_stats()
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

    fn chunk(sequence: usize) -> ChunkIdentifier {
        let when = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ChunkIdentifier::new(
            "KDMX".to_string(),
            nexrad_data::aws::realtime::VolumeIndex::new(1),
            when.naive_utc(),
            sequence,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
            Some(when),
        )
    }

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
        eng.set_collection_anchor(&chunk(5), 1000.0);
        assert_eq!(eng.input_revision, 3);
        eng.set_collection_anchor(&chunk(5), 1000.0);
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn cv_chunk(volume: usize, sequence: usize) -> ChunkIdentifier {
        let when = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ChunkIdentifier::new(
            "KDMX".to_string(),
            nexrad_data::aws::realtime::VolumeIndex::new(volume),
            when.naive_utc(),
            sequence,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
            Some(when),
        )
    }

    fn cv_known(
        volume: usize,
        sequence: usize,
        upload: f64,
        ty: nexrad_data::aws::realtime::ChunkType,
    ) -> KnownChunk {
        KnownChunk {
            coord: super::super::inventory::ChunkCoord {
                volume: VolumeIndex::new(volume),
                sequence,
            },
            upload_secs: upload,
            chunk_type: ty,
        }
    }

    #[wasm_bindgen_test]
    fn fresh_engine_is_cold() {
        let eng = ProjectionEngine::new();
        // No inputs fed yet: revision at zero, no collection anchor, no cached
        // projection.
        assert_eq!(eng.input_revision, 0);
        assert_eq!(eng.collection_anchor_secs(), None);
        assert!(eng.last_projection().is_none());
    }

    #[wasm_bindgen_test]
    fn default_matches_new() {
        // Default delegates to new(): same cold state.
        let eng = ProjectionEngine::default();
        assert_eq!(eng.input_revision, 0);
        assert!(eng.last_projection().is_none());
        assert_eq!(eng.collection_anchor_secs(), None);
    }

    #[wasm_bindgen_test]
    fn set_current_scan_start_secs_bumps_only_on_change() {
        let mut eng = ProjectionEngine::new();
        // First set establishes the value → bump.
        eng.set_current_scan_start_secs(1000.0);
        assert_eq!(eng.input_revision, 1);
        // Same value again → no-op.
        eng.set_current_scan_start_secs(1000.0);
        assert_eq!(eng.input_revision, 1);
        // Different value → bump.
        eng.set_current_scan_start_secs(2000.0);
        assert_eq!(eng.input_revision, 2);
    }

    #[wasm_bindgen_test]
    fn set_in_progress_elevation_bumps_only_on_change() {
        let mut eng = ProjectionEngine::new();
        // Setting Some on a fresh engine (was None) → bump.
        eng.set_in_progress_elevation(1000.0, Some(3));
        assert_eq!(eng.input_revision, 1);
        // Identical (scan_start, elevation) → no-op.
        eng.set_in_progress_elevation(1000.0, Some(3));
        assert_eq!(eng.input_revision, 1);
        // Different elevation at same scan → bump.
        eng.set_in_progress_elevation(1000.0, Some(4));
        assert_eq!(eng.input_revision, 2);
        // Different scan start with same elevation → bump.
        eng.set_in_progress_elevation(2000.0, Some(4));
        assert_eq!(eng.input_revision, 3);
        // Clearing to None (was Some) → bump.
        eng.set_in_progress_elevation(2000.0, None);
        assert_eq!(eng.input_revision, 4);
        // Clearing again (already None) → no-op; the scan_start is ignored when
        // the elevation is None because the stored value is None.
        eng.set_in_progress_elevation(9999.0, None);
        assert_eq!(eng.input_revision, 4);
    }

    #[wasm_bindgen_test]
    fn set_archive_boundaries_bumps_only_on_content_change() {
        let mut eng = ProjectionEngine::new();
        let a = vec![crate::nexrad::ScanBoundary {
            start: 100,
            end: 400,
        }];
        // Empty → non-empty: content changed → bump.
        eng.set_archive_boundaries(a.clone());
        assert_eq!(eng.input_revision, 1);
        // Identical content → no-op.
        eng.set_archive_boundaries(a.clone());
        assert_eq!(eng.input_revision, 1);
        // Different content → bump.
        let b = vec![
            crate::nexrad::ScanBoundary {
                start: 100,
                end: 400,
            },
            crate::nexrad::ScanBoundary {
                start: 400,
                end: 700,
            },
        ];
        eng.set_archive_boundaries(b);
        assert_eq!(eng.input_revision, 2);
        // Clearing back to empty (different from current) → bump.
        eng.set_archive_boundaries(Vec::new());
        assert_eq!(eng.input_revision, 3);
        // Empty again → no-op.
        eng.set_archive_boundaries(Vec::new());
        assert_eq!(eng.input_revision, 3);
    }

    #[wasm_bindgen_test]
    fn observe_known_chunk_bumps_only_when_anchor_advances() {
        let mut eng = ProjectionEngine::new();
        // First observation advances the availability anchor → bump.
        eng.observe_known_chunk(cv_known(
            1,
            2,
            100.0,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
        ));
        assert_eq!(eng.input_revision, 1);
        // Strictly newer upload advances → bump.
        eng.observe_known_chunk(cv_known(
            1,
            3,
            150.0,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
        ));
        assert_eq!(eng.input_revision, 2);
        // Equal upload does not advance the anchor → no bump.
        eng.observe_known_chunk(cv_known(
            1,
            4,
            150.0,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
        ));
        assert_eq!(eng.input_revision, 2);
        // Older (recycled-slot) upload does not advance → no bump.
        eng.observe_known_chunk(cv_known(
            1,
            5,
            50.0,
            nexrad_data::aws::realtime::ChunkType::Intermediate,
        ));
        assert_eq!(eng.input_revision, 2);
    }

    #[wasm_bindgen_test]
    fn observe_listing_empty_does_not_bump() {
        let mut eng = ProjectionEngine::new();
        // An empty listing can't advance the anchor → no bump.
        eng.observe_listing(VolumeIndex::new(1), &[]);
        assert_eq!(eng.input_revision, 0);
    }

    #[wasm_bindgen_test]
    fn observe_listing_advances_on_dated_chunk() {
        let mut eng = ProjectionEngine::new();
        // The helper chunk carries an upload date_time → first listing entry
        // advances the anchor → bump.
        eng.observe_listing(VolumeIndex::new(1), &[cv_chunk(1, 1)]);
        assert_eq!(eng.input_revision, 1);
    }

    #[wasm_bindgen_test]
    fn observations_handles_bump_then_reset_bumps() {
        let mut eng = ProjectionEngine::new();
        // Taking the mutable observations handle always bumps (the caller only
        // takes it to record a change).
        let _ = eng.observations_mut();
        assert_eq!(eng.input_revision, 1);
        let _ = eng.observations_mut();
        assert_eq!(eng.input_revision, 2);
        // Resetting the observations always bumps.
        eng.reset_volume_observations();
        assert_eq!(eng.input_revision, 3);
        eng.reset_volume_observations();
        assert_eq!(eng.input_revision, 4);
    }
}
