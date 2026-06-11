//! Canonical forward-looking model for the real-time stream.
//!
//! [`StreamingPlan`] is the single source of truth for "what will happen next,
//! when." It is computed once per streaming-loop iteration by
//! [`super::streaming_state::StreamingState::build_plan`] and consumed by:
//!
//! - The streaming loop's sleep target (via [`StreamingPlan::next_target`]'s
//!   [`ChunkProjectedTimes::poll_at_secs`]).
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

use super::streaming_filter::StreamingFilter;
use super::timing::{ScanTimingProjection, SchedulerPath};
use super::{ChunkProjectedTimes, ChunkProjectionInfo};

/// Canonical projection of the real-time stream's near future.
///
/// Built once per streaming-loop iteration from a snapshot of
/// [`super::streaming_state::StreamingState`]. Everything downstream that
/// needs to know "what comes next" — the loop's sleep target, the timeline's
/// countdown, the VCP forecast panel — reads from this object.
#[derive(Clone, Debug)]
pub struct StreamingPlan {
    /// Active filter at plan-build time. Diagnostic only — consumers
    /// already know the active filter via the streaming channel; this
    /// is preserved so a captured plan (e.g. for a per-chunk arrival
    /// stat) carries enough context to be interpreted standalone.
    #[allow(dead_code)] // Diagnostic context for captured plans.
    pub filter: StreamingFilter,
    /// Wall-clock time (Unix seconds) the plan was built. Lets consumers
    /// reason about plan staleness without threading `now` through every
    /// derivation.
    #[allow(dead_code)] // Read alongside `revision` for diagnostic display.
    pub built_at_secs: f64,
    /// Monotonically-incrementing per-projector counter, bumped on every
    /// [`super::projector::Projector::build_plan`] call. Lets diagnostics
    /// attribute a prediction to a specific plan revision and lets UI
    /// skip redraws when the plan hasn't changed since the last frame.
    pub revision: u64,
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
        filter: StreamingFilter,
        current_volume_chunk_meta: &[super::timing::ChunkMetadata],
        now_secs: f64,
        revision: u64,
    ) -> Self {
        use std::collections::HashMap;

        // Per-(volume_offset, sequence) forecast lookup. Built in one pass
        // over the projection chunks so structural metadata can later look
        // up its forecast in O(1).
        let mut forecasts: HashMap<(u8, usize), ChunkProjectedTimes> = HashMap::new();
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
                ChunkProjectedTimes {
                    collection_time_secs: c.projected_collection_time_secs(),
                    available_at_secs: c.projected_available_at().timestamp_millis() as f64
                        / 1000.0,
                    poll_at_secs: c.projected_poll_at().timestamp_millis() as f64 / 1000.0,
                    physics_breakdown: c.physics_breakdown(),
                    stats_n: c.stats_n(),
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
                azimuth_rate_dps: meta.azimuth_rate_dps(),
                chunk_index_in_sweep: meta.chunk_index_in_sweep(),
                chunks_in_sweep: meta.chunks_in_sweep(),
                projected: forecasts.remove(&(volume_offset, meta.sequence())),
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
            .find_map(|c| c.projected.as_ref().map(|f| f.collection_time_secs));

        // Immediate next download: first projection chunk the filter accepts.
        // Start chunks (no elevation) are always accepted. Stored as a
        // (volume_offset, sequence) key so `next_target()` resolves to the
        // matching `ChunkProjectionInfo` — keeping the next target and its
        // entry in `current_volume_chunks` / `next_volume_chunks` from
        // drifting.
        let next_target_key = projection
            .chunks()
            .iter()
            .find(|c| filter.accepts(c.elevation_number()))
            .map(|c| (c.volume_offset(), c.sequence()));

        StreamingPlan {
            filter,
            built_at_secs: now_secs,
            revision,
            current_volume_chunks,
            next_volume_chunks,
            current_volume_end_collection_secs,
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

    /// Whether the immediate next download target falls in the *next*
    /// volume rather than the current one. True when the active filter has
    /// no remaining match in the current volume (so the projection extended
    /// into the next volume). Lets the timeline attach the "next chunk"
    /// countdown to the next-volume ghost instead of a current-volume sweep.
    pub fn next_target_in_next_volume(&self) -> bool {
        matches!(self.next_target_key, Some((1, _)))
    }

    /// Elevation number (1-based) of the immediate next download target, or
    /// `None` for a Start chunk / when no target exists. Used to highlight
    /// the matching sweep in the next-volume ghost.
    pub fn next_target_elevation(&self) -> Option<u8> {
        self.next_target()
            .and_then(|c| c.elevation_number)
            .map(|n| n as u8)
    }

    /// Convenience: seconds from `now_secs` until the next target becomes
    /// available in S3 (drives the UI's "next in Xs" countdown). Returns
    /// `None` when no next target exists.
    pub fn next_available_in_secs(&self, now_secs: f64) -> Option<f64> {
        self.next_target()
            .and_then(|t| t.projected.as_ref())
            .map(|f| (f.available_at_secs - now_secs).max(0.0))
    }

    /// Convenience: seconds from `now_secs` until the streaming loop's next
    /// poll fires (the sleep target).
    #[allow(dead_code)] // Public-surface accessor; UI debug overlay consumes it.
    pub fn next_poll_in_secs(&self, now_secs: f64) -> Option<f64> {
        self.next_target()
            .and_then(|t| t.projected.as_ref())
            .map(|f| (f.poll_at_secs - now_secs).max(0.0))
    }

    /// Empty plan with the given filter/revision — test constructor for the
    /// `projection` module's wrapper tests.
    #[cfg(test)]
    pub(crate) fn empty_for_test(filter: StreamingFilter, revision: u64) -> Self {
        StreamingPlan {
            filter,
            built_at_secs: 0.0,
            revision,
            current_volume_chunks: Vec::new(),
            next_volume_chunks: None,
            current_volume_end_collection_secs: None,
            next_target_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn chunk(sequence: usize, elevation_number: Option<usize>) -> ChunkProjectionInfo {
        ChunkProjectionInfo {
            sequence,
            elevation_number,
            azimuth_rate_dps: 0.0,
            chunk_index_in_sweep: 0,
            chunks_in_sweep: 3,
            projected: None,
        }
    }

    fn plan(
        next_target_key: Option<(u8, usize)>,
        current: Vec<ChunkProjectionInfo>,
        next: Option<Vec<ChunkProjectionInfo>>,
    ) -> StreamingPlan {
        StreamingPlan {
            filter: StreamingFilter::Elevation(1),
            built_at_secs: 0.0,
            revision: 0,
            current_volume_chunks: current,
            next_volume_chunks: next,
            current_volume_end_collection_secs: None,
            next_target_key,
        }
    }

    #[wasm_bindgen_test]
    fn next_target_in_next_volume_true_for_volume_offset_1() {
        let p = plan(Some((1, 3)), vec![], Some(vec![chunk(3, Some(1))]));
        assert!(p.next_target_in_next_volume());
        assert_eq!(p.next_target_elevation(), Some(1));
    }

    #[wasm_bindgen_test]
    fn next_target_in_next_volume_false_for_current_volume() {
        let p = plan(Some((0, 5)), vec![chunk(5, Some(2))], None);
        assert!(!p.next_target_in_next_volume());
        assert_eq!(p.next_target_elevation(), Some(2));
    }

    #[wasm_bindgen_test]
    fn next_target_in_next_volume_false_when_no_target() {
        let p = plan(None, vec![], None);
        assert!(!p.next_target_in_next_volume());
        assert_eq!(p.next_target_elevation(), None);
    }
}
