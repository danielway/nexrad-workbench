//! Every interaction the streaming loop has with the shared
//! [`SharedProjectionEngine`]: draining queued observations into it, reading a
//! [`StreamingPlan`] back out, feeding it a freshly-arrived chunk, and handing
//! it the volume boundary.
//!
//! Keeping these in one place preserves the engine invariant that a
//! `borrow_mut()` never spans an `.await` — none of the functions below are
//! `async`.

use crate::core::projection::{ChunkCoord, KnownChunk, SharedProjectionEngine};
use crate::core::StreamingPlan;
use crate::nexrad::live::realtime::ProjectorObservation;
use crate::nexrad::live::streaming_state::StreamingState;
use futures_channel::mpsc::UnboundedReceiver;
use nexrad_data::aws::realtime::{ChunkIdentifier, ChunkType, DownloadedChunk};

/// Drain projector observations queued from `main.rs` (after worker
/// ingest) and apply each to the shared projection engine. The
/// dispatch shape — match on [`ProjectorObservation`] variant,
/// call the matching engine method — is intentionally explicit so
/// adding a new observation kind is just one new arm here.
pub(super) fn drain_pending_observations(
    observations_rx: &mut UnboundedReceiver<ProjectorObservation>,
    engine: &SharedProjectionEngine,
    iter: &StreamingState,
) {
    while let Ok(obs) = observations_rx.try_recv() {
        match obs {
            ProjectorObservation::CollectionEndSecs(secs) => {
                engine
                    .borrow_mut()
                    .set_collection_anchor(iter.current_id(), secs);
            }
            ProjectorObservation::AvailabilityLagSecs(lag_secs) => {
                engine
                    .borrow_mut()
                    .record_availability_lag_for(iter.current_id(), lag_secs);
            }
        }
    }
}

/// Build a [`StreamingPlan`] from the shared engine, anchored at the loop's
/// current download cursor. The `engine.borrow_mut()` is scoped to this call —
/// per the engine invariant it never spans an `.await`.
pub(super) fn build_engine_plan(
    engine: &SharedProjectionEngine,
    anchor: &ChunkIdentifier,
    now_secs: f64,
) -> Option<StreamingPlan> {
    engine
        .borrow_mut()
        .projection(anchor, now_secs)
        .map(|p| p.plan.clone())
}

/// Feed the shared engine a chunk that just arrived: it is now
/// known-available, and its arrival interval feeds the timing-stats blend.
/// Borrows are scoped and never span an `.await`.
///
/// `prev_upload_dt` carries the previous chunk's S3 upload time across
/// iterations; it is advanced here on every chunk that has upload metadata.
pub(super) fn observe_chunk_arrival(
    engine: &SharedProjectionEngine,
    identifier: &ChunkIdentifier,
    chunk_type: ChunkType,
    prev_upload_dt: &mut Option<chrono::DateTime<chrono::Utc>>,
) {
    if let Some(upload) = identifier.upload_date_time() {
        let upload_secs = upload.timestamp_millis() as f64 / 1000.0;
        let mut eng = engine.borrow_mut();
        eng.observe_known_chunk(KnownChunk {
            coord: ChunkCoord {
                volume: *identifier.volume(),
                sequence: identifier.sequence(),
            },
            upload_secs,
            chunk_type,
        });
        if let Some(prev) = *prev_upload_dt {
            eng.record_inter_chunk_duration(identifier, upload - prev, 1);
        }
        drop(eng);
        *prev_upload_dt = Some(upload);
    }
}

/// Volume boundary: rebuild the navigation mapper (`StreamingState`) AND hand
/// the shared engine its stream-side boundary in one call (new VCP, anchor
/// reset, inventory bound, scan start).
pub(super) fn install_volume_boundary(
    iter: &mut StreamingState,
    engine: &SharedProjectionEngine,
    chunk: &DownloadedChunk,
    scan_start_secs: f64,
) {
    if let Some(vcp) = iter.install_vcp_from_start(&chunk.chunk) {
        engine
            .borrow_mut()
            .begin_volume(vcp, scan_start_secs, *chunk.identifier.volume());
    } else {
        engine
            .borrow_mut()
            .set_current_scan_start_secs(scan_start_secs);
    }
}
