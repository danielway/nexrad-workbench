//! Replacement for `nexrad_data::aws::realtime::ChunkIterator`.
//!
//! Mirrors the subset of `ChunkIterator` that `realtime.rs` actually uses:
//! init (fetch latest + optional start chunk, extract VCP), pull-based
//! `try_next`, and timing/metadata accessors. Volume discovery itself is
//! delegated to `nexrad_data::aws::realtime::get_latest_volume`.

use super::streaming_filter::StreamingFilter;
use super::streaming_plan::StreamingPlan;
use super::timing::{
    project_scan_timing_with_next, ChunkCharacteristics, ChunkMetadata, ChunkTimingStats,
    ElevationChunkMapper, ScanTimingProjection,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use log::debug;
use nexrad_data::aws::realtime::{
    download_chunk, list_chunks_in_volume, Chunk, ChunkIdentifier, ChunkType, DownloadedChunk,
    VolumeIndex,
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

/// Outcome of a filter-aware chunk fetch attempt.
///
/// `Downloaded` and `NotYetAvailable` mirror the success / 404 cases of
/// [`StreamingState::try_next`]. `SyntheticVolumeEnd` indicates that the
/// active filter excludes every remaining sequence in the current volume
/// (including the End chunk's elevation), so the loop should advance to the
/// next volume's Start without issuing a fetch — the End chunk itself never
/// surfaces in this case, but the volume-boundary signal does.
#[derive(Debug)]
pub enum TryNextOutcome {
    Downloaded(DownloadedChunk),
    NotYetAvailable,
    SyntheticVolumeEnd,
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
    /// ACTUAL category: collection-end time (Unix seconds) of the most
    /// recently ingested chunk, parsed by the worker as the latest radial
    /// timestamp in the chunk. Pushed in from `main.rs` after each ingest
    /// response and reset on volume boundary. Used as the anchor for
    /// projected COLLECTION times — the projector adds cumulative
    /// inter-chunk physics intervals to this to place future chunks on
    /// the timeline.
    latest_chunk_collection_end_secs: Option<f64>,
    /// Active filter mirrored from the realtime loop. Used by
    /// [`StreamingState::project_remaining_scan_internal`] to decide
    /// whether to extend the projection into the next volume — relevant
    /// when the filter's target elevation has no remaining matches in
    /// the current volume, so the next download will land in the next
    /// volume.
    filter: StreamingFilter,
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
            latest_chunk_collection_end_secs: None,
            filter: StreamingFilter::default(),
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

    /// Filter-aware variant of [`try_next`]. The predicate is invoked on each
    /// candidate sequence's elevation number (`None` = Start chunk); when no
    /// remaining sequence in the current volume matches, returns
    /// [`TryNextOutcome::SyntheticVolumeEnd`] without issuing any HTTP and
    /// advances the iterator's `current` to the volume's final sequence so
    /// the next call rolls over to the new volume's Start.
    ///
    /// `accept_end` keeps the End chunk as an unconditional accept so the
    /// real volume-boundary signal still lands when the user's filter
    /// happens to cover the last sweep.
    pub async fn try_next_matching(
        &mut self,
        accept_end: bool,
        mut predicate: impl FnMut(Option<usize>) -> bool,
    ) -> Result<TryNextOutcome> {
        let (target_seq, final_seq) = {
            let mapper = self
                .elevation_mapper
                .as_ref()
                .ok_or(AWSError::FailedToDetermineNextChunk)?;
            let final_seq = mapper.final_sequence();
            let current_seq = self.current.sequence();

            if current_seq >= final_seq {
                let downloaded = self
                    .try_fetch_volume_start(self.current.volume().next())
                    .await?;
                return Ok(match downloaded {
                    Some(c) => TryNextOutcome::Downloaded(c),
                    None => TryNextOutcome::NotYetAvailable,
                });
            }

            let target =
                mapper.next_matching_sequence_after(current_seq, accept_end, &mut predicate);
            (target, final_seq)
        };

        let Some(target) = target_seq else {
            // Filter excludes every remaining sequence in this volume
            // (including the End chunk). Synthesize the volume-boundary
            // by advancing `current` to the final sequence; the next call
            // will fall through to `try_fetch_volume_start`.
            self.advance_current_to_synthetic_end(final_seq);
            return Ok(TryNextOutcome::SyntheticVolumeEnd);
        };

        let next_type = if target == final_seq {
            ChunkType::End
        } else {
            ChunkType::Intermediate
        };
        let next_id = ChunkIdentifier::new(
            self.current.site().to_string(),
            *self.current.volume(),
            *self.current.date_time_prefix(),
            target,
            next_type,
            None,
        );
        let downloaded = self.try_fetch_chunk(next_id).await?;
        Ok(match downloaded {
            Some(c) => TryNextOutcome::Downloaded(c),
            None => TryNextOutcome::NotYetAvailable,
        })
    }

    /// Advance `current` to the volume's final sequence without issuing a
    /// fetch. Used by the filter-aware path when every remaining chunk is
    /// filtered out, so the next iteration rolls over to the next volume
    /// via the existing `try_fetch_volume_start` path.
    fn advance_current_to_synthetic_end(&mut self, final_sequence: usize) {
        self.current = ChunkIdentifier::new(
            self.current.site().to_string(),
            *self.current.volume(),
            *self.current.date_time_prefix(),
            final_sequence,
            ChunkType::End,
            None,
        );
        self.latest_chunk_collection_end_secs = None;
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
        let final_sequence = mapper.final_sequence();
        let current_sequence = self.current.sequence();

        if current_sequence == final_sequence {
            return self
                .try_fetch_volume_start(self.current.volume().next())
                .await;
        }

        let next_sequence = current_sequence + 1;
        let next_type = if next_sequence == final_sequence {
            ChunkType::End
        } else {
            ChunkType::Intermediate
        };
        let next_id = ChunkIdentifier::new(
            self.current.site().to_string(),
            *self.current.volume(),
            *self.current.date_time_prefix(),
            next_sequence,
            next_type,
            None,
        );
        self.try_fetch_chunk(next_id).await
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
                    self.latest_chunk_collection_end_secs = None;
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

        // Always emit the Start chunk first when transitioning to a new
        // volume — the worker accumulator is cleared on the previous
        // volume's End and only re-initializes when an `is_start: true`
        // chunk arrives. If we instead emitted the latest chunk in the
        // new volume (potentially Intermediate, when polling lands after
        // the inter-volume gap and several chunks are already published),
        // every subsequent ingest would error with
        // "No accumulator — missing Start chunk?".
        //
        // Subsequent `try_next` iterations fetch chunks 2, 3, … in
        // sequence order via the normal `try_fetch_chunk` path.
        let target = match chunks.iter().find(|c| c.chunk_type() == ChunkType::Start) {
            Some(id) => id,
            None => return Ok(None), // Start chunk hasn't been published yet
        };
        let (identifier, chunk) = download_chunk(&self.site, target).await?;
        self.requests_made += 1;
        self.bytes_downloaded += chunk.data().len() as u64;

        // Identifier must be Start by construction above; extract VCP and
        // reset the per-volume collection-end anchor.
        if let Ok(v) = extract_vcp(&chunk) {
            self.elevation_mapper = Some(ElevationChunkMapper::new(&v));
            self.vcp = Some(v);
        }
        self.latest_chunk_collection_end_secs = None;

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
            chunk_type: chunk_id.chunk_type(),
            waveform_type: elevation.waveform_type(),
            channel_configuration: elevation.channel_configuration(),
            is_first_in_sweep,
        })
    }

    /// Attach an observed availability-lag (S3 upload − ACTUAL collection
    /// time) sample to the most recent timing stat recorded for the current
    /// chunk. Called from the streaming loop after a worker ingest produces
    /// the parsed chunk collection time.
    pub fn record_availability_lag_for_current(&mut self, lag_secs: f64) {
        let Some(characteristics) = self.characteristics_for_sequence(&self.current.clone()) else {
            return;
        };
        self.timing_stats.attach_availability_lag(
            &characteristics,
            ChronoDuration::milliseconds((lag_secs * 1000.0) as i64),
        );
    }

    /// Expose the rolling timing statistics for persistence by the streaming loop.
    pub fn timing_stats(&self) -> &ChunkTimingStats {
        &self.timing_stats
    }

    /// Record the latest radial collection time (Unix seconds) of the
    /// most recently ingested chunk. Pushed in from the streaming loop
    /// after each ingest response and used as the anchor for projected
    /// COLLECTION times — the projector adds cumulative inter-chunk
    /// physics intervals to this value.
    pub fn record_chunk_collection_end_secs(&mut self, secs: f64) {
        self.latest_chunk_collection_end_secs = Some(secs);
    }

    /// Most recently recorded chunk collection-end time (Unix seconds),
    /// if any. None until the first M chunk of a volume has been ingested.
    pub fn latest_chunk_collection_end_secs(&self) -> Option<f64> {
        self.latest_chunk_collection_end_secs
    }

    /// Replace the rolling timing statistics with a previously-persisted snapshot.
    /// Called once on stream start when a localStorage cache is available for the site.
    pub fn preload_timing_stats(&mut self, stats: ChunkTimingStats) {
        self.timing_stats = stats;
    }

    /// Sequences in `[lower, upper]` matching `predicate`. Wraps the mapper
    /// helper so the streaming loop can compute filter-aware backfill targets
    /// without exposing the mapper itself.
    pub fn mapper_matching_sequences_in_range(
        &self,
        lower: usize,
        upper: usize,
        predicate: impl FnMut(Option<usize>) -> bool,
    ) -> Vec<usize> {
        self.elevation_mapper
            .as_ref()
            .map(|m| m.matching_sequences_in_range(lower, upper, predicate))
            .unwrap_or_default()
    }

    /// 1-based sequence number of the chunk currently anchoring the iterator.
    pub fn current_sequence(&self) -> usize {
        self.current.sequence()
    }

    /// Volume index the iterator is currently anchored in.
    pub fn current_volume(&self) -> VolumeIndex {
        *self.current.volume()
    }

    /// Anchor source the projector would use right now — `ObservedCollection`
    /// when an ACTUAL collection-end time is available for the current chunk,
    /// `UploadMinusMedian` when only `ChunkTimingStats` median lag is, or
    /// `UploadMinusDefault` otherwise. Captured per-chunk so we can spot
    /// degraded projections in the diagnostics modal.
    pub fn current_anchor_source(&self) -> super::timing::AnchorSource {
        use super::timing::AnchorSource;
        if self.latest_chunk_collection_end_secs.is_some() {
            AnchorSource::ObservedCollection
        } else if self.timing_stats.median_availability_lag_secs().is_some() {
            AnchorSource::UploadMinusMedian
        } else {
            AnchorSource::UploadMinusDefault
        }
    }

    pub fn chunk_metadata(&self, sequence: usize) -> Option<&ChunkMetadata> {
        self.elevation_mapper
            .as_ref()
            .and_then(|m| m.get_chunk_metadata(sequence))
    }

    /// Build a [`StreamingPlan`] — the canonical forward-looking model used
    /// by the streaming loop's sleep target, the timeline UI, and per-chunk
    /// arrival diagnostics. Computed once per loop iteration; every
    /// downstream consumer reads from the returned object so they can't
    /// disagree.
    ///
    /// Returns `None` only when the iterator is in a cold state (no VCP /
    /// no mapper yet) — every other path produces a plan, possibly with
    /// `next_target = None` if there's nothing left to project.
    pub fn build_plan(&self) -> Option<StreamingPlan> {
        let mapper = self.elevation_mapper.as_ref()?;
        let projection = self.project_remaining_scan_internal()?;
        let chunk_meta = mapper.all_chunk_metadata();
        Some(StreamingPlan::from_projection(
            projection,
            self.filter,
            chunk_meta,
        ))
    }

    /// Internal projection builder. The only call site for
    /// [`project_scan_timing_with_next`] in this crate — all forward-looking
    /// timing flows through here. Extends into the next volume when the
    /// active filter has no remaining match in the current volume (so the
    /// scheduler and UI both see the cross-volume hop in one object).
    fn project_remaining_scan_internal(&self) -> Option<ScanTimingProjection> {
        let vcp = self.vcp.as_ref()?;
        let mapper = self.elevation_mapper.as_ref()?;

        // Extend into the next volume iff the active filter has no remaining
        // match in this volume. `has_remaining_match` returns `true` for
        // `StreamingFilter::All` whenever chunks remain, so the All branch
        // naturally never triggers an extension.
        let include_next_volume = !mapper.has_remaining_match(self.filter, self.current.sequence());

        project_scan_timing_with_next(
            &self.current,
            self.latest_chunk_collection_end_secs,
            vcp,
            mapper,
            Some(&self.timing_stats),
            include_next_volume,
        )
    }

    /// Push the streaming-loop's currently-active filter into the state so
    /// projections can be filter-aware.
    pub fn set_filter(&mut self, filter: StreamingFilter) {
        self.filter = filter;
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
