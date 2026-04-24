use chrono::Duration;
use nexrad_data::aws::realtime::ChunkType;
use nexrad_decode::messages::volume_coverage_pattern::{ChannelConfiguration, WaveformType};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

/// Maximum number of timing samples to keep per chunk characteristics
const MAX_TIMING_SAMPLES: usize = 10;

/// Schema version for the serialized `ChunkTimingStats` payload in localStorage.
/// Bump when the on-disk shape changes so old caches are ignored rather than
/// silently misinterpreted.
///
/// v2 added `availability_lag_ms` to every sample; v1 payloads are discarded
/// because the field can't be backfilled from S3 deltas alone.
const PERSIST_SCHEMA_VERSION: u32 = 2;

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

/// Statistics for a single timing sample.
///
/// `duration` is the legacy S3-upload→S3-upload delta (AVAILABILITY-to-
/// AVAILABILITY), used by the scheduler via `get_average_timing`.
/// `availability_lag` is the empirical (upload − ACTUAL collection) delay
/// for this chunk when the collection time could be recorded; used by the
/// projector to estimate the availability-lag tail per characteristics.
/// `None` indicates the sample predates collection-time plumbing.
#[derive(Debug, Clone, Copy)]
pub(super) struct TimingStat {
    duration: Duration,
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
    /// worker ingest, `None` when only the S3 delta is known.
    pub fn add_timing(
        &mut self,
        characteristics: ChunkCharacteristics,
        duration: Duration,
        availability_lag: Option<Duration>,
        attempts: usize,
    ) {
        let entry = self.timings.entry(characteristics).or_default();

        entry.push_back(TimingStat {
            duration,
            availability_lag,
            attempts,
        });

        // Maintain the rolling window by removing oldest if we exceed the max
        if entry.len() > MAX_TIMING_SAMPLES {
            entry.pop_front();
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

    /// Get the average timing for the given chunk characteristics
    pub(super) fn get_average_timing(
        &self,
        characteristics: &ChunkCharacteristics,
    ) -> Option<Duration> {
        self.timings.get(characteristics).and_then(|timings| {
            if timings.is_empty() {
                return None;
            }

            let total_millis: i64 = timings
                .iter()
                .map(|timing| timing.duration.num_milliseconds())
                .sum();

            let avg_millis = total_millis / timings.len() as i64;
            Some(Duration::milliseconds(avg_millis))
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
                    self.get_average_timing(characteristics),
                    self.get_average_attempts(characteristics),
                )
            })
            .collect()
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
                            duration_ms: t.duration.num_milliseconds(),
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
                    duration: Duration::milliseconds(sample.duration_ms),
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
    duration_ms: i64,
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
