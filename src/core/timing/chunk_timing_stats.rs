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
pub(crate) struct ChunkCharacteristics {
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
pub(crate) struct BucketStats {
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

/// Median of a slice of `i64`, or `None` when empty. Sorts `vals` in place.
/// Even-length inputs average the two middle elements (integer division,
/// matching the historical per-site math). Shared by
/// [`ChunkTimingStats::median_availability_lag_secs`] and
/// [`ChunkTimingStats::per_bucket_stats`].
fn median_i64(vals: &mut [i64]) -> Option<i64> {
    if vals.is_empty() {
        return None;
    }
    vals.sort_unstable();
    let mid = vals.len() / 2;
    Some(if vals.len().is_multiple_of(2) {
        (vals[mid - 1] + vals[mid]) / 2
    } else {
        vals[mid]
    })
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
pub(crate) struct ChunkTimingStats {
    /// Timing statistics for each chunk characteristics
    timings: HashMap<ChunkCharacteristics, VecDeque<TimingStat>>,
}

impl ChunkTimingStats {
    /// Create a new empty timing statistics
    pub(crate) fn new() -> Self {
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
    pub(crate) fn add_timing(
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
    pub(crate) fn attach_collection_interval(
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
    pub(crate) fn attach_availability_lag(
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
    pub(crate) fn median_availability_lag_secs(&self) -> Option<f64> {
        let mut lags_ms: Vec<i64> = self
            .timings
            .values()
            .flat_map(|queue| queue.iter())
            .filter_map(|stat| stat.availability_lag.map(|d| d.num_milliseconds()))
            .collect();
        let median_ms = median_i64(&mut lags_ms)?;
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
    pub(crate) fn get_statistics(
        &self,
    ) -> Vec<(ChunkCharacteristics, Option<Duration>, Option<f64>)> {
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
    pub(crate) fn sample_count(&self, characteristics: &ChunkCharacteristics) -> usize {
        self.timings.get(characteristics).map_or(0, |q| q.len())
    }

    /// Total number of samples across all buckets — useful as a "warmup
    /// progress" metric in the diagnostics header.
    pub(crate) fn total_sample_count(&self) -> usize {
        self.timings.values().map(|q| q.len()).sum()
    }

    /// Per-bucket diagnostic snapshot. One entry per characteristics bucket
    /// that has at least one sample. Sorted by `(chunk_type, waveform_type,
    /// channel_configuration, is_first_in_sweep)` for stable display order.
    pub(crate) fn per_bucket_stats(&self) -> Vec<BucketStats> {
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
                let lag_sample_count = lags.len();
                let median_lag_ms = median_i64(&mut lags);
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
    pub(crate) fn to_json(&self) -> Option<String> {
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
    pub(crate) fn from_json(raw: &str) -> Option<Self> {
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
    fn median_i64_edge_cases() {
        // Empty → None.
        assert_eq!(median_i64(&mut []), None);
        // Single element.
        assert_eq!(median_i64(&mut [7]), Some(7));
        // Odd length → middle of the sorted values (input order is irrelevant).
        assert_eq!(median_i64(&mut [9, 1, 5]), Some(5));
        // Even length → integer-division average of the two middle elements.
        // sorted [2,4,6,8] → (4+6)/2 = 5.
        assert_eq!(median_i64(&mut [8, 2, 6, 4]), Some(5));
        // Even-length average truncates toward zero. sorted [1,2] → (1+2)/2 = 1.
        assert_eq!(median_i64(&mut [2, 1]), Some(1));
        // Negative lags (clock skew) sort correctly. sorted [-10,-3,-1] → -3.
        assert_eq!(median_i64(&mut [-1, -10, -3]), Some(-3));
        // Mixed signs, even length. sorted [-4,-2,1,3] → (-2+1)/2 = 0 (trunc).
        assert_eq!(median_i64(&mut [3, -4, 1, -2]), Some(0));
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

    #[wasm_bindgen_test]
    fn from_json_trims_to_newest_window_in_order() {
        // A persisted bucket with MORE than MAX_TIMING_SAMPLES samples must
        // keep exactly the newest MAX, in their original order. We encode the
        // sample index in `attempts` so we can verify which survived and that
        // ordering is preserved (oldest dropped from the front).
        assert_eq!(MAX_TIMING_SAMPLES, 10); // pin the assumed window size
        let n = MAX_TIMING_SAMPLES + 5; // 15 samples
        let samples: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    r#"{{"availability_interval_ms":{},"collection_interval_ms":null,"availability_lag_ms":null,"attempts":{}}}"#,
                    1000 + i,
                    i
                )
            })
            .collect();
        let json = format!(
            r#"{{"version":3,"timings":[[{{"chunk_type":"I","waveform_type":"CS","channel_configuration":"constant_phase","is_first_in_sweep":false}},[{}]]]}}"#,
            samples.join(",")
        );

        let stats = ChunkTimingStats::from_json(&json).expect("parses");
        let b = bucket();
        // Exactly the newest MAX kept.
        assert_eq!(stats.sample_count(&b), MAX_TIMING_SAMPLES);
        // The kept samples are indices 5..15 (the newest 10), in order. The
        // queue's `attempts` should therefore start at 5 and end at 14.
        let q = stats.timings.get(&b).expect("bucket present");
        assert_eq!(q.front().unwrap().attempts, n - MAX_TIMING_SAMPLES); // 5
        assert_eq!(q.back().unwrap().attempts, n - 1); // 14
                                                       // Strictly increasing → original order preserved, front is oldest-kept.
        let attempts: Vec<usize> = q.iter().map(|s| s.attempts).collect();
        assert!(attempts.windows(2).all(|w| w[0] + 1 == w[1]));
    }

    #[wasm_bindgen_test]
    fn from_json_drops_only_the_corrupt_bucket() {
        // One bucket carries a bogus waveform_type string; a valid sibling
        // bucket must survive. The corrupt entry should not poison the cache.
        let json = r#"{"version":3,"timings":[
            [{"chunk_type":"I","waveform_type":"NOPE","channel_configuration":"constant_phase","is_first_in_sweep":false},[{"availability_interval_ms":8000,"collection_interval_ms":null,"availability_lag_ms":null,"attempts":1}]],
            [{"chunk_type":"I","waveform_type":"CS","channel_configuration":"constant_phase","is_first_in_sweep":false},[{"availability_interval_ms":9000,"collection_interval_ms":null,"availability_lag_ms":null,"attempts":1}]]
        ]}"#;
        let stats = ChunkTimingStats::from_json(json).expect("parses");
        // The valid sibling (CS) survives.
        let valid = bucket();
        assert_eq!(stats.sample_count(&valid), 1);
        assert_eq!(
            stats.average_availability_interval(&valid),
            Some(Duration::seconds(9))
        );
        // The corrupt bucket left no other entry behind — only the one valid
        // bucket exists.
        assert_eq!(stats.total_sample_count(), 1);
    }

    #[wasm_bindgen_test]
    fn from_json_all_invalid_bucket_yields_no_entry() {
        // A bucket whose every sample fails to parse leaves an empty queue,
        // which must not be inserted. Here we make the whole bucket invalid via
        // an unrecognized channel_configuration so `characteristics_from_dto`
        // returns None and the bucket is skipped entirely.
        let json = r#"{"version":3,"timings":[
            [{"chunk_type":"I","waveform_type":"CS","channel_configuration":"bogus_phase","is_first_in_sweep":false},[{"availability_interval_ms":8000,"collection_interval_ms":null,"availability_lag_ms":null,"attempts":1}]]
        ]}"#;
        let stats = ChunkTimingStats::from_json(json).expect("parses");
        assert_eq!(stats.total_sample_count(), 0);
        assert!(stats.get_statistics().is_empty());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn cs_bucket() -> ChunkCharacteristics {
        ChunkCharacteristics {
            chunk_type: ChunkType::Intermediate,
            waveform_type: WaveformType::CS,
            channel_configuration: ChannelConfiguration::ConstantPhase,
            is_first_in_sweep: false,
        }
    }

    fn other_bucket() -> ChunkCharacteristics {
        ChunkCharacteristics {
            chunk_type: ChunkType::Start,
            waveform_type: WaveformType::CDW,
            channel_configuration: ChannelConfiguration::RandomPhase,
            is_first_in_sweep: true,
        }
    }

    #[wasm_bindgen_test]
    fn median_lag_none_until_lag_recorded() {
        let mut stats = ChunkTimingStats::new();
        let b = cs_bucket();
        // Availability-only samples carry no lag → still None.
        stats.add_timing(b, Duration::seconds(10), None, 1);
        stats.add_timing(b, Duration::seconds(12), None, 1);
        assert_eq!(stats.median_availability_lag_secs(), None);
        // Empty store is also None.
        assert_eq!(ChunkTimingStats::new().median_availability_lag_secs(), None);
    }

    #[wasm_bindgen_test]
    fn median_lag_across_buckets() {
        let mut stats = ChunkTimingStats::new();
        let a = cs_bucket();
        let b = other_bucket();
        // Lags (ms): 1000, 3000 in one bucket; 9000 in the other.
        // Pooled sorted [1000,3000,9000] → median 3000ms = 3.0s.
        stats.add_timing(a, Duration::seconds(5), Some(Duration::seconds(1)), 1);
        stats.add_timing(a, Duration::seconds(5), Some(Duration::seconds(3)), 1);
        stats.add_timing(b, Duration::seconds(5), Some(Duration::seconds(9)), 1);
        let got = stats.median_availability_lag_secs().expect("has lag");
        assert!((got - 3.0).abs() < 1e-9, "got {got}");
    }

    #[wasm_bindgen_test]
    fn attach_availability_lag_updates_latest_only() {
        let mut stats = ChunkTimingStats::new();
        let b = cs_bucket();
        stats.add_timing(b, Duration::seconds(5), None, 1);
        stats.add_timing(b, Duration::seconds(5), None, 1);
        // Attaches to the most recent sample only → one lag of 8000ms.
        stats.attach_availability_lag(&b, Duration::seconds(8));
        // Single recorded lag → median is that lag.
        let got = stats.median_availability_lag_secs().expect("has lag");
        assert!((got - 8.0).abs() < 1e-9, "got {got}");
    }

    #[wasm_bindgen_test]
    fn attach_on_missing_bucket_is_noop() {
        let mut stats = ChunkTimingStats::new();
        let b = cs_bucket();
        // No samples added yet — attach must not panic and must not create a bucket.
        stats.attach_collection_interval(&b, Duration::seconds(4));
        stats.attach_availability_lag(&b, Duration::seconds(4));
        assert_eq!(stats.sample_count(&b), 0);
        assert_eq!(stats.total_sample_count(), 0);
        assert_eq!(stats.median_availability_lag_secs(), None);
    }

    #[wasm_bindgen_test]
    fn average_attempts_and_missing_bucket() {
        let mut stats = ChunkTimingStats::new();
        let b = cs_bucket();
        stats.add_timing(b, Duration::seconds(1), None, 2);
        stats.add_timing(b, Duration::seconds(1), None, 5);
        // (2 + 5) / 2 = 3.5
        let avg = stats.get_average_attempts(&b).expect("has attempts");
        assert!((avg - 3.5).abs() < 1e-9, "got {avg}");
        // A never-populated bucket → None for all aggregates.
        let missing = other_bucket();
        assert_eq!(stats.get_average_attempts(&missing), None);
        assert_eq!(stats.average_availability_interval(&missing), None);
        assert_eq!(stats.average_collection_interval(&missing), None);
    }

    #[wasm_bindgen_test]
    fn rolling_window_caps_at_max_samples() {
        let mut stats = ChunkTimingStats::new();
        let b = cs_bucket();
        // Push more than MAX_TIMING_SAMPLES; oldest are evicted from the front.
        for i in 0..(MAX_TIMING_SAMPLES + 4) {
            stats.add_timing(b, Duration::milliseconds(1000 + i as i64), None, i);
        }
        assert_eq!(stats.sample_count(&b), MAX_TIMING_SAMPLES);
        assert_eq!(stats.total_sample_count(), MAX_TIMING_SAMPLES);
    }

    #[wasm_bindgen_test]
    fn total_sample_count_sums_buckets() {
        let mut stats = ChunkTimingStats::new();
        let a = cs_bucket();
        let b = other_bucket();
        stats.add_timing(a, Duration::seconds(1), None, 1);
        stats.add_timing(a, Duration::seconds(1), None, 1);
        stats.add_timing(b, Duration::seconds(1), None, 1);
        assert_eq!(stats.sample_count(&a), 2);
        assert_eq!(stats.sample_count(&b), 1);
        assert_eq!(stats.total_sample_count(), 3);
    }

    #[wasm_bindgen_test]
    fn average_availability_interval_integer_truncation() {
        let mut stats = ChunkTimingStats::new();
        let b = cs_bucket();
        // 1000ms + 1001ms → sum 2001 / 2 = 1000ms (integer division truncates).
        stats.add_timing(b, Duration::milliseconds(1000), None, 1);
        stats.add_timing(b, Duration::milliseconds(1001), None, 1);
        assert_eq!(
            stats.average_availability_interval(&b),
            Some(Duration::milliseconds(1000))
        );
    }

    #[wasm_bindgen_test]
    fn per_bucket_stats_empty_when_no_samples() {
        let stats = ChunkTimingStats::new();
        assert!(stats.per_bucket_stats().is_empty());
    }

    #[wasm_bindgen_test]
    fn per_bucket_stats_fields_and_lag_counting() {
        let mut stats = ChunkTimingStats::new();
        let b = cs_bucket();
        // Three availability intervals: 2000, 4000, 6000 → mean 4000ms.
        // Two of them carry a lag (1000, 3000) → median 2000ms, lag_sample_count 2.
        stats.add_timing(
            b,
            Duration::milliseconds(2000),
            Some(Duration::seconds(1)),
            1,
        );
        stats.add_timing(
            b,
            Duration::milliseconds(4000),
            Some(Duration::seconds(3)),
            1,
        );
        stats.add_timing(b, Duration::milliseconds(6000), None, 1);
        let buckets = stats.per_bucket_stats();
        assert_eq!(buckets.len(), 1);
        let bs = &buckets[0];
        assert_eq!(bs.characteristics, b);
        assert_eq!(bs.sample_count, 3);
        assert_eq!(bs.mean_duration_ms, 4000);
        assert_eq!(bs.lag_sample_count, 2);
        assert_eq!(bs.median_lag_ms, Some(2000));
    }

    #[wasm_bindgen_test]
    fn per_bucket_stats_median_lag_none_without_lags() {
        let mut stats = ChunkTimingStats::new();
        let b = cs_bucket();
        stats.add_timing(b, Duration::seconds(5), None, 1);
        let buckets = stats.per_bucket_stats();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].median_lag_ms, None);
        assert_eq!(buckets[0].lag_sample_count, 0);
    }

    #[wasm_bindgen_test]
    fn per_bucket_stats_sorted_by_key_tuple() {
        let mut stats = ChunkTimingStats::new();
        // other_bucket is chunk_type=Start (order 0); cs_bucket is Intermediate (order 1).
        // Start sorts before Intermediate regardless of insertion order.
        let a = cs_bucket();
        let b = other_bucket();
        stats.add_timing(a, Duration::seconds(1), None, 1);
        stats.add_timing(b, Duration::seconds(1), None, 1);
        let buckets = stats.per_bucket_stats();
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].characteristics.chunk_type, ChunkType::Start);
        assert_eq!(
            buckets[1].characteristics.chunk_type,
            ChunkType::Intermediate
        );
    }

    #[wasm_bindgen_test]
    fn get_statistics_reports_each_bucket() {
        let mut stats = ChunkTimingStats::new();
        let a = cs_bucket();
        let b = other_bucket();
        stats.add_timing(a, Duration::seconds(10), None, 3);
        stats.add_timing(b, Duration::seconds(20), None, 1);
        let mut rows = stats.get_statistics();
        assert_eq!(rows.len(), 2);
        // Find the cs_bucket row and check its derived values.
        rows.sort_by_key(|(c, _, _)| chunk_type_order(c.chunk_type));
        // Start bucket first (order 0): 20s interval, 1 attempt.
        let (c0, dur0, att0) = &rows[0];
        assert_eq!(c0.chunk_type, ChunkType::Start);
        assert_eq!(*dur0, Some(Duration::seconds(20)));
        assert!((att0.expect("attempts") - 1.0).abs() < 1e-9);
        // Intermediate bucket: 10s interval, 3 attempts.
        let (c1, dur1, att1) = &rows[1];
        assert_eq!(c1.chunk_type, ChunkType::Intermediate);
        assert_eq!(*dur1, Some(Duration::seconds(10)));
        assert!((att1.expect("attempts") - 3.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn json_round_trips_multiple_buckets_and_lag() {
        let mut stats = ChunkTimingStats::new();
        let a = cs_bucket();
        let b = other_bucket();
        stats.add_timing(a, Duration::seconds(10), Some(Duration::seconds(2)), 1);
        stats.add_timing(b, Duration::seconds(15), Some(Duration::seconds(6)), 4);
        let json = stats.to_json().expect("serializes");
        let back = ChunkTimingStats::from_json(&json).expect("parses");
        assert_eq!(back.total_sample_count(), 2);
        assert_eq!(
            back.average_availability_interval(&a),
            Some(Duration::seconds(10))
        );
        assert_eq!(
            back.average_availability_interval(&b),
            Some(Duration::seconds(15))
        );
        // Pooled lags [2000, 6000] → median (2000+6000)/2 = 4000ms = 4.0s.
        let med = back.median_availability_lag_secs().expect("has lag");
        assert!((med - 4.0).abs() < 1e-9, "got {med}");
        // Attempts preserved on the other bucket.
        let att = back.get_average_attempts(&b).expect("attempts");
        assert!((att - 4.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn empty_stats_to_json_round_trips_to_empty() {
        let stats = ChunkTimingStats::new();
        let json = stats.to_json().expect("serializes empty");
        let back = ChunkTimingStats::from_json(&json).expect("parses empty");
        assert_eq!(back.total_sample_count(), 0);
        assert!(back.per_bucket_stats().is_empty());
    }
}
