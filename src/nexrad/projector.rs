//! Projection state and plan construction.
//!
//! Owns the inputs that feed the forward-looking [`StreamingPlan`]: the
//! current volume's VCP, its derived [`ElevationChunkMapper`], rolling
//! [`ChunkTimingStats`], the active [`StreamingFilter`], and the most
//! recently observed chunk collection-end timestamp (the projection
//! anchor). [`Projector::build_plan`] composes these into a plan.
//!
//! Extracted from [`crate::nexrad::live::streaming_state::StreamingState`] so the
//! projection concern stands alone. Today [`StreamingState`] still owns
//! the projector (delegating projection-related methods to it); a later
//! commit moves ownership to the main thread so observations from the
//! worker pipeline can feed projection without an async round-trip.

use super::live::streaming_filter::StreamingFilter;
use super::live::streaming_plan::StreamingPlan;
use super::timing::{
    project_scan_timing_with_next, AnchorSource, ChunkCharacteristics, ChunkTimingStats,
    ElevationChunkMapper, TimingTuning,
};
use chrono::Duration as ChronoDuration;
use nexrad_data::aws::realtime::{ChunkIdentifier, ChunkType};
use nexrad_decode::messages::volume_coverage_pattern;

/// Single-source-of-truth input vocabulary for the projector.
///
/// Observations originate on the main thread (worker ingest results, UI
/// signals) and are enqueued via [`super::RealtimeChannel`] for the
/// streaming loop to drain and apply to its [`Projector`]. Adding a new
/// observation kind is one enum variant + one match arm in the drain
/// dispatch — no new pending-field-on-state and no new method on the
/// channel needed.
///
/// Filter changes are NOT a `ProjectorObservation`: they have additional
/// sleep-interruption semantics (`filter_epoch`) and a separate
/// re-entry path in the loop, so they keep their own dedicated channel.
#[derive(Clone, Copy, Debug)]
pub enum ProjectorObservation {
    /// ACTUAL category: collection-end time of the most recently
    /// ingested chunk (Unix seconds, sub-second precision). Anchors
    /// projected COLLECTION times for future chunks.
    CollectionEndSecs(f64),
    /// Empirical S3 upload − ACTUAL chunk collection time (seconds) for
    /// the chunk just ingested. Folded into `ChunkTimingStats` so future
    /// projections use a median lag rather than a default.
    AvailabilityLagSecs(f64),
}

/// All state needed to project future chunks for the in-progress volume.
///
/// Fed by chunk-arrival observations (collection-end times, inter-chunk
/// durations, availability lags) and the current volume's VCP. Produces
/// a [`StreamingPlan`] anchored at a caller-supplied chunk.
#[derive(Debug, Default)]
pub struct Projector {
    vcp: Option<volume_coverage_pattern::Message<'static>>,
    elevation_mapper: Option<ElevationChunkMapper>,
    timing_stats: ChunkTimingStats,
    /// ACTUAL category: collection-end time (Unix seconds) of the most
    /// recently ingested chunk, parsed by the worker as the latest radial
    /// timestamp in the chunk. Reset on volume boundary. Used as the
    /// anchor for projected COLLECTION times — the projector adds
    /// cumulative inter-chunk physics intervals to this to place future
    /// chunks on the timeline.
    latest_chunk_collection_end_secs: Option<f64>,
    filter: StreamingFilter,
    /// Estimation tuning knobs (default values; the accuracy-tuning work
    /// varies these — see `TimingTuning`).
    tuning: TimingTuning,
    /// Monotonically-incrementing per-projector revision counter, bumped
    /// on every [`Projector::build_plan`] call. The bumped value is
    /// stamped onto the plan's `revision` field so consumers can
    /// attribute predictions to a specific plan version (diagnostics)
    /// and short-circuit redraws when the plan hasn't changed (UI).
    next_revision: u64,
}

impl Projector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (or replace) the VCP for the in-progress volume. The
    /// derived [`ElevationChunkMapper`] is rebuilt automatically. Called
    /// on every Start-chunk arrival.
    pub fn set_vcp(&mut self, vcp: volume_coverage_pattern::Message<'static>) {
        self.elevation_mapper = Some(ElevationChunkMapper::new(&vcp));
        self.vcp = Some(vcp);
    }

    /// Build a [`StreamingPlan`] anchored at `anchor_chunk` (the most
    /// recently observed chunk). `now_secs` is recorded on the plan as
    /// `built_at_secs` for staleness reasoning. Bumps the internal
    /// revision counter and stamps it onto the plan's `revision` field.
    ///
    /// Returns `None` only when the projector is in a cold state (no VCP
    /// / no mapper yet); the revision counter is NOT bumped in that case.
    /// Build a [`StreamingPlan`] anchored at `anchor_chunk`.
    ///
    /// `collection_override` is the ACTUAL collection-end anchor to use (the
    /// engine passes its stored `latest_chunk_collection_end_secs`); `None`
    /// falls back to the `UploadMinusMedian`/`UploadMinusDefault` lag estimate.
    /// `next_volume_anchor` is an optional freshly-listed next-volume chunk
    /// `(sequence, upload_secs)` that pins the next-volume (offset 1) timeline
    /// to a real measurement.
    pub fn build_plan_with_collection(
        &mut self,
        anchor_chunk: &ChunkIdentifier,
        now_secs: f64,
        collection_override: Option<f64>,
        next_volume_anchor: Option<(usize, f64)>,
    ) -> Option<StreamingPlan> {
        let mapper = self.elevation_mapper.as_ref()?;
        let vcp = self.vcp.as_ref()?;

        // Extend into the next volume iff the active filter has no
        // remaining match in this volume. `has_remaining_match` returns
        // `true` for `StreamingFilter::All` whenever chunks remain, so
        // the All branch naturally never triggers an extension.
        let include_next_volume = !mapper.has_remaining_match(self.filter, anchor_chunk.sequence());

        let projection = project_scan_timing_with_next(
            anchor_chunk,
            collection_override,
            vcp,
            mapper,
            Some(&self.timing_stats),
            include_next_volume,
            next_volume_anchor,
            &self.tuning,
        )?;

        self.next_revision = self.next_revision.wrapping_add(1);
        let chunk_meta = mapper.all_chunk_metadata();
        Some(StreamingPlan::from_projection(
            projection,
            self.filter,
            chunk_meta,
            now_secs,
            self.next_revision,
        ))
    }

    pub fn set_filter(&mut self, filter: StreamingFilter) {
        self.filter = filter;
    }

    /// The active streaming filter. Used by `ProjectionEngine` to skip a
    /// cache-busting revision bump when `set_filter` is a no-op.
    pub fn filter(&self) -> StreamingFilter {
        self.filter
    }

    /// Record the collection-end time (parsed radial timestamp) of the
    /// chunk the loop just ingested. When a previous collection end is
    /// known for this volume, the delta is attached to the chunk's stats
    /// bucket as a COLLECTION-domain interval sample — the historical term
    /// of the interval-estimate blend. The anchor resets at volume
    /// boundaries, so no sample ever spans two volumes.
    pub fn record_collection_end(&mut self, chunk_id: &ChunkIdentifier, secs: f64) {
        if let Some(prev) = self.latest_chunk_collection_end_secs {
            let delta = secs - prev;
            if delta > 0.0 {
                if let Some(characteristics) = self.characteristics_for_sequence(chunk_id) {
                    self.timing_stats.attach_collection_interval(
                        &characteristics,
                        ChronoDuration::milliseconds((delta * 1000.0) as i64),
                    );
                }
            }
        }
        self.latest_chunk_collection_end_secs = Some(secs);
    }

    /// Reset the collection-end anchor — used at volume boundaries when
    /// the previous volume's last-radial time no longer applies.
    pub fn reset_collection_anchor(&mut self) {
        self.latest_chunk_collection_end_secs = None;
    }

    pub fn latest_chunk_collection_end_secs(&self) -> Option<f64> {
        self.latest_chunk_collection_end_secs
    }

    /// Attach an observed availability-lag (S3 upload − ACTUAL collection
    /// time) sample to the most recent timing stat recorded for `current`.
    pub fn record_availability_lag_for(&mut self, current: &ChunkIdentifier, lag_secs: f64) {
        let Some(characteristics) = self.characteristics_for_sequence(current) else {
            return;
        };
        self.timing_stats.attach_availability_lag(
            &characteristics,
            ChronoDuration::milliseconds((lag_secs * 1000.0) as i64),
        );
    }

    /// Anchor source the plan would use right now — `ObservedCollection`
    /// when an ACTUAL collection-end time is available, `UploadMinusMedian`
    /// when only the rolling median S3 lag is, or `UploadMinusDefault`
    /// otherwise. Captured per-chunk so the diagnostics modal can spot
    /// degraded projections.
    pub fn current_anchor_source(&self) -> AnchorSource {
        if self.latest_chunk_collection_end_secs.is_some() {
            AnchorSource::ObservedCollection
        } else if self.timing_stats.median_availability_lag_secs().is_some() {
            AnchorSource::UploadMinusMedian
        } else {
            AnchorSource::UploadMinusDefault
        }
    }

    /// Replace the rolling timing statistics with a previously-persisted
    /// snapshot. Called once on stream start when localStorage has cached
    /// stats for the site.
    pub fn preload_timing_stats(&mut self, stats: ChunkTimingStats) {
        self.timing_stats = stats;
    }

    pub fn timing_stats(&self) -> &ChunkTimingStats {
        &self.timing_stats
    }

    /// Record an inter-chunk arrival sample for `chunk_id`'s bucket.
    /// Called by the streaming loop after each successful download with
    /// the wall-clock duration since the previous chunk landed.
    pub fn record_inter_chunk_duration(
        &mut self,
        chunk_id: &ChunkIdentifier,
        duration: ChronoDuration,
        attempts: usize,
    ) {
        if let Some(characteristics) = self.characteristics_for_sequence(chunk_id) {
            self.timing_stats
                .add_timing(characteristics, duration, None, attempts);
        }
    }

    fn characteristics_for_sequence(
        &self,
        chunk_id: &ChunkIdentifier,
    ) -> Option<ChunkCharacteristics> {
        let vcp = self.vcp.as_ref()?;
        let mapper = self.elevation_mapper.as_ref()?;
        let elevation = mapper
            .get_sequence_elevation_number(chunk_id.sequence())
            .and_then(|n| vcp.elevations().get(n - 1))?;
        let is_first_in_sweep = mapper
            .get_chunk_metadata(chunk_id.sequence())
            .is_some_and(|m| m.is_first_in_sweep());
        Some(ChunkCharacteristics {
            // Normalize to `Intermediate`: the bucket key is shared with the
            // READ path (`interval_estimate::chunk_characteristics`), which
            // always builds the key with `chunk_type: Intermediate`. Using the
            // chunk identifier's own type here would bucket the volume's final
            // (radar-data-bearing) chunk under `End` — see
            // `streaming_state::try_next` setting `ChunkType::End` for the last
            // sequence — and that bucket could never be read back, so the
            // final hop's collection/attempts history would be silently
            // discarded forever.
            chunk_type: ChunkType::Intermediate,
            waveform_type: elevation.waveform_type(),
            channel_configuration: elevation.channel_configuration(),
            is_first_in_sweep,
        })
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use nexrad_data::aws::realtime::{ChunkType, VolumeIndex};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn chunk(sequence: usize) -> ChunkIdentifier {
        let when = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        ChunkIdentifier::new(
            "KDMX".to_string(),
            VolumeIndex::new(1),
            when.naive_utc(),
            sequence,
            ChunkType::Intermediate,
            Some(when),
        )
    }

    #[wasm_bindgen_test]
    fn new_is_cold_with_all_filter() {
        let p = Projector::new();
        // Default filter is `All`.
        assert_eq!(p.filter(), StreamingFilter::All);
        // No collection anchor recorded yet.
        assert_eq!(p.latest_chunk_collection_end_secs(), None);
    }

    #[wasm_bindgen_test]
    fn default_matches_new() {
        let p = Projector::default();
        assert_eq!(p.filter(), StreamingFilter::All);
        assert_eq!(p.latest_chunk_collection_end_secs(), None);
    }

    #[wasm_bindgen_test]
    fn set_filter_round_trips() {
        let mut p = Projector::new();
        p.set_filter(StreamingFilter::Elevation(3));
        assert_eq!(p.filter(), StreamingFilter::Elevation(3));
        p.set_filter(StreamingFilter::All);
        assert_eq!(p.filter(), StreamingFilter::All);
    }

    #[wasm_bindgen_test]
    fn record_collection_end_sets_anchor() {
        let mut p = Projector::new();
        // No VCP installed → characteristics_for_sequence returns None, so no
        // stats sample is attached, but the anchor must still update.
        p.record_collection_end(&chunk(5), 1234.5);
        let got = p.latest_chunk_collection_end_secs().unwrap();
        assert!((got - 1234.5).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn record_collection_end_overwrites_anchor() {
        let mut p = Projector::new();
        p.record_collection_end(&chunk(5), 100.0);
        p.record_collection_end(&chunk(6), 250.0);
        let got = p.latest_chunk_collection_end_secs().unwrap();
        assert!((got - 250.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn reset_collection_anchor_clears() {
        let mut p = Projector::new();
        p.record_collection_end(&chunk(5), 999.0);
        assert!(p.latest_chunk_collection_end_secs().is_some());
        p.reset_collection_anchor();
        assert_eq!(p.latest_chunk_collection_end_secs(), None);
    }

    #[wasm_bindgen_test]
    fn anchor_source_cold_is_upload_minus_default() {
        // Cold: no collection anchor, empty timing stats → no median lag.
        let p = Projector::new();
        assert_eq!(p.current_anchor_source(), AnchorSource::UploadMinusDefault);
    }

    #[wasm_bindgen_test]
    fn anchor_source_with_anchor_is_observed_collection() {
        let mut p = Projector::new();
        p.record_collection_end(&chunk(2), 500.0);
        assert_eq!(p.current_anchor_source(), AnchorSource::ObservedCollection);
        // Clearing the anchor returns to the cold fallback.
        p.reset_collection_anchor();
        assert_eq!(p.current_anchor_source(), AnchorSource::UploadMinusDefault);
    }

    #[wasm_bindgen_test]
    fn build_plan_cold_returns_none() {
        // No VCP / no elevation mapper → projector is cold → no plan.
        let mut p = Projector::new();
        let anchor = chunk(1);
        assert!(p
            .build_plan_with_collection(&anchor, 1000.0, None, None)
            .is_none());
        // A second call also stays None; the revision counter must not have
        // been bumped into a state that produces a plan.
        assert!(p
            .build_plan_with_collection(&anchor, 2000.0, Some(900.0), Some((2, 1500.0)))
            .is_none());
    }

    #[wasm_bindgen_test]
    fn preload_timing_stats_is_observable_via_getter() {
        let mut p = Projector::new();
        // A fresh, empty stats snapshot has no median availability lag.
        let stats = ChunkTimingStats::new();
        p.preload_timing_stats(stats);
        assert_eq!(p.timing_stats().median_availability_lag_secs(), None);
        // With no collection anchor and no median lag, anchor source is default.
        assert_eq!(p.current_anchor_source(), AnchorSource::UploadMinusDefault);
    }

    #[wasm_bindgen_test]
    fn record_observations_without_vcp_are_noops() {
        // Without a VCP, characteristics_for_sequence returns None, so these
        // recorders must not panic and must not disturb the collection anchor.
        let mut p = Projector::new();
        p.record_availability_lag_for(&chunk(4), 12.0);
        p.record_inter_chunk_duration(&chunk(4), ChronoDuration::seconds(7), 1);
        assert_eq!(p.latest_chunk_collection_end_secs(), None);
        assert_eq!(p.timing_stats().median_availability_lag_secs(), None);
        // Filter is untouched.
        assert_eq!(p.filter(), StreamingFilter::All);
    }
}
