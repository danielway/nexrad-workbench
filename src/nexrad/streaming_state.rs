//! Replacement for `nexrad_data::aws::realtime::ChunkIterator` that lets us
//! supply our own volume discovery (see [`super::volume_discovery`]) instead of
//! the library's sequential binary search.
//!
//! This mirrors the subset of `ChunkIterator` that `realtime.rs` actually uses:
//! init (fetch latest + optional start chunk, extract VCP), pull-based
//! `try_next`, and timing/metadata accessors.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use log::debug;
use nexrad_data::aws::realtime::{
    download_chunk, estimate_chunk_availability_time, list_chunks_in_volume, project_scan_timing,
    Chunk, ChunkCharacteristics, ChunkIdentifier, ChunkMetadata, ChunkTimingStats, ChunkType,
    DownloadedChunk, ElevationChunkMapper, NextChunk, ScanTimingProjection, VolumeIndex,
};
use nexrad_data::result::{aws::AWSError, Error, Result};
use nexrad_decode::messages::volume_coverage_pattern;

/// Fetched at init: the latest chunk in the volume, plus the Start chunk if
/// we joined mid-volume. Mirrors `nexrad_data::aws::realtime::ChunkIteratorInit`.
#[derive(Debug)]
pub struct StreamingInit {
    pub state: StreamingState,
    pub latest_chunk: DownloadedChunk,
    pub start_chunk: Option<DownloadedChunk>,
}

/// Tracks the state of an ongoing real-time stream. Replaces `ChunkIterator`.
#[derive(Debug)]
pub struct StreamingState {
    site: String,
    current: ChunkIdentifier,
    elevation_mapper: Option<ElevationChunkMapper>,
    vcp: Option<volume_coverage_pattern::Message<'static>>,
    timing_stats: ChunkTimingStats,
    last_chunk_time: Option<DateTime<Utc>>,
    requests_made: usize,
    bytes_downloaded: u64,
}

impl StreamingState {
    /// Initializes a stream at the given volume. Lists chunks, downloads the
    /// latest, and (if mid-volume) downloads the Start chunk to extract the VCP.
    ///
    /// `prior_requests` counts requests already made during volume discovery so
    /// the iterator's `requests_made()` reflects total session cost.
    pub async fn init_at_volume(
        site: &str,
        volume: VolumeIndex,
        prior_requests: usize,
    ) -> Result<StreamingInit> {
        let chunks = list_chunks_in_volume(site, volume, 100).await?;
        let mut requests_made = prior_requests + 1;

        let latest_id = chunks.last().ok_or(AWSError::ExpectedChunkNotFound)?;
        let (latest_id, latest_chunk) = download_chunk(site, latest_id).await?;
        requests_made += 1;
        let mut bytes_downloaded = latest_chunk.data().len() as u64;

        let mut elevation_mapper = None;
        let mut vcp = None;
        let mut start_chunk_download: Option<DownloadedChunk> = None;

        if latest_id.chunk_type() == ChunkType::Start {
            // Latest IS the Start chunk — extract VCP from it.
            if let Ok(v) = extract_vcp(&latest_chunk) {
                elevation_mapper = Some(ElevationChunkMapper::new(&v));
                vcp = Some(v);
            }
        } else {
            // Mid-volume join — fetch the Start chunk (sequence 1) separately.
            let start_id = ChunkIdentifier::new(
                site.to_string(),
                volume,
                *latest_id.date_time_prefix(),
                1,
                ChunkType::Start,
                None,
            );
            if let Ok((sid, schunk)) = download_chunk(site, &start_id).await {
                requests_made += 1;
                bytes_downloaded += schunk.data().len() as u64;
                if let Ok(v) = extract_vcp(&schunk) {
                    elevation_mapper = Some(ElevationChunkMapper::new(&v));
                    vcp = Some(v);
                }
                start_chunk_download = Some(DownloadedChunk {
                    identifier: sid,
                    chunk: schunk,
                    attempts: 1,
                });
            }
        }

        let last_chunk_time = latest_id.upload_date_time();
        let state = StreamingState {
            site: site.to_string(),
            current: latest_id.clone(),
            elevation_mapper,
            vcp,
            timing_stats: ChunkTimingStats::new(),
            last_chunk_time,
            requests_made,
            bytes_downloaded,
        };

        Ok(StreamingInit {
            state,
            latest_chunk: DownloadedChunk {
                identifier: latest_id,
                chunk: latest_chunk,
                attempts: 1,
            },
            start_chunk: start_chunk_download,
        })
    }

    /// Attempts to fetch the next chunk.
    /// - `Ok(Some(chunk))` — downloaded
    /// - `Ok(None)` — not yet available, caller should wait and retry
    /// - `Err(...)` — unrecoverable error
    pub async fn try_next(&mut self) -> Result<Option<DownloadedChunk>> {
        let mapper = self
            .elevation_mapper
            .as_ref()
            .ok_or(AWSError::FailedToDetermineNextChunk)?;
        let next = self
            .current
            .next_chunk(mapper)
            .ok_or(AWSError::FailedToDetermineNextChunk)?;

        match next {
            NextChunk::Sequence(next_id) => self.try_fetch_chunk(next_id).await,
            NextChunk::Volume(next_volume) => self.try_fetch_volume_start(next_volume).await,
        }
    }

    async fn try_fetch_chunk(
        &mut self,
        chunk_id: ChunkIdentifier,
    ) -> Result<Option<DownloadedChunk>> {
        self.requests_made += 1;
        match download_chunk(&self.site, &chunk_id).await {
            Ok((identifier, chunk)) => {
                self.bytes_downloaded += chunk.data().len() as u64;

                if identifier.chunk_type() == ChunkType::Start {
                    if let Ok(v) = extract_vcp(&chunk) {
                        self.elevation_mapper = Some(ElevationChunkMapper::new(&v));
                        self.vcp = Some(v);
                    }
                }

                if let (Some(upload), Some(prev)) =
                    (identifier.upload_date_time(), self.last_chunk_time)
                {
                    self.update_timing_stats(&identifier, upload - prev, 1);
                }

                self.last_chunk_time = identifier.upload_date_time();
                self.current = identifier.clone();

                Ok(Some(DownloadedChunk {
                    identifier,
                    chunk,
                    attempts: 1,
                }))
            }
            Err(Error::AWS(AWSError::S3ObjectNotFound)) => {
                debug!("Chunk {} not yet available", chunk_id.name());
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn try_fetch_volume_start(
        &mut self,
        volume: VolumeIndex,
    ) -> Result<Option<DownloadedChunk>> {
        let chunks = list_chunks_in_volume(&self.site, volume, 100).await?;
        self.requests_made += 1;
        let latest = match chunks.last() {
            Some(id) => id,
            None => return Ok(None), // Volume hasn't started yet
        };
        let (identifier, chunk) = download_chunk(&self.site, latest).await?;
        self.requests_made += 1;
        self.bytes_downloaded += chunk.data().len() as u64;

        if identifier.chunk_type() == ChunkType::Start {
            if let Ok(v) = extract_vcp(&chunk) {
                self.elevation_mapper = Some(ElevationChunkMapper::new(&v));
                self.vcp = Some(v);
            }
        } else if self.elevation_mapper.is_none() {
            // Joined mid-volume without a mapper — fetch the Start chunk too.
            let start_id = ChunkIdentifier::new(
                self.site.clone(),
                volume,
                *identifier.date_time_prefix(),
                1,
                ChunkType::Start,
                None,
            );
            if let Ok((_, start_chunk)) = download_chunk(&self.site, &start_id).await {
                self.requests_made += 1;
                self.bytes_downloaded += start_chunk.data().len() as u64;
                if let Ok(v) = extract_vcp(&start_chunk) {
                    self.elevation_mapper = Some(ElevationChunkMapper::new(&v));
                    self.vcp = Some(v);
                }
            }
        }

        if let (Some(upload), Some(prev)) = (identifier.upload_date_time(), self.last_chunk_time) {
            self.update_timing_stats(&identifier, upload - prev, 1);
        }

        self.last_chunk_time = identifier.upload_date_time();
        self.current = identifier.clone();

        Ok(Some(DownloadedChunk {
            identifier,
            chunk,
            attempts: 1,
        }))
    }

    fn update_timing_stats(
        &mut self,
        chunk_id: &ChunkIdentifier,
        duration: ChronoDuration,
        attempts: usize,
    ) {
        if let (Some(vcp), Some(mapper)) = (&self.vcp, &self.elevation_mapper) {
            if let Some(elevation) = mapper
                .get_sequence_elevation_number(chunk_id.sequence())
                .and_then(|n| vcp.elevations().get(n - 1))
            {
                let characteristics = ChunkCharacteristics {
                    chunk_type: chunk_id.chunk_type(),
                    waveform_type: elevation.waveform_type(),
                    channel_configuration: elevation.channel_configuration(),
                };
                self.timing_stats
                    .add_timing(characteristics, duration, attempts);
            }
        }
    }

    pub fn next_expected_time(&self) -> Option<DateTime<Utc>> {
        let vcp = self.vcp.as_ref()?;
        let mapper = self.elevation_mapper.as_ref()?;
        estimate_chunk_availability_time(&self.current, vcp, mapper, Some(&self.timing_stats))
    }

    pub fn time_until_next(&self) -> Option<ChronoDuration> {
        let expected = self.next_expected_time()?;
        let now = Utc::now();
        if expected <= now {
            None
        } else {
            Some(expected - now)
        }
    }

    /// Whether the next chunk to fetch crosses a sweep or volume boundary.
    ///
    /// Returns `true` when the next chunk is the first of a new elevation cut
    /// or the Start of a new volume. Both cases share the same publishing
    /// behavior — extra processing/uplink latency that the upstream timing
    /// model under-budgets — so the polling loop can apply an additional
    /// wait on top of the library's prediction.
    ///
    /// Returns `false` when we can't determine the next chunk (no VCP yet,
    /// no mapper) — callers should treat this as "no extra budget".
    pub fn next_chunk_starts_sweep_or_volume(&self) -> bool {
        let Some(mapper) = self.elevation_mapper.as_ref() else {
            return false;
        };
        let Some(next) = self.current.next_chunk(mapper) else {
            return false;
        };
        match next {
            NextChunk::Volume(_) => true,
            NextChunk::Sequence(next_id) => mapper
                .get_chunk_metadata(next_id.sequence())
                .map(|m| m.is_first_in_sweep())
                .unwrap_or(false),
        }
    }

    pub fn chunk_metadata(&self, sequence: usize) -> Option<&ChunkMetadata> {
        self.elevation_mapper
            .as_ref()
            .and_then(|m| m.get_chunk_metadata(sequence))
    }

    pub fn all_chunk_metadata(&self) -> Option<&[ChunkMetadata]> {
        self.elevation_mapper
            .as_ref()
            .map(|m| m.all_chunk_metadata())
    }

    pub fn project_remaining_scan(&self) -> Option<ScanTimingProjection> {
        let vcp = self.vcp.as_ref()?;
        let mapper = self.elevation_mapper.as_ref()?;
        project_scan_timing(&self.current, vcp, mapper, Some(&self.timing_stats))
    }

    pub fn projected_volume_end_time(&self) -> Option<DateTime<Utc>> {
        self.project_remaining_scan().map(|p| p.volume_end_time())
    }

    pub fn requests_made(&self) -> usize {
        self.requests_made
    }

    pub fn bytes_downloaded(&self) -> u64 {
        self.bytes_downloaded
    }
}

fn extract_vcp(chunk: &Chunk) -> Result<volume_coverage_pattern::Message<'static>> {
    if let Chunk::Start(file) = chunk {
        for mut record in file.records()? {
            if record.compressed() {
                record = record.decompress()?;
            }
            for message in record.messages()? {
                if let nexrad_decode::messages::MessageContents::VolumeCoveragePattern(vcp) =
                    message.contents()
                {
                    return Ok(vcp.clone().into_owned());
                }
            }
        }
    }
    Err(Error::MissingCoveragePattern)
}
