//! Canonical forward-looking model for the real-time stream.
//!
//! [`StreamingPlan`] is the single source of truth for "what will happen next,
//! when." It is computed once per streaming-loop iteration by
//! [`super::streaming_state::StreamingState::build_plan`] and consumed by:
//!
//! - The streaming loop's sleep target ([`NextChunkTarget::projected_poll_at_secs`]).
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

use super::timing::{ChunkCharacteristics, PhysicsBreakdown, ScanTimingProjection, SchedulerPath};
use super::ChunkProjectionInfo;

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
    /// structural metadata for every chunk and projected times for chunks
    /// still in the future.
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
    /// The single immediate next-download target — the first chunk in the
    /// projection that the filter accepts. `None` only when there are no
    /// projected chunks (e.g. at end-of-volume with no next-volume extension).
    /// The loop's sleep, the UI's countdown, and the per-chunk arrival
    /// diagnostics all source from this.
    pub next_target: Option<NextChunkTarget>,
}

/// The immediate next chunk the streaming loop plans to download.
///
/// Distilled from the first filter-accepted [`super::timing::ChunkProjection`]
/// in the underlying [`ScanTimingProjection`]. Sleep duration is
/// `(projected_poll_at_secs - now).max(0.0)`; UI countdown is
/// `(projected_available_at_secs - now).max(0.0)`.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Forecast/diagnostic surface — fields are part of the contract.
pub struct NextChunkTarget {
    pub sequence: usize,
    /// `0` = current volume; `1` = next volume (the chained projection's
    /// extension fired because the filter has no current-volume match).
    pub volume_offset: u8,
    pub elevation_number: Option<usize>,
    /// COLLECTION category: when the radar physically finishes this chunk.
    pub projected_collection_time_secs: f64,
    /// AVAILABILITY category: when the chunk is expected to appear on S3.
    pub projected_available_at_secs: f64,
    /// POLL category: when the scheduler should fire its first download
    /// poll (`available_at + retry_budget + POLL_BIAS`). This is the
    /// streaming loop's sleep target.
    pub projected_poll_at_secs: f64,
    /// Typical retry-poll budget folded into `projected_poll_at_secs`.
    pub retry_budget_secs: f64,
    /// Physics decomposition for the hop into this chunk. Attached to
    /// the per-chunk [`crate::state::ChunkArrivalStat`] on success so the
    /// diagnostics modal can attribute prediction errors.
    pub physics_breakdown: PhysicsBreakdown,
    /// Which projector branch supplied the interval: `Blended` when
    /// historical samples contributed, `Physics` otherwise.
    pub scheduler_path: SchedulerPath,
    /// Bucket sample count at projection time.
    pub stats_n_at_prediction: usize,
    /// The bucket key the lookup hit (or missed).
    pub bucket: Option<ChunkCharacteristics>,
}

impl StreamingPlan {
    /// Build a plan from a fresh [`ScanTimingProjection`] plus the structural
    /// metadata of the current volume's chunks. The projection is consumed.
    ///
    /// `current_volume_chunk_meta` must be every chunk's structural
    /// metadata for the current volume (from
    /// [`super::timing::ElevationChunkMapper::all_chunk_metadata`]), in
    /// sequence order. Projected times are looked up in `projection.chunks()`
    /// keyed by `(volume_offset, sequence)` so pass-1 and pass-2 entries
    /// don't collide.
    pub(super) fn from_projection(
        projection: ScanTimingProjection,
        filter: Option<u8>,
        current_volume_chunk_meta: &[super::timing::ChunkMetadata],
    ) -> Self {
        use std::collections::HashMap;

        // Per-(volume_offset, sequence) lookups for projected times.
        let mut collection_by: HashMap<(u8, usize), f64> = HashMap::new();
        let mut available_by: HashMap<(u8, usize), f64> = HashMap::new();
        let mut has_next_volume = false;
        for c in projection.chunks() {
            let key = (c.volume_offset(), c.sequence());
            collection_by.insert(key, c.projected_collection_time_secs());
            available_by.insert(
                key,
                c.projected_available_at().timestamp_millis() as f64 / 1000.0,
            );
            if c.volume_offset() == 1 {
                has_next_volume = true;
            }
        }

        let make_info =
            |meta: &super::timing::ChunkMetadata, volume_offset: u8| ChunkProjectionInfo {
                sequence: meta.sequence(),
                elevation_number: meta.elevation_number(),
                elevation_angle_deg: meta.elevation_angle_deg(),
                azimuth_rate_dps: meta.azimuth_rate_dps(),
                projected_collection_time_secs: collection_by
                    .get(&(volume_offset, meta.sequence()))
                    .copied(),
                projected_available_at_secs: available_by
                    .get(&(volume_offset, meta.sequence()))
                    .copied(),
                starts_new_sweep: meta.is_first_in_sweep(),
                chunk_index_in_sweep: meta.chunk_index_in_sweep(),
                chunks_in_sweep: meta.chunks_in_sweep(),
                volume_offset,
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
            .find_map(|c| c.projected_collection_time_secs);
        let current_volume_end_available_at_secs = current_volume_chunks
            .iter()
            .rev()
            .find_map(|c| c.projected_available_at_secs);

        // Immediate next download: first projection chunk the filter accepts.
        // Start chunks (no elevation) are always accepted.
        let next_target = projection
            .chunks()
            .iter()
            .find(|c| match (filter, c.elevation_number()) {
                (None, _) => true,
                (Some(_), None) => true,
                (Some(f), Some(elev)) => elev as u8 == f,
            })
            .map(|c| {
                let path = if c.used_historical() {
                    SchedulerPath::Blended
                } else {
                    SchedulerPath::Physics
                };
                NextChunkTarget {
                    sequence: c.sequence(),
                    volume_offset: c.volume_offset(),
                    elevation_number: c.elevation_number(),
                    projected_collection_time_secs: c.projected_collection_time_secs(),
                    projected_available_at_secs: c.projected_available_at().timestamp_millis()
                        as f64
                        / 1000.0,
                    projected_poll_at_secs: c.projected_poll_at().timestamp_millis() as f64
                        / 1000.0,
                    retry_budget_secs: c.retry_budget_secs(),
                    physics_breakdown: c.physics_breakdown(),
                    scheduler_path: path,
                    stats_n_at_prediction: c.stats_n(),
                    bucket: c.bucket().cloned(),
                }
            });

        StreamingPlan {
            filter,
            current_volume_chunks,
            next_volume_chunks,
            current_volume_end_collection_secs,
            current_volume_end_available_at_secs,
            next_target,
        }
    }

    /// Convenience: seconds from `now_secs` until the next target becomes
    /// available in S3 (drives the UI's "next in Xs" countdown). Returns
    /// `None` when no next target exists.
    pub fn next_available_in_secs(&self, now_secs: f64) -> Option<f64> {
        self.next_target
            .as_ref()
            .map(|t| (t.projected_available_at_secs - now_secs).max(0.0))
    }

    /// Convenience: seconds from `now_secs` until the streaming loop's next
    /// poll fires (the sleep target).
    #[allow(dead_code)] // Public-surface accessor; UI debug overlay consumes it.
    pub fn next_poll_in_secs(&self, now_secs: f64) -> Option<f64> {
        self.next_target
            .as_ref()
            .map(|t| (t.projected_poll_at_secs - now_secs).max(0.0))
    }
}
