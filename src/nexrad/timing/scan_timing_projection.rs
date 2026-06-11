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
pub enum AnchorSource {
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
    pub fn short(&self) -> &'static str {
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
pub struct ScanTimingProjection {
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
    pub fn anchor_sequence(&self) -> usize {
        self.anchor_sequence
    }

    /// AVAILABILITY category: the S3-upload time of the anchor chunk (or
    /// current time as a fallback). Physics intervals added to this yield
    /// projected availability times for future chunks.
    pub fn anchor_available_at(&self) -> DateTime<Utc> {
        self.anchor_available_at
    }

    /// COLLECTION category: Unix-seconds collection time of the anchor
    /// chunk — parsed from a radial header when available, otherwise
    /// estimated as `anchor_available_at - observed_anchor_lag`.
    #[allow(dead_code)] // Consumed by debug UI in a later commit.
    pub fn anchor_collection_time_secs(&self) -> f64 {
        self.anchor_collection_time_secs
    }

    /// AVAILABILITY-lag category: observed anchor lag, if the caller
    /// provided an ACTUAL collection anchor. `None` signals the default
    /// fallback lag was used and projections carry more uncertainty.
    #[allow(dead_code)] // Consumed by debug UI in a later commit.
    pub fn observed_anchor_lag_secs(&self) -> Option<f64> {
        self.observed_anchor_lag_secs
    }

    /// Which branch the anchor came from. Used by the diagnostics modal to
    /// flag projections built on a fallback anchor.
    pub fn anchor_source(&self) -> AnchorSource {
        self.anchor_source
    }

    /// Projected timing for each future chunk, in sequence order.
    pub fn chunks(&self) -> &[ChunkProjection] {
        &self.chunks
    }

    /// AVAILABILITY category: projected time the final chunk becomes
    /// available in S3.
    pub fn volume_end_available_at(&self) -> DateTime<Utc> {
        self.volume_end_available_at
    }

    /// Projected remaining duration from anchor to volume end.
    pub fn remaining_duration(&self) -> Duration {
        self.remaining_duration
    }
}

/// Projection for a single future chunk.
#[derive(Debug, Clone)]
pub struct ChunkProjection {
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
    /// Duration from the anchor to this chunk's projected availability.
    offset_from_anchor: Duration,
    /// Duration from the previous chunk to this chunk.
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
    pub fn sequence(&self) -> usize {
        self.sequence
    }

    /// The elevation number (1-based), or None for the Start chunk.
    pub fn elevation_number(&self) -> Option<usize> {
        self.elevation_number
    }

    /// Elevation angle in degrees.
    pub fn elevation_angle_deg(&self) -> f64 {
        self.elevation_angle_deg
    }

    /// AVAILABILITY category: projected time this chunk becomes available
    /// in S3.
    pub fn projected_available_at(&self) -> DateTime<Utc> {
        self.projected_available_at
    }

    /// POLL category: projected time the scheduler will fire its first
    /// download poll for this chunk.
    pub fn projected_poll_at(&self) -> DateTime<Utc> {
        self.projected_poll_at
    }

    /// COLLECTION category: projected Unix-seconds time the radar physically
    /// emits/receives for this chunk.
    pub fn projected_collection_time_secs(&self) -> f64 {
        self.projected_collection_time_secs
    }

    /// Duration from the anchor to this chunk's projected availability.
    pub fn offset_from_anchor(&self) -> Duration {
        self.offset_from_anchor
    }

    /// Duration from the previous chunk to this chunk.
    pub fn interval_from_previous(&self) -> Duration {
        self.interval_from_previous
    }

    /// Whether this chunk starts a new sweep.
    pub fn starts_new_sweep(&self) -> bool {
        self.starts_new_sweep
    }

    /// Which volume this projection belongs to, relative to the anchor.
    /// `0` = current volume; `1` = next volume (produced by chained
    /// projection in [`project_scan_timing_with_next`]).
    pub fn volume_offset(&self) -> u8 {
        self.volume_offset
    }

    /// Physics decomposition for the hop into this chunk.
    pub fn physics_breakdown(&self) -> PhysicsBreakdown {
        self.physics_breakdown
    }

    /// Bucket sample count at projection time. `0` when no historical
    /// samples were available for this chunk's bucket.
    pub fn stats_n(&self) -> usize {
        self.stats_n
    }

    /// Whether historical samples contributed to the projected interval.
    pub fn used_historical(&self) -> bool {
        self.used_historical
    }

    /// Typical retry-poll overhead `(avg_attempts − 1).max(0)` for this
    /// chunk's bucket. Already folded into `projected_poll_at`.
    pub fn retry_budget_secs(&self) -> f64 {
        self.retry_budget_secs
    }

    /// The bucket key this chunk's projection consulted.
    pub fn bucket(&self) -> Option<&ChunkCharacteristics> {
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
pub fn project_scan_timing(
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
pub fn project_scan_timing_with_next(
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
    let remaining_duration = Duration::milliseconds(cumulative_offset_ms);

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
    }
}

/// Build a timing projection for an entire volume from the Start chunk.
///
/// This projects all chunks from sequence 1 through the final sequence, useful when
/// starting a fresh volume and wanting to display the full expected timeline.
///
/// The `start_time` parameter is the time the Start chunk was uploaded (or current time).
pub fn project_full_scan_timing(
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
    use super::next_volume_shift_secs;
    use wasm_bindgen_test::wasm_bindgen_test;

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
}
