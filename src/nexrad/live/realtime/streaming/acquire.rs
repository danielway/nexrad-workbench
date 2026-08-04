//! Stream acquisition: resolve the site's latest volume slot and initialize a
//! [`StreamingState`] at it, under a wall-clock timeout.
//!
//! The fast path forward-probes from the localStorage volume hint
//! ([`super::persist::get_cached_volume_hint`]); any miss falls back to the
//! library's rotated-array binary search, so there is no correctness regression.

use super::current_timestamp_f64;
use super::persist::get_cached_volume_hint;
use crate::net::retry::sleep_ms;
use crate::nexrad::live::realtime::RealtimeResult;
use crate::nexrad::live::streaming_state::{StreamingInit, StreamingState};
use eframe::egui;
use futures_channel::mpsc::UnboundedSender;
use std::cell::Cell;

/// How recent the cached volume hint must be (in seconds) for the fast-path
/// resume to trust it. A volume is one VCP (~4–10 min); within ~20 min the true
/// latest is only a few slots ahead, so the forward-probe walk costs far fewer
/// list calls than the library's binary search. Past this the hint is ignored
/// and discovery falls back to `get_latest_volume`.
const VOLUME_HINT_MAX_AGE_SECS: f64 = 20.0 * 60.0;

/// Decide whether the forward-probe should advance from the current candidate to
/// the next slot, given each slot's newest-chunk S3 upload time (seconds). We
/// advance only while the next slot is strictly newer; a not-yet-written or
/// recycled-older slot stops the walk. Pure — split out for testing.
pub(super) fn probe_should_advance(
    candidate_upload_secs: f64,
    next_upload_secs: Option<f64>,
) -> bool {
    matches!(next_upload_secs, Some(next) if next > candidate_upload_secs)
}

/// Newest-chunk S3 upload time (seconds) for a volume slot, or `None` if the
/// slot has no chunks / no upload metadata.
async fn slot_newest_upload_secs(
    site_id: &str,
    volume: nexrad_data::aws::realtime::VolumeIndex,
) -> Option<f64> {
    let chunks = nexrad_data::aws::realtime::list_chunks_in_volume(site_id, volume, 100)
        .await
        .ok()?;
    chunks
        .last()?
        .upload_date_time()
        .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
}

/// Forward-probe from a fresh cache hint to the true latest volume, comparing S3
/// upload times (list-only, no chunk downloads). Returns the resolved volume and
/// the number of list calls made (threaded into `init_at_volume` for accurate
/// request accounting). Returns `None` — forcing the caller to fall back to the
/// library's binary search — when the hint slot itself is empty or its upload
/// time is already older than [`VOLUME_HINT_MAX_AGE_SECS`].
async fn probe_latest_from_hint(
    site_id: &str,
    hint: nexrad_data::aws::realtime::VolumeIndex,
) -> Option<(nexrad_data::aws::realtime::VolumeIndex, usize)> {
    let mut calls = 1;
    let mut candidate = hint;
    let mut candidate_upload = slot_newest_upload_secs(site_id, hint).await?;

    // Guard against a slot that survived the recency gate by cached-at time but
    // whose data is actually stale (e.g. the site went offline): if the hint's
    // own newest chunk is old, don't trust it — fall back to a full search.
    if current_timestamp_f64() - candidate_upload >= VOLUME_HINT_MAX_AGE_SECS {
        return None;
    }

    loop {
        let next = candidate.next();
        let next_upload = slot_newest_upload_secs(site_id, next).await;
        calls += 1;
        if probe_should_advance(candidate_upload, next_upload) {
            candidate = next;
            candidate_upload = next_upload.expect("Some by probe_should_advance");
        } else {
            break;
        }
    }

    Some((candidate, calls))
}

/// Discover the latest volume for a site and initialize a [`StreamingState`] at
/// it.
///
/// Fast path: if [`get_cached_volume_hint`] returns a hint cached within
/// [`VOLUME_HINT_MAX_AGE_SECS`], forward-probe from it ([`probe_latest_from_hint`])
/// — usually 1–3 list calls — and init there. Any miss (no hint, stale, empty
/// slot, or stale slot data) falls through to the library's rotated-array binary
/// search via `get_latest_volume`, so there is no correctness regression.
async fn acquire_streaming_state(site_id: &str) -> nexrad_data::result::Result<StreamingInit> {
    if let Some((hint, cached_at)) = get_cached_volume_hint(site_id) {
        if current_timestamp_f64() - cached_at < VOLUME_HINT_MAX_AGE_SECS {
            if let Some((volume, calls)) = probe_latest_from_hint(site_id, hint).await {
                log::debug!(
                    "Realtime: resumed {} from cached volume hint {} → latest {} ({} list calls)",
                    site_id,
                    hint.as_number(),
                    volume.as_number(),
                    calls
                );
                return StreamingState::init_at_volume(site_id, volume, calls).await;
            }
        }
    }

    let result = nexrad_data::aws::realtime::get_latest_volume(site_id).await?;
    let volume = result.volume.ok_or(nexrad_data::result::Error::AWS(
        nexrad_data::result::aws::AWSError::LatestVolumeNotFound,
    ))?;
    StreamingState::init_at_volume(site_id, volume, result.calls).await
}

/// Run [`acquire_streaming_state`] with a timeout to avoid indefinite waiting
/// when the site has no data or is unreachable. Each `.await` is a cancellation
/// point — when the timeout wins the select, the init future is dropped, which
/// drops any in-flight HTTP request futures and cancels them.
///
/// On any failure this reports the error to the UI, clears the `active` flag and
/// requests a repaint, then returns `None`; the caller simply returns.
pub(super) async fn acquire_with_timeout(
    site_id: &str,
    active: &Cell<bool>,
    results_tx: &UnboundedSender<RealtimeResult>,
    ctx: &egui::Context,
) -> Option<StreamingInit> {
    const ACQUIRE_TIMEOUT_SECS: u32 = 10;

    let init_future = acquire_streaming_state(site_id);
    let timeout_future = sleep_ms(ACQUIRE_TIMEOUT_SECS * 1000);

    futures_util::pin_mut!(init_future);
    futures_util::pin_mut!(timeout_future);

    match futures_util::future::select(init_future, timeout_future).await {
        futures_util::future::Either::Left((Ok(init), _)) => Some(init),
        futures_util::future::Either::Left((Err(e), _)) => {
            let _ = results_tx.unbounded_send(RealtimeResult::Error(format!(
                "Failed to initialize: {}",
                e
            )));
            active.set(false);
            ctx.request_repaint();
            None
        }
        futures_util::future::Either::Right(_) => {
            log::warn!(
                "Realtime acquisition timed out after {}s for site {}",
                ACQUIRE_TIMEOUT_SECS,
                site_id
            );
            let _ = results_tx.unbounded_send(RealtimeResult::Error(format!(
                "Acquisition timed out after {}s — data may be unavailable for this site",
                ACQUIRE_TIMEOUT_SECS
            )));
            active.set(false);
            ctx.request_repaint();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn recency_gate_boundary() {
        let now = 1_700_001_200.0;
        // Just inside the 20-minute window is fresh.
        let cached_at = now - (VOLUME_HINT_MAX_AGE_SECS - 1.0);
        assert!(now - cached_at < VOLUME_HINT_MAX_AGE_SECS);
        // Just outside is stale.
        let cached_at = now - (VOLUME_HINT_MAX_AGE_SECS + 1.0);
        assert!(now - cached_at >= VOLUME_HINT_MAX_AGE_SECS);
    }

    #[wasm_bindgen_test]
    fn probe_advances_only_to_strictly_newer_slots() {
        // Next slot newer → advance.
        assert!(probe_should_advance(100.0, Some(150.0)));
        // Next slot not yet written → stop.
        assert!(!probe_should_advance(100.0, None));
        // Next slot is an older recycled slot (e.g. wrap to slot 1) → stop.
        assert!(!probe_should_advance(100.0, Some(50.0)));
        // Equal upload time (no progress) → stop.
        assert!(!probe_should_advance(100.0, Some(100.0)));
    }
}
