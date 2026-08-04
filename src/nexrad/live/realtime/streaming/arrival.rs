//! Per-chunk arrival diagnostics: the derivations that turn "a chunk just
//! landed" into the fields of a [`crate::core::ChunkArrivalStat`].
//!
//! These feed the VCP forecast modal's predicted-vs-observed comparison. All
//! but [`log_ingest_lag`] are pure functions of the plan and the captured
//! forecast, so they are unit-testable without a browser or a live stream.

use crate::core::projection::SharedProjectionEngine;
use crate::core::{ChunkProjectedTimes, StreamingPlan};

/// Human-readable chunk-type label used by the arrival stat and the ingest-lag
/// log line. Pure.
pub(super) fn chunk_type_label(is_start: bool, is_end: bool) -> &'static str {
    if is_start {
        "Start"
    } else if is_end {
        "End"
    } else {
        "Intermediate"
    }
}

/// Log the empirical NEXRAD ingest lag: difference between the chunk's
/// S3 upload time (AVAILABILITY) and its latest radial collection time
/// (ACTUAL). No-op when either side is unknown.
pub(super) fn log_ingest_lag(
    engine: &SharedProjectionEngine,
    s3_last_modified_at: Option<f64>,
    chunks_in_volume: u32,
    type_label: &str,
) {
    if let (Some(upload_secs), Some(collection_end_secs)) = (
        s3_last_modified_at,
        engine.borrow().collection_anchor_secs(),
    ) {
        log::debug!(
            "ingest lag: upload={:.3}s collection_end={:.3}s Δ={:+.3}s (seq={} type={})",
            upload_secs,
            collection_end_secs,
            upload_secs - collection_end_secs,
            chunks_in_volume,
            type_label,
        );
    }
}

/// Structural metadata for the chunk that just arrived — `(elevation_number,
/// chunk_index_in_sweep, chunks_in_sweep)` — looked up by sequence in the fresh
/// plan's current-volume slice. Pure.
pub(super) fn chunk_structure_from_plan(
    plan: Option<&StreamingPlan>,
    sequence: u32,
) -> (Option<u8>, Option<u32>, Option<u32>) {
    plan.and_then(|p| {
        p.current_volume_chunks
            .iter()
            .find(|c| c.sequence as u32 == sequence)
    })
    .map(|c| {
        (
            c.elevation_number.map(|e| e as u8),
            Some(c.chunk_index_in_sweep as u32),
            Some(c.chunks_in_sweep as u32),
        )
    })
    .unwrap_or((None, None, None))
}

/// The prediction-side arrival-stat fields — `(bucket_key,
/// stats_n_at_prediction, scheduler_path, physics_breakdown)` — mapped out of
/// the forecast that produced this chunk's sleep target (captured on the
/// chunk's first iteration). All-empty when no forecast was available. Pure.
#[allow(clippy::type_complexity)]
pub(super) fn forecast_stat_fields(
    forecast: Option<&ChunkProjectedTimes>,
) -> (
    Option<crate::core::BucketKey>,
    usize,
    Option<crate::core::timing::SchedulerPath>,
    Option<crate::core::timing::PhysicsBreakdown>,
) {
    match forecast {
        Some(f) => (
            f.bucket
                .as_ref()
                .map(crate::core::BucketKey::from_characteristics),
            f.stats_n,
            Some(f.scheduler_path),
            Some(f.physics_breakdown),
        ),
        None => (None, 0, None, None),
    }
}
