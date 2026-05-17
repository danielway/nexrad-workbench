//! Replacement for `nexrad_data::aws::realtime::ChunkIterator`.
//!
//! Mirrors the subset of `ChunkIterator` that `realtime.rs` actually uses:
//! init (fetch latest + optional start chunk, extract VCP), pull-based
//! `try_next`, and timing/metadata accessors. Volume discovery itself is
//! delegated to `nexrad_data::aws::realtime::get_latest_volume`.
//!
//! Projection state (VCP, mapper, timing stats, collection-end anchor,
//! filter) lives on the embedded [`Projector`]; this type's other fields
//! own only download bookkeeping. Projection-touching methods are kept
//! on `StreamingState` as thin delegations so call sites stay unchanged
//! while the producer moves toward main-thread ownership.

use super::projector::Projector;
use super::streaming_filter::StreamingFilter;
use super::streaming_plan::StreamingPlan;
use super::timing::{ChunkMetadata, ChunkTimingStats};
use chrono::{DateTime, Utc};
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
    last_chunk_time: Option<DateTime<Utc>>,
    requests_made: usize,
    bytes_downloaded: u64,
    /// Projection state — VCP, mapper, timing stats, collection-end
    /// anchor, filter. Owned here for now; a later commit moves this to
    /// the main thread so worker-pipeline observations can feed it
    /// directly.
    projector: Projector,
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

        let mut projector = Projector::new();
        let mut start_chunk_download: Option<DownloadedChunk> = None;

        if latest_id.chunk_type() == ChunkType::Start {
            // Latest IS the Start chunk — extract VCP from it.
            if let Ok(v) = extract_vcp(&latest_chunk) {
                projector.set_vcp(v);
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
                    projector.set_vcp(v);
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
            last_chunk_time,
            requests_made,
            bytes_downloaded,
            projector,
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
                .projector
                .mapper()
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
        self.projector.reset_collection_anchor();
    }

    /// Attempts to fetch the next chunk.
    /// - `Ok(Some(chunk))` — downloaded
    /// - `Ok(None)` — not yet available, caller should wait and retry
    /// - `Err(...)` — unrecoverable error
    pub async fn try_next(&mut self) -> Result<Option<DownloadedChunk>> {
        let mapper = self
            .projector
            .mapper()
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
                        self.projector.set_vcp(v);
                    }
                    self.projector.reset_collection_anchor();
                }

                if let (Some(upload), Some(prev)) =
                    (identifier.upload_date_time(), self.last_chunk_time)
                {
                    self.projector
                        .record_inter_chunk_duration(&identifier, upload - prev, 1);
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
            self.projector.set_vcp(v);
        }
        self.projector.reset_collection_anchor();

        if let (Some(upload), Some(prev)) = (identifier.upload_date_time(), self.last_chunk_time) {
            self.projector
                .record_inter_chunk_duration(&identifier, upload - prev, 1);
        }

        self.last_chunk_time = identifier.upload_date_time();
        self.current = identifier.clone();

        Ok(Some(DownloadedChunk {
            identifier,
            chunk,
            attempts: 1,
        }))
    }

    // ── Projection delegations ─────────────────────────────────────────
    //
    // Thin wrappers over the embedded [`Projector`]. Kept on
    // `StreamingState` so external call sites (the streaming loop) don't
    // change while ownership remains here. A later commit reroutes the
    // loop to read from a main-thread projector and these delegations
    // collapse.

    /// Attach an observed availability-lag (S3 upload − ACTUAL collection
    /// time) sample to the most recent timing stat recorded for the
    /// current chunk.
    pub fn record_availability_lag_for_current(&mut self, lag_secs: f64) {
        let current = self.current.clone();
        self.projector
            .record_availability_lag_for(&current, lag_secs);
    }

    /// Expose the rolling timing statistics for persistence by the streaming loop.
    pub fn timing_stats(&self) -> &ChunkTimingStats {
        self.projector.timing_stats()
    }

    pub fn record_chunk_collection_end_secs(&mut self, secs: f64) {
        self.projector.record_chunk_collection_end_secs(secs);
    }

    pub fn latest_chunk_collection_end_secs(&self) -> Option<f64> {
        self.projector.latest_chunk_collection_end_secs()
    }

    pub fn preload_timing_stats(&mut self, stats: ChunkTimingStats) {
        self.projector.preload_timing_stats(stats);
    }

    pub fn mapper_matching_sequences_in_range(
        &self,
        lower: usize,
        upper: usize,
        predicate: impl FnMut(Option<usize>) -> bool,
    ) -> Vec<usize> {
        self.projector
            .mapper_matching_sequences_in_range(lower, upper, predicate)
    }

    pub fn current_anchor_source(&self) -> super::timing::AnchorSource {
        self.projector.current_anchor_source()
    }

    pub fn chunk_metadata(&self, sequence: usize) -> Option<&ChunkMetadata> {
        self.projector.chunk_metadata(sequence)
    }

    /// Build a [`StreamingPlan`] anchored at the current chunk. See
    /// [`Projector::build_plan`] for the contract.
    pub fn build_plan(&self, now_secs: f64) -> Option<StreamingPlan> {
        self.projector.build_plan(&self.current, now_secs)
    }

    pub fn set_filter(&mut self, filter: StreamingFilter) {
        self.projector.set_filter(filter);
    }

    // ── Download bookkeeping ───────────────────────────────────────────

    /// 1-based sequence number of the chunk currently anchoring the iterator.
    pub fn current_sequence(&self) -> usize {
        self.current.sequence()
    }

    /// Volume index the iterator is currently anchored in.
    pub fn current_volume(&self) -> VolumeIndex {
        *self.current.volume()
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
