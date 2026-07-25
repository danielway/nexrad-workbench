use super::{
    chunk_characteristics, estimate_interval, ChunkCharacteristics, ChunkTimingStats,
    ElevationChunkMapper, PhysicsBreakdown, TimingTuning,
};
use chrono::{DateTime, Duration, Utc};
use nexrad_data::aws::realtime::{ChunkIdentifier, ChunkType, VolumeIndex};
use nexrad_decode::messages::volume_coverage_pattern;

/// Which branch [`project_scan_timing`] used to anchor the collection axis.
/// Surfaced on per-chunk diagnostics so we can tell whether a projection was
/// based on a real radial-derived anchor or had to fall back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchorSource {
    /// Used the caller-supplied ACTUAL collection time (parsed from a radial
    /// header). Best case — projections are anchored on real data.
    ObservedCollection,
    /// No collection time available; estimated as `upload − median lag` from
    /// `ChunkTimingStats`. Reasonable once stats have warmed up.
    UploadMinusMedian,
    /// No collection time and no lag stats; fell back to
    /// `upload − DEFAULT_AVAILABILITY_LAG_SECS`. Cold start only — projections
    /// will carry the largest uncertainty.
    UploadMinusDefault,
}

impl AnchorSource {
    pub(crate) fn short(&self) -> &'static str {
        match self {
            AnchorSource::ObservedCollection => "obs",
            AnchorSource::UploadMinusMedian => "median",
            AnchorSource::UploadMinusDefault => "default",
        }
    }
}

/// A projected timeline for all remaining chunks in a volume scan.
///
/// Carries two parallel time axes for each chunk:
///   - ACTUAL + physics → projected COLLECTION time (when the radar emits/receives).
///   - Collection + lag → projected AVAILABILITY time (when the chunk appears on S3).
///
/// The collection axis anchors on the parsed volume header time when the caller
/// supplies one; otherwise it falls back to a lag-adjusted S3 upload time.
#[derive(Debug, Clone)]
pub(crate) struct ScanTimingProjection {
    /// The sequence number of the anchor chunk (the last observed chunk).
    anchor_sequence: usize,
    /// AVAILABILITY category: S3-upload time of the anchor chunk (or current
    /// time as a fallback). Used as the base for projecting future chunks'
    /// availability.
    anchor_available_at: DateTime<Utc>,
    /// COLLECTION category: parsed radial-header collection time of the
    /// current volume when available, else `anchor_available_at -
    /// observed_anchor_lag_secs` as an estimate.
    anchor_collection_time_secs: f64,
    /// AVAILABILITY-lag category: empirical delay between ACTUAL collection
    /// time of the anchor and its S3 upload time. `None` when the header
    /// time is unavailable (falls back to `DEFAULT_AVAILABILITY_LAG_SECS`).
    observed_anchor_lag_secs: Option<f64>,
    /// Which branch the anchor came from — surfaced in diagnostics so we
    /// can tell when a projection is degraded by a fallback anchor.
    anchor_source: AnchorSource,
    /// Projected timing for each future chunk, in sequence order.
    chunks: Vec<ChunkProjection>,
    /// AVAILABILITY category: projected time the final chunk becomes
    /// available in S3.
    volume_end_available_at: DateTime<Utc>,
    /// Projected total remaining duration from anchor to volume end.
    remaining_duration: Duration,
}

impl ScanTimingProjection {
    /// The sequence number of the anchor chunk this projection is relative to.
    pub(crate) fn anchor_sequence(&self) -> usize {
        self.anchor_sequence
    }

    /// AVAILABILITY category: the S3-upload time of the anchor chunk (or
    /// current time as a fallback). Physics intervals added to this yield
    /// projected availability times for future chunks.
    pub(crate) fn anchor_available_at(&self) -> DateTime<Utc> {
        self.anchor_available_at
    }

    /// COLLECTION category: Unix-seconds collection time of the anchor
    /// chunk — parsed from a radial header when available, otherwise
    /// estimated as `anchor_available_at - observed_anchor_lag`.
    #[allow(dead_code)] // Consumed by debug UI in a later commit.
    pub(crate) fn anchor_collection_time_secs(&self) -> f64 {
        self.anchor_collection_time_secs
    }

    /// AVAILABILITY-lag category: observed anchor lag, if the caller
    /// provided an ACTUAL collection anchor. `None` signals the default
    /// fallback lag was used and projections carry more uncertainty.
    #[allow(dead_code)] // Consumed by debug UI in a later commit.
    pub(crate) fn observed_anchor_lag_secs(&self) -> Option<f64> {
        self.observed_anchor_lag_secs
    }

    /// Which branch the anchor came from. Used by the diagnostics modal to
    /// flag projections built on a fallback anchor.
    pub(crate) fn anchor_source(&self) -> AnchorSource {
        self.anchor_source
    }

    /// Projected timing for each future chunk, in sequence order.
    pub(crate) fn chunks(&self) -> &[ChunkProjection] {
        &self.chunks
    }

    /// AVAILABILITY category: projected time the final chunk becomes
    /// available in S3.
    pub(crate) fn volume_end_available_at(&self) -> DateTime<Utc> {
        self.volume_end_available_at
    }

    /// Projected remaining duration from anchor to volume end.
    pub(crate) fn remaining_duration(&self) -> Duration {
        self.remaining_duration
    }
}

/// Projection for a single future chunk.
#[derive(Debug, Clone)]
pub(crate) struct ChunkProjection {
    /// The chunk's sequence number.
    sequence: usize,
    /// The elevation number (1-based), or None for the Start chunk.
    elevation_number: Option<usize>,
    /// Elevation angle in degrees (0.0 for the Start chunk).
    elevation_angle_deg: f64,
    /// COLLECTION category: projected Unix-seconds time the radar physically
    /// emits/receives for this chunk. Drives timeline placeholders for
    /// future sweeps and chunks.
    projected_collection_time_secs: f64,
    /// AVAILABILITY category: projected time this chunk becomes available
    /// in S3 (`collection_at + lag`). Drives "next in Xs" countdown labels.
    projected_available_at: DateTime<Utc>,
    /// POLL category: projected time the scheduler will fire its first
    /// download poll (`available_at + retry_budget + POLL_BIAS`). Surfaced
    /// so a debug overlay can show poll fire vs. expected availability
    /// without re-deriving the math.
    projected_poll_at: DateTime<Utc>,
    /// Duration from the anchor to this chunk's projected availability. Kept
    /// in lock-step with `projected_available_at` when a next-volume anchor
    /// shifts the offset-1 timeline (see `apply_next_volume_anchor`), so
    /// `offset_from_anchor == projected_available_at − anchor` always holds.
    offset_from_anchor: Duration,
    /// Duration from the previous chunk to this chunk, as estimated by the
    /// chained physics model. NOT adjusted by a next-volume anchor shift, so
    /// for the first offset-1 chunk this reflects the pre-anchor inter-volume
    /// estimate rather than the shifted gap; consumers needing the realized
    /// gap should difference consecutive `projected_collection_time_secs`.
    interval_from_previous: Duration,
    /// Whether this chunk starts a new sweep (useful for UI grouping).
    starts_new_sweep: bool,
    /// Which volume this projection belongs to, relative to the anchor:
    /// 0 = current volume, 1 = next volume (only emitted by
    /// [`project_scan_timing_with_next`] when chained projection is requested).
    /// `sequence` is NOT unique on its own when `volume_offset > 0`; key by
    /// `(volume_offset, sequence)` when uniqueness is required.
    volume_offset: u8,
    /// Physics decomposition for the hop into this chunk (azimuth gap,
    /// inter-sweep transition, inter-volume gap). Surfaced so the
    /// diagnostics modal can attribute a prediction error to a specific
    /// component without re-deriving the math.
    physics_breakdown: PhysicsBreakdown,
    /// Bucket sample count consulted at projection time. `0` when no
    /// bucket / no stats were available (cold start, new VCP).
    stats_n: usize,
    /// Whether historical bucket samples contributed to the blended
    /// interval used for this chunk's projected time.
    used_historical: bool,
    /// `(avg_attempts − 1).max(0)` for the bucket — typical retry-poll
    /// overhead added between expected availability and the scheduler's
    /// first poll. Already folded into `projected_poll_at` (via
    /// [`IntervalEstimate::project_times`]); exposed so consumers can
    /// display the budget separately.
    retry_budget_secs: f64,
    /// The bucket key the lookup hit (or missed). `None` when no
    /// elevation was resolvable (Start chunk).
    bucket: Option<ChunkCharacteristics>,
}

impl ChunkProjection {
    /// The chunk's sequence number.
    pub(crate) fn sequence(&self) -> usize {
        self.sequence
    }

    /// The elevation number (1-based), or None for the Start chunk.
    pub(crate) fn elevation_number(&self) -> Option<usize> {
        self.elevation_number
    }

    /// Elevation angle in degrees.
    pub(crate) fn elevation_angle_deg(&self) -> f64 {
        self.elevation_angle_deg
    }

    /// AVAILABILITY category: projected time this chunk becomes available
    /// in S3.
    pub(crate) fn projected_available_at(&self) -> DateTime<Utc> {
        self.projected_available_at
    }

    /// POLL category: projected time the scheduler will fire its first
    /// download poll for this chunk.
    pub(crate) fn projected_poll_at(&self) -> DateTime<Utc> {
        self.projected_poll_at
    }

    /// COLLECTION category: projected Unix-seconds time the radar physically
    /// emits/receives for this chunk.
    pub(crate) fn projected_collection_time_secs(&self) -> f64 {
        self.projected_collection_time_secs
    }

    /// Duration from the anchor to this chunk's projected availability.
    pub(crate) fn offset_from_anchor(&self) -> Duration {
        self.offset_from_anchor
    }

    /// Duration from the previous chunk to this chunk.
    pub(crate) fn interval_from_previous(&self) -> Duration {
        self.interval_from_previous
    }

    /// Whether this chunk starts a new sweep.
    pub(crate) fn starts_new_sweep(&self) -> bool {
        self.starts_new_sweep
    }

    /// Which volume this projection belongs to, relative to the anchor.
    /// `0` = current volume; `1` = next volume (produced by chained
    /// projection in [`project_scan_timing_with_next`]).
    pub(crate) fn volume_offset(&self) -> u8 {
        self.volume_offset
    }

    /// Physics decomposition for the hop into this chunk.
    pub(crate) fn physics_breakdown(&self) -> PhysicsBreakdown {
        self.physics_breakdown
    }

    /// Bucket sample count at projection time. `0` when no historical
    /// samples were available for this chunk's bucket.
    pub(crate) fn stats_n(&self) -> usize {
        self.stats_n
    }

    /// Whether historical samples contributed to the projected interval.
    pub(crate) fn used_historical(&self) -> bool {
        self.used_historical
    }

    /// Typical retry-poll overhead `(avg_attempts − 1).max(0)` for this
    /// chunk's bucket. Already folded into `projected_poll_at`.
    pub(crate) fn retry_budget_secs(&self) -> f64 {
        self.retry_budget_secs
    }

    /// The bucket key this chunk's projection consulted.
    pub(crate) fn bucket(&self) -> Option<&ChunkCharacteristics> {
        self.bucket.as_ref()
    }
}

/// Build a timing projection for all remaining chunks in the current volume.
///
/// The projection starts from `anchor_chunk` (the most recently observed chunk) and
/// projects forward through the final chunk in the volume. Each chunk carries both a
/// projected COLLECTION time (ACTUAL collection anchor + physics intervals) and a
/// projected AVAILABILITY time (COLLECTION + empirical ingest lag).
///
/// When `anchor_collection_time_secs` is `Some`, projections are anchored in real
/// collection time and availability uses the observed anchor lag. Otherwise the
/// function falls back to anchoring on `anchor_chunk.upload_date_time()` with a
/// default lag estimate, which keeps behavior identical to the pre-split model.
///
/// Returns `None` if the anchor chunk's metadata cannot be resolved or if there are
/// no remaining chunks to project.
pub(crate) fn project_scan_timing(
    anchor_chunk: &ChunkIdentifier,
    anchor_collection_time_secs: Option<f64>,
    vcp: &volume_coverage_pattern::Message,
    mapper: &ElevationChunkMapper,
    timing_stats: Option<&ChunkTimingStats>,
    tuning: &TimingTuning,
) -> Option<ScanTimingProjection> {
    project_scan_timing_with_next(
        anchor_chunk,
        anchor_collection_time_secs,
        vcp,
        mapper,
        timing_stats,
        false,
        None,
        tuning,
    )
}

/// Like [`project_scan_timing`], but when `include_next_volume` is true,
/// chains a second pass through `mapper` after the current volume's final
/// sequence — projecting one full additional volume assuming the VCP is
/// unchanged.
///
/// The chained pass tags every emitted [`ChunkProjection`] with
/// `volume_offset = 1`. The transition between the last chunk of the current
/// volume and the first chunk (Start) of the next is automatically handled
/// by the physics model's `InterVolume` case (8.5 s gap).
///
/// Useful for the streaming timeline's "ghost next-scan" rendering, when the
/// user has filtered to an elevation that no longer appears in the current
/// volume and the next download target falls in the next volume.
///
/// Note: `volume_end_available_at` and `remaining_duration` describe the
/// LAST chunk in `chunks` — which is the end of the next volume when chained.
/// Consumers wanting current-volume-only bounds should filter `chunks` by
/// `volume_offset == 0`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn project_scan_timing_with_next(
    anchor_chunk: &ChunkIdentifier,
    anchor_collection_time_secs: Option<f64>,
    _vcp: &volume_coverage_pattern::Message,
    mapper: &ElevationChunkMapper,
    timing_stats: Option<&ChunkTimingStats>,
    include_next_volume: bool,
    next_volume_anchor: Option<(usize, f64)>,
    tuning: &TimingTuning,
) -> Option<ScanTimingProjection> {
    let anchor_sequence = anchor_chunk.sequence();
    let anchor_available_at = anchor_chunk.upload_date_time().unwrap_or_else(Utc::now);
    let anchor_available_at_secs = anchor_available_at.timestamp_millis() as f64 / 1000.0;
    let final_sequence = mapper.final_sequence();

    // If we're at/past the final chunk of the current volume and the caller
    // doesn't want a next-volume pass, there's nothing to project.
    if anchor_sequence >= final_sequence && !include_next_volume {
        return None;
    }

    let anchor_metadata = mapper.get_chunk_metadata(anchor_sequence)?;

    // Split anchor into collection + lag. When the caller knows the ACTUAL
    // volume header collection time, use it and derive the empirical lag as
    // upload − collection. Otherwise fall back to a lag estimate: prefer the
    // median lag observed in `ChunkTimingStats`, else a static default.
    let (anchor_collection_secs, observed_lag_secs, availability_lag_secs, anchor_source) =
        match anchor_collection_time_secs {
            Some(collection_secs) => {
                let lag = anchor_available_at_secs - collection_secs;
                (
                    collection_secs,
                    Some(lag),
                    lag,
                    AnchorSource::ObservedCollection,
                )
            }
            None => match timing_stats.and_then(|s| s.median_availability_lag_secs()) {
                Some(median_lag) => (
                    anchor_available_at_secs - median_lag,
                    None,
                    median_lag,
                    AnchorSource::UploadMinusMedian,
                ),
                None => (
                    anchor_available_at_secs - tuning.default_availability_lag_secs,
                    None,
                    tuning.default_availability_lag_secs,
                    AnchorSource::UploadMinusDefault,
                ),
            },
        };

    let mut projections = Vec::new();
    let mut cumulative_offset_ms: i64 = 0;
    let mut prev_metadata = anchor_metadata;
    let mut prev_collection_secs = anchor_collection_secs;

    // Chain the current-volume pass (volume_offset=0) with an optional
    // next-volume pass (volume_offset=1) that reuses the same mapper under
    // the assumption the VCP doesn't change. At the volume boundary, the
    // first hop's `next_metadata` is the next volume's Start chunk, so the
    // physics model's `InterVolume` case fires automatically and applies the
    // 8.5 s inter-volume gap.
    let pass1 = ((anchor_sequence + 1)..=final_sequence).map(|s| (s, 0u8));
    let pass2 = include_next_volume
        .then(|| (1..=final_sequence).map(|s| (s, 1u8)))
        .into_iter()
        .flatten();

    for (seq, volume_offset) in pass1.chain(pass2) {
        let next_metadata = mapper.get_chunk_metadata(seq)?;

        // Shared blended primitive (see `interval_estimate` module): pure
        // physics when no stats, 70/30 physics/historical blend otherwise.
        // `project_times` derives the three time axes (collection /
        // availability / poll) from one calculation so the scheduler and the
        // UI stay in lock-step.
        let bucket = chunk_characteristics(next_metadata, _vcp);
        let estimate = estimate_interval(
            prev_metadata,
            next_metadata,
            bucket.as_ref(),
            timing_stats,
            tuning,
        );
        let times = estimate.project_times(prev_collection_secs, availability_lag_secs, tuning);

        let interval_ms = (estimate.seconds * 1000.0) as i64;
        cumulative_offset_ms += interval_ms;
        let interval_duration = Duration::milliseconds(interval_ms);
        let offset_duration = Duration::milliseconds(cumulative_offset_ms);

        let projected_available_at =
            DateTime::<Utc>::from_timestamp_millis((times.available_at_secs * 1000.0) as i64)
                .unwrap_or(anchor_available_at);
        let projected_poll_at =
            DateTime::<Utc>::from_timestamp_millis((times.poll_at_secs * 1000.0) as i64)
                .unwrap_or(projected_available_at);

        projections.push(ChunkProjection {
            sequence: seq,
            elevation_number: next_metadata.elevation_number(),
            elevation_angle_deg: next_metadata.elevation_angle_deg(),
            projected_collection_time_secs: times.collection_at_secs,
            projected_available_at,
            projected_poll_at,
            offset_from_anchor: offset_duration,
            interval_from_previous: interval_duration,
            starts_new_sweep: next_metadata.is_first_in_sweep(),
            volume_offset,
            physics_breakdown: estimate.physics,
            stats_n: estimate.stats_n,
            used_historical: estimate.used_historical,
            retry_budget_secs: estimate.retry_budget_secs,
            bucket,
        });

        prev_metadata = next_metadata;
        prev_collection_secs = times.collection_at_secs;
    }

    // Self-anchor the next-volume (offset 1) timeline on a measured listing.
    // When the caller supplies a freshly-listed next-volume chunk
    // `(sequence, upload_secs)`, shift every offset-1 projection so that
    // sequence lands at its measured collection time (upload − lag). This pins
    // the ghost / cross-volume target to reality instead of the chained
    // inter-volume estimate, WITHOUT moving the volume frame — offset 0 stays
    // the anchor's volume, so the UI's "current scan" never gets mis-framed.
    apply_next_volume_anchor(&mut projections, next_volume_anchor, availability_lag_secs);

    let volume_end_available_at = projections
        .last()
        .map(|p| p.projected_available_at)
        .unwrap_or(anchor_available_at);
    // Derive `remaining_duration` from the (possibly anchor-shifted) last
    // projection's `offset_from_anchor` rather than the pre-shift
    // `cumulative_offset_ms`, so the invariant
    // `anchor + remaining_duration == volume_end` survives an anchor shift.
    // `apply_next_volume_anchor` shifts `offset_from_anchor` in lock-step with
    // the projected times, so this stays consistent in both the shifted and
    // unshifted cases.
    let remaining_duration = projections
        .last()
        .map(|p| p.offset_from_anchor)
        .unwrap_or_else(|| Duration::milliseconds(cumulative_offset_ms));

    Some(ScanTimingProjection {
        anchor_sequence,
        anchor_available_at,
        anchor_collection_time_secs: anchor_collection_secs,
        observed_anchor_lag_secs: observed_lag_secs,
        anchor_source,
        chunks: projections,
        volume_end_available_at,
        remaining_duration,
    })
}

/// Seconds to shift the next-volume timeline by: `measured − projected` at the
/// anchor sequence. `None` when the anchor sequence wasn't projected. Pure.
fn next_volume_shift_secs(
    projected_at_anchor: Option<f64>,
    measured_collection: f64,
) -> Option<f64> {
    projected_at_anchor.map(|projected| measured_collection - projected)
}

/// Shift every offset-1 (next-volume) projection so the measured anchor
/// sequence lands at its measured collection time (`upload − lag`). No-op when
/// no anchor is supplied or the anchor sequence isn't in the projection.
fn apply_next_volume_anchor(
    projections: &mut [ChunkProjection],
    next_volume_anchor: Option<(usize, f64)>,
    availability_lag_secs: f64,
) {
    let Some((anchor_seq, anchor_upload_secs)) = next_volume_anchor else {
        return;
    };
    let measured_collection = anchor_upload_secs - availability_lag_secs;
    let projected_at_anchor = projections
        .iter()
        .find(|p| p.volume_offset == 1 && p.sequence == anchor_seq)
        .map(|p| p.projected_collection_time_secs);
    let Some(delta_secs) = next_volume_shift_secs(projected_at_anchor, measured_collection) else {
        return;
    };
    let delta = Duration::milliseconds((delta_secs * 1000.0) as i64);
    for p in projections.iter_mut().filter(|p| p.volume_offset == 1) {
        p.projected_collection_time_secs += delta_secs;
        p.projected_available_at += delta;
        p.projected_poll_at += delta;
        // Keep `offset_from_anchor` in lock-step with the shifted projected
        // times so `offset_from_anchor == projected - anchor` holds for the
        // shifted (offset-1) chunks, and so the caller can derive
        // `remaining_duration`/`volume_end` consistently from the last chunk.
        p.offset_from_anchor += delta;
    }
}

/// Build a timing projection for an entire volume from the Start chunk.
///
/// This projects all chunks from sequence 1 through the final sequence, useful when
/// starting a fresh volume and wanting to display the full expected timeline.
///
/// The `start_time` parameter is the time the Start chunk was uploaded (or current time).
pub(crate) fn project_full_scan_timing(
    site: &str,
    volume: VolumeIndex,
    start_time: DateTime<Utc>,
    vcp: &volume_coverage_pattern::Message,
    mapper: &ElevationChunkMapper,
    timing_stats: Option<&ChunkTimingStats>,
    tuning: &TimingTuning,
) -> Option<ScanTimingProjection> {
    let start_chunk = ChunkIdentifier::new(
        site.to_string(),
        volume,
        start_time.naive_utc(),
        1,
        ChunkType::Start,
        Some(start_time),
    );

    project_scan_timing(&start_chunk, None, vcp, mapper, timing_stats, tuning)
}

#[cfg(test)]
mod tests {
    use super::super::test_vcp::{build_vcp, TestElevation};
    use super::super::ChunkCharacteristics;
    use super::*;
    use chrono::DateTime;
    use nexrad_data::aws::realtime::VolumeIndex;
    use nexrad_decode::messages::volume_coverage_pattern as vcpmsg;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn upload_at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn anchor(sequence: usize, chunk_type: ChunkType, upload_secs: i64) -> ChunkIdentifier {
        ChunkIdentifier::new(
            "KDMX".to_string(),
            VolumeIndex::new(1),
            upload_at(upload_secs).naive_utc(),
            sequence,
            chunk_type,
            Some(upload_at(upload_secs)),
        )
    }

    /// One super-res CS elevation at 22.5 dps → 6 chunks (seqs 2..=7), final 7.
    fn vcp_1elev() -> vcpmsg::Message<'static> {
        build_vcp(&[TestElevation {
            super_res: true,
            elevation_angle_raw: 0,
            azimuth_rate_raw: 1 << 14, // 22.5 dps
            waveform_raw: 1,           // CS
            channel_raw: 0,            // ConstantPhase
        }])
    }

    #[wasm_bindgen_test]
    fn shift_is_measured_minus_projected() {
        // Projection put the anchor sequence at 1100s; the measured collection
        // is 1090s → shift the next-volume timeline back by 10s.
        assert_eq!(next_volume_shift_secs(Some(1100.0), 1090.0), Some(-10.0));
        // Measured later than projected → shift forward.
        assert_eq!(next_volume_shift_secs(Some(1100.0), 1130.0), Some(30.0));
        // Anchor sequence not present in the projection → no shift.
        assert_eq!(next_volume_shift_secs(None, 1090.0), None);
    }

    #[wasm_bindgen_test]
    fn anchor_source_observed_collection_when_collection_supplied() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let a = anchor(3, ChunkType::Intermediate, 1000);
        let p = project_scan_timing(
            &a,
            Some(995.0), // ACTUAL collection time
            &vcp,
            &mapper,
            None,
            &TimingTuning::DEFAULT,
        )
        .unwrap();
        assert_eq!(p.anchor_source(), AnchorSource::ObservedCollection);
        // Observed lag = upload(1000) − collection(995) = 5.
        assert_eq!(p.observed_anchor_lag_secs(), Some(5.0));
        assert!((p.anchor_collection_time_secs() - 995.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn anchor_source_upload_minus_median_when_lag_stats_present() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        // Stats with a recorded availability lag → median is available.
        let mut stats = ChunkTimingStats::new();
        let b = ChunkCharacteristics {
            chunk_type: ChunkType::Intermediate,
            waveform_type: vcpmsg::WaveformType::CS,
            channel_configuration: vcpmsg::ChannelConfiguration::ConstantPhase,
            is_first_in_sweep: false,
        };
        stats.add_timing(b, Duration::seconds(10), Some(Duration::seconds(4)), 1);

        let a = anchor(3, ChunkType::Intermediate, 1000);
        let p = project_scan_timing(
            &a,
            None,
            &vcp,
            &mapper,
            Some(&stats),
            &TimingTuning::DEFAULT,
        )
        .unwrap();
        assert_eq!(p.anchor_source(), AnchorSource::UploadMinusMedian);
        // collection = upload(1000) − median_lag(4) = 996.
        assert!((p.anchor_collection_time_secs() - 996.0).abs() < 1e-9);
        assert_eq!(p.observed_anchor_lag_secs(), None);
    }

    #[wasm_bindgen_test]
    fn anchor_source_upload_minus_default_when_no_lag() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let a = anchor(3, ChunkType::Intermediate, 1000);
        let p = project_scan_timing(&a, None, &vcp, &mapper, None, &TimingTuning::DEFAULT).unwrap();
        assert_eq!(p.anchor_source(), AnchorSource::UploadMinusDefault);
        // collection = upload(1000) − default_lag(5) = 995.
        assert!(
            (p.anchor_collection_time_secs()
                - (1000.0 - TimingTuning::DEFAULT.default_availability_lag_secs))
                .abs()
                < 1e-9
        );
    }

    #[wasm_bindgen_test]
    fn none_at_volume_boundary_without_next_volume() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        // Anchor at the final sequence (7), no next-volume pass requested.
        let a = anchor(7, ChunkType::End, 1000);
        assert!(
            project_scan_timing(&a, None, &vcp, &mapper, None, &TimingTuning::DEFAULT).is_none()
        );
    }

    #[wasm_bindgen_test]
    fn chaining_emits_offset1_starting_with_a_start_chunk_at_inter_volume_gap() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        // Anchor at the final sequence so the only remaining work is the next
        // volume; request the chained pass.
        let a = anchor(7, ChunkType::End, 1000);
        let p = project_scan_timing_with_next(
            &a,
            Some(990.0),
            &vcp,
            &mapper,
            None,
            true,
            None,
            &TimingTuning::DEFAULT,
        )
        .unwrap();
        // All chunks are next-volume (offset 1), seqs 1..=7.
        assert!(p.chunks().iter().all(|c| c.volume_offset() == 1));
        let seqs: Vec<usize> = p.chunks().iter().map(|c| c.sequence()).collect();
        assert_eq!(seqs, (1..=7).collect::<Vec<_>>());

        // The first offset-1 chunk is the next volume's Start chunk (seq 1).
        let first = &p.chunks()[0];
        assert_eq!(first.sequence(), 1);
        assert_eq!(first.elevation_number(), None);
        // The hop into it is the InterVolume case (fixed 8.5s gap).
        assert_eq!(
            first.physics_breakdown().case,
            crate::nexrad::timing::IntervalCase::InterVolume
        );
        assert!((first.physics_breakdown().total_secs - 8.5).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn next_volume_anchor_shifts_only_offset1_and_keeps_invariants() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        // Anchor mid-volume so we have both offset-0 and offset-1 chunks.
        let a = anchor(3, ChunkType::Intermediate, 1000);
        let lag = 5.0; // default lag (no collection supplied, no stats)

        // Baseline chained projection (no measured next-volume anchor).
        let base = project_scan_timing_with_next(
            &a,
            None,
            &vcp,
            &mapper,
            None,
            true,
            None,
            &TimingTuning::DEFAULT,
        )
        .unwrap();
        // Pick the next-volume seq-2 chunk's baseline projected collection.
        let base_seq2 = base
            .chunks()
            .iter()
            .find(|c| c.volume_offset() == 1 && c.sequence() == 2)
            .unwrap();
        let base_seq2_collection = base_seq2.projected_collection_time_secs();

        // Supply a measured next-volume anchor for seq 2 that is 12s LATER than
        // the chained estimate would place it. measured_collection = upload−lag,
        // so pick upload so that upload − lag == base_seq2_collection + 12.
        let measured_collection = base_seq2_collection + 12.0;
        let upload_secs = measured_collection + lag;
        let p = project_scan_timing_with_next(
            &a,
            None,
            &vcp,
            &mapper,
            None,
            true,
            Some((2, upload_secs)),
            &TimingTuning::DEFAULT,
        )
        .unwrap();

        // Offset-0 chunks are unchanged vs baseline.
        for (b, c) in base
            .chunks()
            .iter()
            .filter(|c| c.volume_offset() == 0)
            .zip(p.chunks().iter().filter(|c| c.volume_offset() == 0))
        {
            assert_eq!(b.sequence(), c.sequence());
            assert!(
                (b.projected_collection_time_secs() - c.projected_collection_time_secs()).abs()
                    < 1e-6
            );
        }

        // The anchored offset-1 seq-2 chunk now lands at the measured collection.
        let seq2 = p
            .chunks()
            .iter()
            .find(|c| c.volume_offset() == 1 && c.sequence() == 2)
            .unwrap();
        assert!(
            (seq2.projected_collection_time_secs() - measured_collection).abs() < 1e-2,
            "got {}",
            seq2.projected_collection_time_secs()
        );

        // Every offset-1 chunk shifted by ~+12s vs baseline.
        for (b, c) in base
            .chunks()
            .iter()
            .filter(|c| c.volume_offset() == 1)
            .zip(p.chunks().iter().filter(|c| c.volume_offset() == 1))
        {
            let delta = c.projected_collection_time_secs() - b.projected_collection_time_secs();
            assert!(
                (delta - 12.0).abs() < 1e-2,
                "seq {} delta {}",
                b.sequence(),
                delta
            );
        }

        // Invariant: offset_from_anchor == projected_available_at − anchor for
        // the shifted offset-1 chunks (the fix keeps these in lock-step).
        let anchor_avail = p.anchor_available_at();
        for c in p.chunks().iter().filter(|c| c.volume_offset() == 1) {
            let from_times = c.projected_available_at() - anchor_avail;
            let diff = (from_times - c.offset_from_anchor())
                .num_milliseconds()
                .abs();
            assert!(
                diff <= 1,
                "offset mismatch for seq {}: {diff}ms",
                c.sequence()
            );
        }

        // Invariant: anchor_available_at + remaining_duration == volume_end.
        let recomputed_end = p.anchor_available_at() + p.remaining_duration();
        let diff = (recomputed_end - p.volume_end_available_at())
            .num_milliseconds()
            .abs();
        assert!(diff <= 1, "remaining/volume_end mismatch: {diff}ms");
    }

    #[wasm_bindgen_test]
    fn blend_path_used_when_collection_stats_present() {
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        // Seed a collection-domain interval for the seq-4 bucket so the hop
        // 3->4 blends physics with history.
        let next_meta = mapper.get_chunk_metadata(4).unwrap();
        let bucket = chunk_characteristics(next_meta, &vcp).unwrap();
        let mut stats = ChunkTimingStats::new();
        stats.add_timing(bucket, Duration::seconds(30), None, 1);
        stats.attach_collection_interval(&bucket, Duration::seconds(30));

        let a = anchor(3, ChunkType::Intermediate, 1000);
        let p = project_scan_timing(
            &a,
            Some(995.0),
            &vcp,
            &mapper,
            Some(&stats),
            &TimingTuning::DEFAULT,
        )
        .unwrap();
        let seq4 = p.chunks().iter().find(|c| c.sequence() == 4).unwrap();
        assert!(seq4.used_historical());
        assert_eq!(seq4.stats_n(), 1);
        assert_eq!(seq4.bucket(), Some(&bucket));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::super::test_vcp::{build_vcp, TestElevation};
    use super::*;
    use chrono::DateTime;
    use nexrad_data::aws::realtime::VolumeIndex;
    use nexrad_decode::messages::volume_coverage_pattern as vcpmsg;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn upload_at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn anchor(sequence: usize, chunk_type: ChunkType, upload_secs: i64) -> ChunkIdentifier {
        ChunkIdentifier::new(
            "KDMX".to_string(),
            VolumeIndex::new(1),
            upload_at(upload_secs).naive_utc(),
            sequence,
            chunk_type,
            Some(upload_at(upload_secs)),
        )
    }

    /// One super-res CS elevation at 22.5 dps → 6 chunks (seqs 2..=7), final 7.
    fn vcp_1elev() -> vcpmsg::Message<'static> {
        build_vcp(&[TestElevation {
            super_res: true,
            elevation_angle_raw: 0,
            azimuth_rate_raw: 1 << 14, // 22.5 dps
            waveform_raw: 1,           // CS
            channel_raw: 0,            // ConstantPhase
        }])
    }

    #[wasm_bindgen_test]
    fn anchor_source_short_strings() {
        // Pure enum → string mapping; not covered by existing tests.
        assert_eq!(AnchorSource::ObservedCollection.short(), "obs");
        assert_eq!(AnchorSource::UploadMinusMedian.short(), "median");
        assert_eq!(AnchorSource::UploadMinusDefault.short(), "default");
    }

    #[wasm_bindgen_test]
    fn anchor_source_eq_and_copy() {
        let a = AnchorSource::UploadMinusMedian;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(
            AnchorSource::ObservedCollection,
            AnchorSource::UploadMinusDefault
        );
    }

    #[wasm_bindgen_test]
    fn current_volume_only_projects_remaining_offset0_seqs() {
        // Anchor mid-volume at seq 3; no next-volume pass. The remaining
        // chunks in the current volume are seqs 4..=7, all offset 0.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let a = anchor(3, ChunkType::Intermediate, 1000);
        let p = project_scan_timing(&a, Some(995.0), &vcp, &mapper, None, &TimingTuning::DEFAULT)
            .unwrap();

        assert_eq!(p.anchor_sequence(), 3);
        let seqs: Vec<usize> = p.chunks().iter().map(|c| c.sequence()).collect();
        assert_eq!(seqs, vec![4, 5, 6, 7]);
        assert!(p.chunks().iter().all(|c| c.volume_offset() == 0));
    }

    #[wasm_bindgen_test]
    fn offsets_and_available_times_are_monotonic() {
        // offset_from_anchor and projected_available_at must be strictly
        // increasing across the projected chunks (each interval is positive).
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let a = anchor(2, ChunkType::Intermediate, 2000);
        let p = project_scan_timing(
            &a,
            Some(1990.0),
            &vcp,
            &mapper,
            None,
            &TimingTuning::DEFAULT,
        )
        .unwrap();
        let chunks = p.chunks();
        assert!(chunks.len() >= 2);
        for w in chunks.windows(2) {
            assert!(
                w[1].offset_from_anchor() > w[0].offset_from_anchor(),
                "offset_from_anchor not increasing"
            );
            assert!(
                w[1].projected_available_at() > w[0].projected_available_at(),
                "projected_available_at not increasing"
            );
            // Each per-chunk interval is strictly positive.
            assert!(w[1].interval_from_previous().num_milliseconds() > 0);
        }
        // First chunk's offset equals its own interval_from_previous.
        assert_eq!(
            chunks[0].offset_from_anchor().num_milliseconds(),
            chunks[0].interval_from_previous().num_milliseconds()
        );
    }

    #[wasm_bindgen_test]
    fn intervals_sum_to_last_offset() {
        // The cumulative offset_from_anchor of the final chunk equals the sum
        // of all per-chunk intervals (no anchor shift in this simple case).
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let a = anchor(3, ChunkType::Intermediate, 1000);
        let p = project_scan_timing(&a, None, &vcp, &mapper, None, &TimingTuning::DEFAULT).unwrap();
        let sum_ms: i64 = p
            .chunks()
            .iter()
            .map(|c| c.interval_from_previous().num_milliseconds())
            .sum();
        let last_off = p
            .chunks()
            .last()
            .unwrap()
            .offset_from_anchor()
            .num_milliseconds();
        assert_eq!(sum_ms, last_off);
    }

    #[wasm_bindgen_test]
    fn remaining_duration_invariant_simple_case() {
        // anchor_available_at + remaining_duration == volume_end_available_at,
        // and volume_end matches the last chunk's projected availability.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let a = anchor(4, ChunkType::Intermediate, 1500);
        let p = project_scan_timing(
            &a,
            Some(1495.0),
            &vcp,
            &mapper,
            None,
            &TimingTuning::DEFAULT,
        )
        .unwrap();

        let recomputed_end = p.anchor_available_at() + p.remaining_duration();
        let diff = (recomputed_end - p.volume_end_available_at())
            .num_milliseconds()
            .abs();
        assert!(diff <= 1, "remaining/volume_end mismatch: {diff}ms");

        let last_avail = p.chunks().last().unwrap().projected_available_at();
        assert_eq!(last_avail, p.volume_end_available_at());
    }

    #[wasm_bindgen_test]
    fn full_scan_projects_whole_volume_from_start() {
        // project_full_scan_timing builds the Start chunk (seq 1) and projects
        // seqs 2..=7. No collection supplied → UploadMinusDefault anchor.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let start_time = upload_at(3000);
        let p = project_full_scan_timing(
            "KDMX",
            VolumeIndex::new(1),
            start_time,
            &vcp,
            &mapper,
            None,
            &TimingTuning::DEFAULT,
        )
        .unwrap();

        assert_eq!(p.anchor_sequence(), 1);
        assert_eq!(p.anchor_source(), AnchorSource::UploadMinusDefault);
        assert_eq!(p.observed_anchor_lag_secs(), None);
        let seqs: Vec<usize> = p.chunks().iter().map(|c| c.sequence()).collect();
        assert_eq!(seqs, vec![2, 3, 4, 5, 6, 7]);
        // collection anchor = upload(3000) − default_lag(5) = 2995.
        assert!(
            (p.anchor_collection_time_secs()
                - (3000.0 - TimingTuning::DEFAULT.default_availability_lag_secs))
                .abs()
                < 1e-9
        );
    }

    #[wasm_bindgen_test]
    fn no_stats_means_no_historical_blend() {
        // With timing_stats=None every chunk uses pure physics: stats_n == 0,
        // used_historical == false, retry_budget == 0.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let a = anchor(3, ChunkType::Intermediate, 1000);
        let p = project_scan_timing(&a, Some(995.0), &vcp, &mapper, None, &TimingTuning::DEFAULT)
            .unwrap();
        for c in p.chunks() {
            assert_eq!(c.stats_n(), 0);
            assert!(!c.used_historical());
            assert!((c.retry_budget_secs() - 0.0).abs() < 1e-9);
        }
    }

    #[wasm_bindgen_test]
    fn collection_times_increase_and_lead_availability() {
        // projected_collection_time_secs is strictly increasing, and each
        // chunk's availability is collection + a non-negative lag.
        let vcp = vcp_1elev();
        let mapper = ElevationChunkMapper::new(&vcp);
        let a = anchor(3, ChunkType::Intermediate, 1000);
        // Observed collection 990 → lag = upload(1000) − 990 = 10s.
        let p = project_scan_timing(&a, Some(990.0), &vcp, &mapper, None, &TimingTuning::DEFAULT)
            .unwrap();
        let chunks = p.chunks();
        for w in chunks.windows(2) {
            assert!(
                w[1].projected_collection_time_secs() > w[0].projected_collection_time_secs(),
                "collection not increasing"
            );
        }
        for c in chunks {
            let avail_secs = c.projected_available_at().timestamp_millis() as f64 / 1000.0;
            // availability = collection + lag(10) → strictly later, ~10s gap.
            let gap = avail_secs - c.projected_collection_time_secs();
            assert!(gap > 0.0, "availability not after collection");
            assert!((gap - 10.0).abs() < 0.5, "lag gap unexpected: {gap}");
        }
    }
}
