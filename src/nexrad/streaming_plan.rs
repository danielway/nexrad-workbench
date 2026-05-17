//! Canonical forward-looking model for the real-time stream.
//!
//! [`StreamingPlan`] is the single source of truth for "what will happen next,
//! when." It is computed once per streaming-loop iteration by
//! [`super::streaming_state::StreamingState::build_plan`] and consumed by:
//!
//! - The streaming loop's sleep target (via [`StreamingPlan::next_target`]'s
//!   [`ChunkForecast::poll_at_secs`]).
//! - The timeline's countdown and future-chunk markers.
//! - The VCP forecast panel.
//! - Per-chunk arrival diagnostics ([`crate::state::ChunkArrivalStat`]).
//!
//! Before this module existed there were two parallel computation paths —
//! `StreamingState::time_until_next` / `next_matching_chunk_diagnostics` /
//! `time_until_next_filtered_chunk_across_volumes` for the scheduler, and
//! `project_remaining_scan` for the UI — which drifted apart and produced
//! mismatched sleep targets vs. on-screen countdowns. The plan unifies them:
//! every consumer reads from the same object, so they can't disagree.
//!
//! All times are Unix seconds (sub-second precision) unless documented
//! otherwise.

use super::timing::{ScanTimingProjection, SchedulerPath};
use super::{ChunkForecast, ChunkProjectionInfo};

/// Canonical projection of the real-time stream's near future.
///
/// Built once per streaming-loop iteration from a snapshot of
/// [`super::streaming_state::StreamingState`]. Everything downstream that
/// needs to know "what comes next" — the loop's sleep target, the timeline's
/// countdown, the VCP forecast panel — reads from this object.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Forecast/diagnostic surface — fields are part of the contract.
pub struct StreamingPlan {
    /// Active elevation filter at plan-build time. `None` = no filter;
    /// `Some(n)` = only chunks for elevation `n` will be downloaded.
    pub filter: Option<u8>,
    /// Per-chunk info for the current in-progress volume. Carries
    /// structural metadata for every chunk and (via `forecast`) projected
    /// times + diagnostics for chunks still in the future.
    pub current_volume_chunks: Vec<ChunkProjectionInfo>,
    /// Per-chunk info for the *next* volume, present only when the filter
    /// has no remaining matches in the current volume — so the next
    /// download will land in the next volume. Reuses the current VCP's
    /// structure (the projector assumes the VCP doesn't change between
    /// volumes; if it does, the plan is rebuilt from the real next-volume
    /// Start chunk on its arrival).
    pub next_volume_chunks: Option<Vec<ChunkProjectionInfo>>,
    /// COLLECTION time the radar finishes the current volume's final
    /// chunk. Always refers to the *current* volume regardless of whether
    /// the plan extends into the next volume — drives the timeline's
    /// projected end-of-volume marker.
    pub current_volume_end_collection_secs: Option<f64>,
    /// AVAILABILITY time the current volume's final chunk lands on S3.
    pub current_volume_end_available_at_secs: Option<f64>,
    /// `(volume_offset, sequence)` key of the immediate next download —
    /// the first projection chunk the filter accepts. Resolved into the
    /// matching [`ChunkProjectionInfo`] via [`StreamingPlan::next_target`].
    /// `None` only when there are no projected chunks (e.g. at end-of-volume
    /// with no next-volume extension).
    next_target_key: Option<(u8, usize)>,
}

impl StreamingPlan {
    /// Build a plan from a fresh [`ScanTimingProjection`] plus the structural
    /// metadata of the current volume's chunks. The projection is consumed.
    ///
    /// `current_volume_chunk_meta` must be every chunk's structural
    /// metadata for the current volume (from
    /// [`super::timing::ElevationChunkMapper::all_chunk_metadata`]), in
    /// sequence order. Each chunk's `forecast` is populated by looking up
    /// `(volume_offset, sequence)` in `projection.chunks()` so pass-1 and
    /// pass-2 entries don't collide.
    pub(super) fn from_projection(
        projection: ScanTimingProjection,
        filter: Option<u8>,
        current_volume_chunk_meta: &[super::timing::ChunkMetadata],
    ) -> Self {
        use std::collections::HashMap;

        // Per-(volume_offset, sequence) forecast lookup. Built in one pass
        // over the projection chunks so structural metadata can later look
        // up its forecast in O(1).
        let mut forecasts: HashMap<(u8, usize), ChunkForecast> = HashMap::new();
        let mut has_next_volume = false;
        for c in projection.chunks() {
            let key = (c.volume_offset(), c.sequence());
            let scheduler_path = if c.used_historical() {
                SchedulerPath::Blended
            } else {
                SchedulerPath::Physics
            };
            forecasts.insert(
                key,
                ChunkForecast {
                    collection_time_secs: c.projected_collection_time_secs(),
                    available_at_secs: c.projected_available_at().timestamp_millis() as f64
                        / 1000.0,
                    poll_at_secs: c.projected_poll_at().timestamp_millis() as f64 / 1000.0,
                    retry_budget_secs: c.retry_budget_secs(),
                    physics_breakdown: c.physics_breakdown(),
                    stats_n: c.stats_n(),
                    used_historical: c.used_historical(),
                    scheduler_path,
                    bucket: c.bucket().cloned(),
                },
            );
            if c.volume_offset() == 1 {
                has_next_volume = true;
            }
        }

        let mut make_info =
            |meta: &super::timing::ChunkMetadata, volume_offset: u8| ChunkProjectionInfo {
                sequence: meta.sequence(),
                elevation_number: meta.elevation_number(),
                elevation_angle_deg: meta.elevation_angle_deg(),
                azimuth_rate_dps: meta.azimuth_rate_dps(),
                starts_new_sweep: meta.is_first_in_sweep(),
                chunk_index_in_sweep: meta.chunk_index_in_sweep(),
                chunks_in_sweep: meta.chunks_in_sweep(),
                volume_offset,
                forecast: forecasts.remove(&(volume_offset, meta.sequence())),
            };

        let current_volume_chunks: Vec<ChunkProjectionInfo> = current_volume_chunk_meta
            .iter()
            .map(|m| make_info(m, 0))
            .collect();
        let next_volume_chunks = has_next_volume.then(|| {
            current_volume_chunk_meta
                .iter()
                .map(|m| make_info(m, 1))
                .collect()
        });

        // Always derive current-volume end from the current_volume_chunks
        // pass — never from `projection.chunks().last()`, which points at
        // the next volume's tail when chained projection is active.
        let current_volume_end_collection_secs = current_volume_chunks
            .iter()
            .rev()
            .find_map(|c| c.forecast.as_ref().map(|f| f.collection_time_secs));
        let current_volume_end_available_at_secs = current_volume_chunks
            .iter()
            .rev()
            .find_map(|c| c.forecast.as_ref().map(|f| f.available_at_secs));

        // Immediate next download: first projection chunk the filter accepts.
        // Start chunks (no elevation) are always accepted. Stored as a
        // (volume_offset, sequence) key so `next_target()` resolves to the
        // matching `ChunkProjectionInfo` — keeping the next target and its
        // entry in `current_volume_chunks` / `next_volume_chunks` from
        // drifting.
        let next_target_key = projection
            .chunks()
            .iter()
            .find(|c| match (filter, c.elevation_number()) {
                (None, _) => true,
                (Some(_), None) => true,
                (Some(f), Some(elev)) => elev as u8 == f,
            })
            .map(|c| (c.volume_offset(), c.sequence()));

        StreamingPlan {
            filter,
            current_volume_chunks,
            next_volume_chunks,
            current_volume_end_collection_secs,
            current_volume_end_available_at_secs,
            next_target_key,
        }
    }

    /// The immediate next chunk the streaming loop plans to download.
    ///
    /// Resolves [`Self::next_target_key`] into the matching
    /// [`ChunkProjectionInfo`] in `current_volume_chunks` (when
    /// `volume_offset = 0`) or `next_volume_chunks` (when `volume_offset = 1`).
    /// Returns `None` when no projected chunk matches the active filter —
    /// e.g. at end-of-volume with no next-volume extension.
    ///
    /// The returned chunk's `forecast` is always `Some` (next-target by
    /// definition points to a chunk the projector emitted timing for).
    pub fn next_target(&self) -> Option<&ChunkProjectionInfo> {
        let (vol_offset, seq) = self.next_target_key?;
        let chunks: &[ChunkProjectionInfo] = match vol_offset {
            0 => &self.current_volume_chunks,
            1 => self.next_volume_chunks.as_deref()?,
            _ => return None,
        };
        chunks.iter().find(|c| c.sequence == seq)
    }

    /// Convenience: seconds from `now_secs` until the next target becomes
    /// available in S3 (drives the UI's "next in Xs" countdown). Returns
    /// `None` when no next target exists.
    pub fn next_available_in_secs(&self, now_secs: f64) -> Option<f64> {
        self.next_target()
            .and_then(|t| t.forecast.as_ref())
            .map(|f| (f.available_at_secs - now_secs).max(0.0))
    }

    /// Convenience: seconds from `now_secs` until the streaming loop's next
    /// poll fires (the sleep target).
    #[allow(dead_code)] // Public-surface accessor; UI debug overlay consumes it.
    pub fn next_poll_in_secs(&self, now_secs: f64) -> Option<f64> {
        self.next_target()
            .and_then(|t| t.forecast.as_ref())
            .map(|f| (f.poll_at_secs - now_secs).max(0.0))
    }
}
