use super::{ChunkCharacteristics, ChunkTimingModel, ChunkTimingStats, ElevationChunkMapper};
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

/// Fallback NEXRAD ingest lag (Unix seconds) used when we have no observed
/// anchor lag yet. Chosen to roughly match typical S3 upload latencies
/// observed during live streaming (~5-15 s). A later commit replaces this
/// with a median from the split `ChunkTimingStats`.
const DEFAULT_AVAILABILITY_LAG_SECS: f64 = 5.0;

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
    /// in S3. Drives the download scheduler and "next in Xs" UI language.
    projected_available_at: DateTime<Utc>,
    /// Duration from the anchor to this chunk's projected availability.
    offset_from_anchor: Duration,
    /// Duration from the previous chunk to this chunk.
    interval_from_previous: Duration,
    /// Whether this chunk starts a new sweep (useful for UI grouping).
    starts_new_sweep: bool,
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
    _vcp: &volume_coverage_pattern::Message,
    mapper: &ElevationChunkMapper,
    timing_stats: Option<&ChunkTimingStats>,
) -> Option<ScanTimingProjection> {
    let anchor_sequence = anchor_chunk.sequence();
    let anchor_available_at = anchor_chunk.upload_date_time().unwrap_or_else(Utc::now);
    let anchor_available_at_secs = anchor_available_at.timestamp_millis() as f64 / 1000.0;
    let final_sequence = mapper.final_sequence();

    // Nothing to project if we're at or past the final chunk
    if anchor_sequence >= final_sequence {
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
                    anchor_available_at_secs - DEFAULT_AVAILABILITY_LAG_SECS,
                    None,
                    DEFAULT_AVAILABILITY_LAG_SECS,
                    AnchorSource::UploadMinusDefault,
                ),
            },
        };

    let mut projections = Vec::new();
    let mut cumulative_offset_ms: i64 = 0;
    let mut prev_metadata = anchor_metadata;

    for seq in (anchor_sequence + 1)..=final_sequence {
        let next_metadata = mapper.get_chunk_metadata(seq)?;

        // Compute interval using physics model
        let mut interval_secs =
            ChunkTimingModel::estimate_chunk_interval_secs(prev_metadata, next_metadata);

        // Blend with historical data if available
        if let Some(stats) = timing_stats {
            if let Some(elev_num) = next_metadata.elevation_number() {
                if let Some(elev_data) = _vcp.elevations().get(elev_num - 1) {
                    let characteristics = ChunkCharacteristics {
                        chunk_type: ChunkType::Intermediate,
                        waveform_type: elev_data.waveform_type(),
                        channel_configuration: elev_data.channel_configuration(),
                        is_first_in_sweep: next_metadata.is_first_in_sweep(),
                    };

                    if let Some(avg_timing) = stats.get_average_timing(&characteristics) {
                        let historical_secs = avg_timing.num_milliseconds() as f64 / 1000.0;
                        // Blend: 70% physics, 30% historical
                        interval_secs = interval_secs * 0.7 + historical_secs * 0.3;
                    }
                }
            }
        }

        let interval_ms = (interval_secs * 1000.0) as i64;
        cumulative_offset_ms += interval_ms;

        let interval_duration = Duration::milliseconds(interval_ms);
        let offset_duration = Duration::milliseconds(cumulative_offset_ms);
        let offset_secs = cumulative_offset_ms as f64 / 1000.0;

        let projected_collection_time_secs = anchor_collection_secs + offset_secs;
        let projected_available_at_secs = projected_collection_time_secs + availability_lag_secs;
        let projected_available_at =
            DateTime::<Utc>::from_timestamp_millis((projected_available_at_secs * 1000.0) as i64)
                .unwrap_or(anchor_available_at);

        projections.push(ChunkProjection {
            sequence: seq,
            elevation_number: next_metadata.elevation_number(),
            elevation_angle_deg: next_metadata.elevation_angle_deg(),
            projected_collection_time_secs,
            projected_available_at,
            offset_from_anchor: offset_duration,
            interval_from_previous: interval_duration,
            starts_new_sweep: next_metadata.is_first_in_sweep(),
        });

        prev_metadata = next_metadata;
    }

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
) -> Option<ScanTimingProjection> {
    let start_chunk = ChunkIdentifier::new(
        site.to_string(),
        volume,
        start_time.naive_utc(),
        1,
        ChunkType::Start,
        Some(start_time),
    );

    project_scan_timing(&start_chunk, None, vcp, mapper, timing_stats)
}
