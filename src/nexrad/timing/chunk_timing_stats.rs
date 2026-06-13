use chrono::Duration;
use nexrad_data::aws::realtime::ChunkType;
use nexrad_decode::messages::volume_coverage_pattern::{ChannelConfiguration, WaveformType};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

/// Maximum number of timing samples to keep per chunk characteristics.
/// Read from the global tuning default — the rolling window is a property
/// of the stats store itself, not per-projector.
const MAX_TIMING_SAMPLES: usize = super::TimingTuning::DEFAULT.max_timing_samples;

/// Schema version for the serialized `ChunkTimingStats` payload in localStorage.
/// Bump when the on-disk shape changes so old caches are ignored rather than
/// silently misinterpreted.
///
/// v2 added `availability_lag_ms`; v3 renamed `duration_ms` to
/// `availability_interval_ms` and added `collection_interval_ms` — older
/// payloads are discarded because the collection-domain samples can't be
/// backfilled from S3 deltas alone.
const PERSIST_SCHEMA_VERSION: u32 = 3;

/// Characteristics of a chunk that affect timing.
///
/// `is_first_in_sweep` is part of the key because first-chunks-of-sweep include a
/// substantial inter-sweep transition overhead (antenna slew + waveform mode switch),
/// while intra-sweep chunks are purely rotation-rate-driven. Mixing the two into one
/// statistics bucket prevents the rolling average from converging on either value.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ChunkCharacteristics {
    /// Type of the chunk
    pub chunk_type: ChunkType,
    /// Waveform type of the elevation
    pub waveform_type: WaveformType,
    /// Channel configuration of the elevation
    pub channel_configuration: ChannelConfiguration,
    /// Whether this chunk is the first in its sweep (inter-sweep transition overhead
    /// applies). See struct docs for why this is keyed separately.
    pub is_first_in_sweep: bool,
}

impl Hash for ChunkCharacteristics {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(&self.chunk_type).hash(state);
        std::mem::discriminant(&self.waveform_type).hash(state);
        std::mem::discriminant(&self.channel_configuration).hash(state);
        self.is_first_in_sweep.hash(state);
    }
}

/// Per-bucket diagnostic snapshot, returned by
/// [`ChunkTimingStats::per_bucket_stats`]. Used in the VCP forecast
/// diagnostics summary so we can see which buckets have warmed up and
/// whether their availability lag deviates from the global median.
#[derive(Debug, Clone, Copy)]
pub struct BucketStats {
    pub characteristics: ChunkCharacteristics,
    pub sample_count: usize,
    pub mean_duration_ms: i64,
    /// Median availability lag for this bucket only. `None` until at least
    /// one sample with a recorded `availability_lag` exists for this bucket.
    pub median_lag_ms: Option<i64>,
    /// How many of `sample_count` samples carried a non-`None`
    /// `availability_lag` measurement.
    pub lag_sample_count: usize,
}

fn chunk_type_order(t: ChunkType) -> u8 {
    match t {
        ChunkType::Start => 0,
        ChunkType::Intermediate => 1,
        ChunkType::End => 2,
    }
}

fn waveform_order(w: WaveformType) -> u8 {
    match w {
        WaveformType::CS => 0,
        WaveformType::CDW => 1,
        WaveformType::CDWO => 2,
        WaveformType::B => 3,
        WaveformType::SPP => 4,
        WaveformType::Unknown => 5,
    }
}

fn channel_order(c: ChannelConfiguration) -> u8 {
    match c {
        ChannelConfiguration::ConstantPhase => 0,
        ChannelConfiguration::RandomPhase => 1,
        ChannelConfiguration::SZ2Phase => 2,
        ChannelConfiguration::UnknownPhase => 3,
    }
}

/// Statistics for a single timing sample.
///
/// Each field is named by its time domain:
/// - `availability_interval`: S3-upload→S3-upload delta (AVAILABILITY-to-
///   AVAILABILITY) measured by the streaming loop at download time.
///   Diagnostics-only — systematically biased by upload jitter, so it must
///   NOT proxy for the collection interval.
/// - `collection_interval`: delta between consecutive parsed radial
///   collection-end times (COLLECTION-to-COLLECTION), attached once the
///   worker decodes the chunk. Feeds the interval-estimate blend.
/// - `availability_lag`: empirical (upload − ACTUAL collection) delay for
///   this chunk; estimates the availability-lag tail per characteristics.
///
/// `None` on the optional fields indicates the observation never arrived
/// for that sample (decode failed, predates plumbing, …).
#[derive(Debug, Clone, Copy)]
pub(super) struct TimingStat {
    availability_interval: Duration,
    collection_interval: Option<Duration>,
    availability_lag: Option<Duration>,
    attempts: usize,
}

/// Statistics for timing between chunks
#[derive(Debug, Clone, Default)]
pub struct ChunkTimingStats {
    /// Timing statistics for each chunk characteristics
    timings: HashMap<ChunkCharacteristics, VecDeque<TimingStat>>,
}

impl ChunkTimingStats {
    /// Create a new empty timing statistics
    pub fn new() -> Self {
        Self {
            timings: HashMap::new(),
        }
    }

    /// Add an S3-upload-delta timing sample for the given chunk characteristics.
    /// `availability_lag` is optional; pass `Some` when the per-chunk lag
    /// (S3 upload − ACTUAL collection time) can be computed from a successful
    /// worker ingest, `None` when only the S3 delta is known. The
    /// collection-interval observation arrives later (worker decode) via
    /// [`Self::attach_collection_interval`].
    pub fn add_timing(
        &mut self,
        characteristics: ChunkCharacteristics,
        availability_interval: Duration,
        availability_lag: Option<Duration>,
        attempts: usize,
    ) {
        let entry = self.timings.entry(characteristics).or_default();

        entry.push_back(TimingStat {
            availability_interval,
            collection_interval: None,
            availability_lag,
            attempts,
        });

        // Maintain the rolling window by removing oldest if we exceed the max
        if entry.len() > MAX_TIMING_SAMPLES {
            entry.pop_front();
        }
    }

    /// Update the most recent sample for the given characteristics to attach
    /// a COLLECTION-domain inter-chunk interval (delta of consecutive parsed
    /// radial collection-end times). Recorded once the worker decodes the
    /// chunk, mirroring [`Self::attach_availability_lag`].
    pub fn attach_collection_interval(
        &mut self,
        characteristics: &ChunkCharacteristics,
        collection_interval: Duration,
    ) {
        if let Some(entry) = self.timings.get_mut(characteristics) {
            if let Some(latest) = entry.back_mut() {
                latest.collection_interval = Some(collection_interval);
            }
        }
    }

    /// Update the most recent sample for the given characteristics to attach
    /// an availability lag observation. Used when the S3 delta is recorded in
    /// the streaming loop but the ACTUAL collection time only becomes known
    /// later once the worker decodes the chunk.
    pub fn attach_availability_lag(
        &mut self,
        characteristics: &ChunkCharacteristics,
        availability_lag: Duration,
    ) {
        if let Some(entry) = self.timings.get_mut(characteristics) {
            if let Some(latest) = entry.back_mut() {
                latest.availability_lag = Some(availability_lag);
            }
        }
    }

    /// Median availability lag (S3 upload − ACTUAL collection) across all
    /// characteristics, as Unix seconds. Returns `None` until at least one
    /// sample with a lag observation has been recorded. Median (not mean)
    /// so clock outliers don't skew the projection fallback.
    pub fn median_availability_lag_secs(&self) -> Option<f64> {
        let mut lags_ms: Vec<i64> = self
            .timings
            .values()
            .flat_map(|queue| queue.iter())
            .filter_map(|stat| stat.availability_lag.map(|d| d.num_milliseconds()))
            .collect();
        if lags_ms.is_empty() {
            return None;
        }
        lags_ms.sort_unstable();
        let mid = lags_ms.len() / 2;
        let median_ms = if lags_ms.len().is_multiple_of(2) {
            (lags_ms[mid - 1] + lags_ms[mid]) / 2
        } else {
            lags_ms[mid]
        };
        Some(median_ms as f64 / 1000.0)
    }

    /// Average AVAILABILITY-domain (S3-upload→S3-upload) interval for the
    /// given chunk characteristics. Diagnostics-only — see [`TimingStat`].
    pub(super) fn average_availability_interval(
        &self,
        characteristics: &ChunkCharacteristics,
    ) -> Option<Duration> {
        self.timings.get(characteristics).and_then(|timings| {
            if timings.is_empty() {
                return None;
            }

            let total_millis: i64 = timings
                .iter()
                .map(|timing| timing.availability_interval.num_milliseconds())
                .sum();

            let avg_millis = total_millis / timings.len() as i64;
            Some(Duration::milliseconds(avg_millis))
        })
    }

    /// Average COLLECTION-domain inter-chunk interval for the given chunk
    /// characteristics, over the samples that carry one. `None` when no
    /// sample in the bucket has a collection observation yet — callers fall
    /// back to pure physics, never to the availability deltas (that would
    /// re-conflate the domains).
    pub(super) fn average_collection_interval(
        &self,
        characteristics: &ChunkCharacteristics,
    ) -> Option<Duration> {
        self.timings.get(characteristics).and_then(|timings| {
            let intervals_ms: Vec<i64> = timings
                .iter()
                .filter_map(|t| t.collection_interval.map(|d| d.num_milliseconds()))
                .collect();
            if intervals_ms.is_empty() {
                return None;
            }
            let avg = intervals_ms.iter().sum::<i64>() / intervals_ms.len() as i64;
            Some(Duration::milliseconds(avg))
        })
    }

    /// Get the average number of attempts for the given chunk characteristics
    pub(super) fn get_average_attempts(
        &self,
        characteristics: &ChunkCharacteristics,
    ) -> Option<f64> {
        self.timings.get(characteristics).and_then(|timings| {
            if timings.is_empty() {
                return None;
            }

            let total_attempts: usize = timings.iter().map(|timing| timing.attempts).sum();
            Some(total_attempts as f64 / timings.len() as f64)
        })
    }

    /// Get all chunk statistics for display purposes
    pub fn get_statistics(&self) -> Vec<(ChunkCharacteristics, Option<Duration>, Option<f64>)> {
        self.timings
            .keys()
            .map(|characteristics| {
                (
                    *characteristics,
                    self.average_availability_interval(characteristics),
                    self.get_average_attempts(characteristics),
                )
            })
            .collect()
    }

    /// Number of samples held for the given characteristics bucket.
    /// 0 if the bucket has never been populated. Capped at
    /// [`MAX_TIMING_SAMPLES`] (10) by the rolling window.
    pub fn sample_count(&self, characteristics: &ChunkCharacteristics) -> usize {
        self.timings.get(characteristics).map_or(0, |q| q.len())
    }

    /// Total number of samples across all buckets — useful as a "warmup
    /// progress" metric in the diagnostics header.
    pub fn total_sample_count(&self) -> usize {
        self.timings.values().map(|q| q.len()).sum()
    }

    /// Per-bucket diagnostic snapshot. One entry per characteristics bucket
    /// that has at least one sample. Sorted by `(chunk_type, waveform_type,
    /// channel_configuration, is_first_in_sweep)` for stable display order.
    pub fn per_bucket_stats(&self) -> Vec<BucketStats> {
        let mut out: Vec<BucketStats> = self
            .timings
            .iter()
            .filter_map(|(k, q)| {
                if q.is_empty() {
                    return None;
                }
                let n = q.len();
                let mean_duration_ms = q
                    .iter()
                    .map(|s| s.availability_interval.num_milliseconds())
                    .sum::<i64>()
                    / n as i64;
                let mut lags: Vec<i64> = q
                    .iter()
                    .filter_map(|s| s.availability_lag.map(|d| d.num_milliseconds()))
                    .collect();
                let median_lag_ms = if lags.is_empty() {
                    None
                } else {
                    lags.sort_unstable();
                    let mid = lags.len() / 2;
                    Some(if lags.len().is_multiple_of(2) {
                        (lags[mid - 1] + lags[mid]) / 2
                    } else {
                        lags[mid]
                    })
                };
                let lag_sample_count = lags.len();
                Some(BucketStats {
                    characteristics: *k,
                    sample_count: n,
                    mean_duration_ms,
                    median_lag_ms,
                    lag_sample_count,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            let ka = (
                chunk_type_order(a.characteristics.chunk_type),
                waveform_order(a.characteristics.waveform_type),
                channel_order(a.characteristics.channel_configuration),
                a.characteristics.is_first_in_sweep,
            );
            let kb = (
                chunk_type_order(b.characteristics.chunk_type),
                waveform_order(b.characteristics.waveform_type),
                channel_order(b.characteristics.channel_configuration),
                b.characteristics.is_first_in_sweep,
            );
            ka.cmp(&kb)
        });
        out
    }

    /// Serialize to a compact JSON string suitable for localStorage.
    ///
    /// The payload includes a schema version; `from_json` ignores payloads with
    /// a mismatched version so schema bumps cleanly invalidate old caches.
    pub fn to_json(&self) -> Option<String> {
        let dto = ChunkTimingStatsDto {
            version: PERSIST_SCHEMA_VERSION,
            timings: self
                .timings
                .iter()
                .filter_map(|(k, v)| {
                    let chars_dto = characteristics_to_dto(k)?;
                    let samples = v
                        .iter()
                        .map(|t| TimingStatDto {
                            availability_interval_ms: t.availability_interval.num_milliseconds(),
                            collection_interval_ms: t
                                .collection_interval
                                .map(|d| d.num_milliseconds()),
                            availability_lag_ms: t.availability_lag.map(|d| d.num_milliseconds()),
                            attempts: t.attempts,
                        })
                        .collect();
                    Some((chars_dto, samples))
                })
                .collect(),
        };
        serde_json::to_string(&dto).ok()
    }

    /// Deserialize from a JSON string previously produced by `to_json`.
    ///
    /// Returns `None` on any parse error or version mismatch. Individual samples
    /// with unrecognised enum variants are silently dropped — a corrupted entry
    /// should not poison the whole cache.
    pub fn from_json(raw: &str) -> Option<Self> {
        let dto: ChunkTimingStatsDto = serde_json::from_str(raw).ok()?;
        if dto.version != PERSIST_SCHEMA_VERSION {
            return None;
        }
        let mut timings: HashMap<ChunkCharacteristics, VecDeque<TimingStat>> = HashMap::new();
        for (chars_dto, samples) in dto.timings {
            let Some(chars) = characteristics_from_dto(&chars_dto) else {
                continue;
            };
            let mut queue: VecDeque<TimingStat> =
                VecDeque::with_capacity(samples.len().min(MAX_TIMING_SAMPLES));
            for sample in samples.into_iter().rev().take(MAX_TIMING_SAMPLES).rev() {
                queue.push_back(TimingStat {
                    availability_interval: Duration::milliseconds(sample.availability_interval_ms),
                    collection_interval: sample.collection_interval_ms.map(Duration::milliseconds),
                    availability_lag: sample.availability_lag_ms.map(Duration::milliseconds),
                    attempts: sample.attempts,
                });
            }
            if !queue.is_empty() {
                timings.insert(chars, queue);
            }
        }
        Some(Self { timings })
    }
}

// ── Persistence DTOs ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct ChunkTimingStatsDto {
    version: u32,
    timings: Vec<(ChunkCharacteristicsDto, Vec<TimingStatDto>)>,
}

#[derive(Serialize, Deserialize)]
struct ChunkCharacteristicsDto {
    chunk_type: String,
    waveform_type: String,
    channel_configuration: String,
    is_first_in_sweep: bool,
}

#[derive(Serialize, Deserialize)]
struct TimingStatDto {
    availability_interval_ms: i64,
    #[serde(default)]
    collection_interval_ms: Option<i64>,
    #[serde(default)]
    availability_lag_ms: Option<i64>,
    attempts: usize,
}

fn characteristics_to_dto(c: &ChunkCharacteristics) -> Option<ChunkCharacteristicsDto> {
    Some(ChunkCharacteristicsDto {
        chunk_type: chunk_type_to_str(c.chunk_type).to_string(),
        waveform_type: waveform_type_to_str(c.waveform_type).to_string(),
        channel_configuration: channel_configuration_to_str(c.channel_configuration).to_string(),
        is_first_in_sweep: c.is_first_in_sweep,
    })
}

fn characteristics_from_dto(dto: &ChunkCharacteristicsDto) -> Option<ChunkCharacteristics> {
    Some(ChunkCharacteristics {
        chunk_type: chunk_type_from_str(&dto.chunk_type)?,
        waveform_type: waveform_type_from_str(&dto.waveform_type)?,
        channel_configuration: channel_configuration_from_str(&dto.channel_configuration)?,
        is_first_in_sweep: dto.is_first_in_sweep,
    })
}

fn chunk_type_to_str(t: ChunkType) -> &'static str {
    match t {
        ChunkType::Start => "S",
        ChunkType::Intermediate => "I",
        ChunkType::End => "E",
    }
}

fn chunk_type_from_str(s: &str) -> Option<ChunkType> {
    match s {
        "S" => Some(ChunkType::Start),
        "I" => Some(ChunkType::Intermediate),
        "E" => Some(ChunkType::End),
        _ => None,
    }
}

fn waveform_type_to_str(t: WaveformType) -> &'static str {
    match t {
        WaveformType::CS => "CS",
        WaveformType::CDW => "CDW",
        WaveformType::CDWO => "CDWO",
        WaveformType::B => "B",
        WaveformType::SPP => "SPP",
        WaveformType::Unknown => "Unknown",
    }
}

fn waveform_type_from_str(s: &str) -> Option<WaveformType> {
    match s {
        "CS" => Some(WaveformType::CS),
        "CDW" => Some(WaveformType::CDW),
        "CDWO" => Some(WaveformType::CDWO),
        "B" => Some(WaveformType::B),
        "SPP" => Some(WaveformType::SPP),
        "Unknown" => Some(WaveformType::Unknown),
        _ => None,
    }
}

fn channel_configuration_to_str(c: ChannelConfiguration) -> &'static str {
    match c {
        ChannelConfiguration::ConstantPhase => "constant_phase",
        ChannelConfiguration::RandomPhase => "random_phase",
        ChannelConfiguration::SZ2Phase => "sz2_phase",
        ChannelConfiguration::UnknownPhase => "unknown_phase",
    }
}

fn channel_configuration_from_str(s: &str) -> Option<ChannelConfiguration> {
    match s {
        "constant_phase" => Some(ChannelConfiguration::ConstantPhase),
        "random_phase" => Some(ChannelConfiguration::RandomPhase),
        "sz2_phase" => Some(ChannelConfiguration::SZ2Phase),
        "unknown_phase" => Some(ChannelConfiguration::UnknownPhase),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn bucket() -> ChunkCharacteristics {
        ChunkCharacteristics {
            chunk_type: ChunkType::Intermediate,
            waveform_type: WaveformType::CS,
            channel_configuration: ChannelConfiguration::ConstantPhase,
            is_first_in_sweep: false,
        }
    }

    #[wasm_bindgen_test]
    fn average_collection_interval_ignores_samples_without_one() {
        let mut stats = ChunkTimingStats::new();
        let b = bucket();
        // Two availability-only samples → no collection average.
        stats.add_timing(b, Duration::seconds(10), None, 1);
        stats.add_timing(b, Duration::seconds(12), None, 1);
        assert_eq!(stats.average_collection_interval(&b), None);
        // Attach a collection interval to the latest sample → averaged over
        // the carrying samples only.
        stats.attach_collection_interval(&b, Duration::seconds(4));
        assert_eq!(
            stats.average_collection_interval(&b),
            Some(Duration::seconds(4))
        );
        // The availability average is unaffected and stays in its own domain.
        assert_eq!(
            stats.average_availability_interval(&b),
            Some(Duration::seconds(11))
        );
    }

    #[wasm_bindgen_test]
    fn persist_round_trips_collection_interval_at_current_version() {
        let mut stats = ChunkTimingStats::new();
        let b = bucket();
        stats.add_timing(b, Duration::seconds(10), Some(Duration::seconds(3)), 2);
        stats.attach_collection_interval(&b, Duration::seconds(9));

        let json = stats.to_json().expect("serializes");
        let back = ChunkTimingStats::from_json(&json).expect("parses at current version");
        assert_eq!(
            back.average_collection_interval(&b),
            Some(Duration::seconds(9))
        );
        assert_eq!(
            back.average_availability_interval(&b),
            Some(Duration::seconds(10))
        );
        assert_eq!(back.median_availability_lag_secs(), Some(3.0));
    }

    #[wasm_bindgen_test]
    fn chunk_type_is_part_of_the_bucket_key() {
        // Regression pin for the End-chunk bucket-key mismatch: the WRITE path
        // (`Projector::characteristics_for_sequence`) and the READ path
        // (`interval_estimate::chunk_characteristics`) must agree on
        // `chunk_type`, because `ChunkCharacteristics`'s Hash/Eq distinguishes
        // it. A sample stored under one `chunk_type` is invisible under
        // another — so if the write path bucketed the volume's final chunk
        // under `End` while the read path always queries `Intermediate`, the
        // final hop's history would be silently unreadable.
        let mut stats = ChunkTimingStats::new();
        let mut end_bucket = bucket();
        end_bucket.chunk_type = ChunkType::End;
        stats.add_timing(end_bucket, Duration::seconds(7), None, 1);
        stats.attach_collection_interval(&end_bucket, Duration::seconds(7));

        // Same metadata, only `chunk_type` differs → a distinct bucket. The
        // End sample is NOT visible from the Intermediate key the read path
        // builds. This is the mismatch the projector fix prevents by
        // normalizing the write-side `chunk_type` to `Intermediate`.
        let mut intermediate_bucket = end_bucket;
        intermediate_bucket.chunk_type = ChunkType::Intermediate;
        assert_eq!(stats.sample_count(&intermediate_bucket), 0);
        assert_eq!(
            stats.average_collection_interval(&intermediate_bucket),
            None
        );

        // The End bucket itself round-trips: written stats ARE retrievable
        // when read under the matching key.
        assert_eq!(stats.sample_count(&end_bucket), 1);
        assert_eq!(
            stats.average_collection_interval(&end_bucket),
            Some(Duration::seconds(7))
        );

        // After the fix the write path stores the final chunk under
        // `Intermediate`, so a same-metadata Intermediate write IS read back.
        let mut stats2 = ChunkTimingStats::new();
        stats2.add_timing(intermediate_bucket, Duration::seconds(9), None, 1);
        stats2.attach_collection_interval(&intermediate_bucket, Duration::seconds(9));
        assert_eq!(
            stats2.average_collection_interval(&intermediate_bucket),
            Some(Duration::seconds(9))
        );
    }

    #[wasm_bindgen_test]
    fn persist_rejects_previous_schema_version() {
        // A v2 payload (pre-domain-split shape) must be discarded — its
        // `duration_ms` samples can't be reinterpreted as collection
        // intervals.
        let v2 = r#"{"version":2,"timings":[[{"chunk_type":"I","waveform_type":"CS","channel_configuration":"constant_phase","is_first_in_sweep":false},[{"duration_ms":10000,"availability_lag_ms":null,"attempts":1}]]]}"#;
        assert!(ChunkTimingStats::from_json(v2).is_none());
    }
}
