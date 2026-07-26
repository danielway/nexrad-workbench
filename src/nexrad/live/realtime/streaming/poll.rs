//! The fetch/poll cluster: waiting for the next chunk to become available and
//! actually downloading it.
//!
//! Two halves. [`wait_for_next_target`] is the adaptive cross-volume wait — it
//! list-probes the S3 slot the target lives in so a long dead-reckoned sleep can
//! early-fire or re-anchor. [`fetch_next_chunk`] is the retry loop that turns a
//! `StreamingState` advance into either a downloaded chunk, a synthetic
//! volume-end, or a terminal error, applying `REALTIME_CHUNK_POLICY` to 404s and
//! transient transport failures.

use super::current_timestamp_f64;
use super::loop_state::{drain_control, interruptible_sleep, LoopState, SleepOutcome};
use crate::core::projection::SharedProjectionEngine;
use crate::core::StreamingFilter;
use crate::net::retry::{
    attempt_with_timeout, compute_delay, sleep_duration, Verdict, REALTIME_CHUNK_POLICY,
};
use crate::nexrad::live::realtime::ControlMessage;
use crate::nexrad::live::streaming_state::StreamingState;
use eframe::egui;
use futures_channel::mpsc::UnboundedReceiver;
use nexrad_data::aws::realtime::ChunkIdentifier;

/// Either an actual downloaded chunk or a synthetic-volume-end signal from
/// the filter-aware fetch path. Plumbed through the retry loop's `Verdict`
/// so the existing 404 / transient-error handling stays unchanged.
#[derive(Debug)]
enum FilterFetchResult {
    Downloaded(nexrad_data::aws::realtime::DownloadedChunk),
    SyntheticEnd,
}

/// Map a [`crate::nexrad::live::streaming_state::TryNextOutcome`] to a retry [`Verdict`]
/// for the filter-aware fetch path. Mirrors [`classify_chunk_result`] for
/// the unfiltered path; the only new case is `SyntheticVolumeEnd`, which is
/// not a retry — it's a terminal-for-this-iteration outcome the loop turns
/// into a synthetic `is_volume_end` signal.
fn classify_filter_outcome(
    result: nexrad_data::result::Result<crate::nexrad::live::streaming_state::TryNextOutcome>,
) -> Verdict<FilterFetchResult> {
    use crate::nexrad::live::streaming_state::TryNextOutcome;
    use nexrad_data::result::aws::AWSError;
    use nexrad_data::result::Error;
    match result {
        Ok(TryNextOutcome::Downloaded(c)) => Verdict::Ok(FilterFetchResult::Downloaded(c)),
        Ok(TryNextOutcome::NotYetAvailable) => Verdict::Retry { after: None },
        Ok(TryNextOutcome::SyntheticVolumeEnd) => Verdict::Ok(FilterFetchResult::SyntheticEnd),
        Err(Error::AWS(
            AWSError::S3GetObjectRequest(_)
            | AWSError::S3GetObject(_)
            | AWSError::S3Streaming(_)
            | AWSError::S3ListObjects(_)
            | AWSError::TruncatedListObjectsResponse,
        )) => Verdict::Retry { after: None },
        Err(e) => Verdict::Terminal(format!("{}", e)),
    }
}

/// Map a `nexrad-data` chunk-fetch result to a retry [`Verdict`].
///
/// `Ok(None)` (S3 returned 404 — chunk not yet published) is treated as a
/// retryable miss in this call site, since real-time chunks land seconds late
/// by design. Transport-layer errors are also retryable; data-decoding and
/// identifier errors are terminal.
fn classify_chunk_result(
    result: nexrad_data::result::Result<Option<nexrad_data::aws::realtime::DownloadedChunk>>,
) -> Verdict<FilterFetchResult> {
    use nexrad_data::result::aws::AWSError;
    use nexrad_data::result::Error;
    match result {
        Ok(Some(chunk)) => Verdict::Ok(FilterFetchResult::Downloaded(chunk)),
        Ok(None) => Verdict::Retry { after: None },
        Err(Error::AWS(
            AWSError::S3GetObjectRequest(_)
            | AWSError::S3GetObject(_)
            | AWSError::S3Streaming(_)
            | AWSError::S3ListObjects(_)
            | AWSError::TruncatedListObjectsResponse,
        )) => Verdict::Retry { after: None },
        Err(e) => Verdict::Terminal(format!("{}", e)),
    }
}

/// Fetch the next chunk. The first attempt fires at the timing-prediction
/// sleep in the caller; if it returns 404 (chunk not yet published) or a
/// transient transport error, the loop below applies the standard
/// exponential-backoff-with-jitter policy from `REALTIME_CHUNK_POLICY`.
/// The retry loop is inlined (rather than going through `with_retry`)
/// because each attempt borrows `iter` mutably and the resulting Future
/// can't escape an `FnMut` closure body.
///
/// Returns the fetch result plus whether the filter-aware path produced a
/// synthetic volume end (reported through the retry loop as a dedicated
/// outcome the post-retry match has to surface; tagging the verdict here lets
/// the existing retry loop continue to drive 404 + transient errors
/// uniformly). `none_retries` and `last_empty_at` are the caller's per-chunk
/// arrival counters, advanced in place on every empty poll.
pub(super) async fn fetch_next_chunk(
    iter: &mut StreamingState,
    active_filter: StreamingFilter,
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
    chunk_fetch_start: web_time::Instant,
    none_retries: &mut u32,
    last_empty_at: &mut Option<f64>,
) -> (
    Result<nexrad_data::aws::realtime::DownloadedChunk, String>,
    bool,
) {
    let policy = &REALTIME_CHUNK_POLICY;
    let mut last_msg: Option<String> = None;
    let mut synthetic_volume_end = false;
    let fetch_outcome: Result<nexrad_data::aws::realtime::DownloadedChunk, String> = 'retry: {
        for attempt in 1..=policy.max_attempts {
            drain_control(loop_state, control_rx);
            if loop_state.stop_requested {
                break 'retry Err("stopped".into());
            }
            if attempt > 1 {
                *none_retries = attempt - 1;
                *last_empty_at = Some(current_timestamp_f64());
            }
            let verdict = match active_filter {
                StreamingFilter::All => {
                    attempt_with_timeout(
                        async { classify_chunk_result(iter.try_next().await) },
                        policy.per_attempt_timeout,
                    )
                    .await
                }
                StreamingFilter::Elevation(_) => {
                    attempt_with_timeout(
                        async {
                            let outcome = iter
                                .try_next_matching(
                                    // See the matching `accept_end=false`
                                    // comment on `next_matching_chunk_diagnostics`
                                    // above.
                                    false,
                                    |elev| active_filter.accepts(elev),
                                )
                                .await;
                            classify_filter_outcome(outcome)
                        },
                        policy.per_attempt_timeout,
                    )
                    .await
                }
            };
            match verdict {
                Verdict::Ok(FilterFetchResult::Downloaded(c)) => {
                    break 'retry Ok(c);
                }
                Verdict::Ok(FilterFetchResult::SyntheticEnd) => {
                    synthetic_volume_end = true;
                    break 'retry Err("synthetic_volume_end".into());
                }
                Verdict::Terminal(m) => break 'retry Err(m),
                Verdict::Retry { after } => {
                    last_msg = Some(format!("attempt {} empty/transient", attempt));
                    if attempt >= policy.max_attempts {
                        break;
                    }
                    let elapsed = chunk_fetch_start.elapsed();
                    if elapsed >= policy.total_budget {
                        break 'retry Err(format!(
                            "chunk_fetch: budget {}s exhausted after {} attempts",
                            policy.total_budget.as_secs(),
                            attempt
                        ));
                    }
                    let mut delay = compute_delay(policy, attempt, after);
                    let remaining = policy.total_budget.saturating_sub(elapsed);
                    if delay > remaining {
                        delay = remaining;
                    }
                    sleep_duration(delay).await;
                }
            }
        }
        Err(format!(
            "chunk_fetch: gave up after {} attempts ({})",
            policy.max_attempts,
            last_msg.as_deref().unwrap_or("retry exhausted")
        ))
    };

    (fetch_outcome, synthetic_volume_end)
}

// ── Adaptive cross-volume wait (list-probe early-fire + re-anchor) ─────────

/// Minimum predicted wait (seconds) before the adaptive list-probe engages.
/// Below this the single-sleep path is used verbatim — short / same-volume
/// waits don't accumulate enough projection error to be worth an S3 list call.
pub(super) const LIST_PROBE_THRESHOLD_SECS: f64 = 30.0;

/// Cadence (seconds) of the list probe during a long cross-volume wait. Also
/// the worst-case overshoot: once the target is published, the next probe
/// fires within this window and breaks the sleep. Trades S3 list calls for
/// timing tightness.
const LIST_PROBE_CADENCE_SECS: f64 = 20.0;

/// Whether the adaptive list-probe should engage for this wait. Any long wait
/// (cross-volume OR a filtered current-volume gap) benefits: cross-volume from
/// the re-anchor correction, current-volume from flipping a future cut to
/// `AvailableNotCollected` live as soon as it publishes. The caller lists the
/// volume the target lives in. Pure — split out for testing.
pub(super) fn should_list_now(
    wait_ms: u32,
    _target_in_next_volume: bool,
    threshold_ms: u32,
) -> bool {
    wait_ms > threshold_ms
}

/// Whether the next-volume slot has started writing the *new* volume (vs. still
/// holding the previous occupant of this rotating slot). The new volume's
/// chunks upload strictly later than our current-volume anchor, while a
/// recycled old occupant uploaded long ago — so "strictly newer than the
/// anchor" is the rollover signal. Reuses [`super::acquire::probe_should_advance`]. Pure.
fn slot_is_fresh(prev_anchor_upload_secs: f64, slot_newest_upload_secs: Option<f64>) -> bool {
    super::acquire::probe_should_advance(prev_anchor_upload_secs, slot_newest_upload_secs)
}

/// Whether the target chunk is already published in the listing. Presence is by
/// sequence: the target sequence was resolved by the projector to the user's
/// filtered elevation, and chunks publish in order, so any published sequence
/// `>= target_seq` means the target is available. An End chunk also fires
/// (the volume finished — possibly shorter than projected — and the fetch path
/// will roll over). Pure — split out for testing.
fn target_present_in_listing(
    listed_seqs: &[usize],
    listed_has_end: bool,
    target_seq: usize,
) -> bool {
    listed_has_end || listed_seqs.iter().any(|&s| s >= target_seq)
}

/// Convert a fresh projected poll time into a remaining-wait in milliseconds,
/// clamped to zero. Pure — split out for testing.
fn recompute_remaining_wait_ms(new_poll_at_secs: f64, now_secs: f64) -> u32 {
    ((new_poll_at_secs - now_secs).max(0.0) * 1000.0) as u32
}

/// Newest-published chunk's S3 upload time (Unix seconds) from a listing, or
/// `None` when the slot is empty / carries no upload metadata. Assumes the
/// listing is sequence-sorted (S3 ListObjects is lexicographic), so the last
/// entry is newest — same assumption as `slot_newest_upload_secs`.
fn listing_newest_upload_secs(
    listed: &[nexrad_data::aws::realtime::ChunkIdentifier],
) -> Option<f64> {
    listed
        .last()?
        .upload_date_time()
        .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
}

/// Sleep until the next download target should be available, periodically
/// listing the next-volume S3 slot to (a) early-fire the moment the target is
/// published and (b) re-anchor the remaining-wait projection on a freshly
/// published chunk — correcting the accumulated-error overshoot of the
/// single-sleep path.
///
/// Returns the terminal [`SleepOutcome`] (so the caller keeps its existing
/// stop/filter-change/completed handling) plus how the wait resolved (for
/// per-chunk arrival diagnostics). Never mutates the download cursor: re-anchor
/// goes through [`StreamingState::build_plan_from_anchor`], which leaves
/// `self.current` intact.
///
/// `prev_anchor_upload_secs` is the current-volume anchor's S3 upload time, the
/// reference for the rotating-slot freshness guard.
#[allow(clippy::too_many_arguments)]
pub(super) async fn wait_for_next_target(
    site_id: &str,
    engine: &SharedProjectionEngine,
    cursor_anchor: &ChunkIdentifier,
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
    ctx: &egui::Context,
    initial_wait_ms: u32,
    target_seq: usize,
    target_volume: nexrad_data::aws::realtime::VolumeIndex,
    prev_anchor_upload_secs: Option<f64>,
    wake_epoch: u64,
) -> (SleepOutcome, crate::core::WaitResolution) {
    use crate::core::WaitResolution;
    use nexrad_data::aws::realtime::{list_chunks_in_volume, ChunkType};

    let cadence_ms = (LIST_PROBE_CADENCE_SECS * 1000.0) as u32;
    let mut remaining_ms = initial_wait_ms;
    let mut resolution = WaitResolution::SleptToPrediction;

    loop {
        // Final approach: within one cadence of the target, just sleep the
        // remainder — no point listing right before the fetch attempt.
        if remaining_ms <= cadence_ms {
            let outcome =
                interruptible_sleep(loop_state, control_rx, ctx, remaining_ms, wake_epoch).await;
            return (outcome, resolution);
        }

        // Sleep one segment; bail immediately on stop / filter change.
        match interruptible_sleep(loop_state, control_rx, ctx, cadence_ms, wake_epoch).await {
            SleepOutcome::Completed => {}
            other => return (other, resolution),
        }
        remaining_ms = remaining_ms.saturating_sub(cadence_ms);

        // Re-check stop before spending a network request.
        drain_control(loop_state, control_rx);
        if loop_state.stop_requested {
            return (SleepOutcome::Stopped, resolution);
        }

        // Probe the next-volume slot. A failure is non-fatal: keep the
        // existing schedule, so we're never worse than the single-sleep path.
        let listed = match list_chunks_in_volume(site_id, target_volume, 100).await {
            Ok(ids) => ids,
            Err(e) => {
                log::warn!(
                    "list-probe: list_chunks_in_volume failed: {} — continuing scheduled wait",
                    e
                );
                continue;
            }
        };

        // Rotating-slot freshness guard: skip both early-fire and re-anchor
        // until the slot actually holds the new volume.
        let slot_newest = listing_newest_upload_secs(&listed);
        let fresh = match prev_anchor_upload_secs {
            Some(prev) => slot_is_fresh(prev, slot_newest),
            None => slot_newest.is_some(),
        };
        if !fresh {
            log::debug!("list-probe: next-volume slot not fresh yet (rollover pending)");
            continue;
        }

        // Feed the listing into the shared inventory so every surface (not just
        // this loop's sleep) reflects what's now published — this is what makes
        // re-anchoring visible in the timeline / VCP panel.
        engine.borrow_mut().observe_listing(target_volume, &listed);

        let listed_seqs: Vec<usize> = listed.iter().map(|c| c.sequence()).collect();
        let has_end = listed.iter().any(|c| c.chunk_type() == ChunkType::End);
        if target_present_in_listing(&listed_seqs, has_end, target_seq) {
            log::debug!(
                "list-probe: early-fire (target seq {} present in next-volume slot)",
                target_seq
            );
            return (SleepOutcome::Completed, WaitResolution::EarlyFired);
        }

        // Re-anchor the remaining wait. We already fed the listing into the
        // inventory above, so the engine's normal cursor-anchored projection
        // now self-anchors the next volume on that fresh measurement — fresh
        // timing in the right frame (offset 1), no separate anchor. Recompute
        // the remaining wait from it.
        let now2 = current_timestamp_f64();
        let new_poll = engine
            .borrow_mut()
            .projection(cursor_anchor, now2)
            .and_then(|p| {
                p.next_target()
                    .and_then(|t| t.projected.as_ref())
                    .map(|f| f.poll_at_secs)
            });
        if let Some(new_poll_at) = new_poll {
            let new_remaining = recompute_remaining_wait_ms(new_poll_at, now2);
            log::debug!(
                "list-probe: re-anchored from inventory — remaining wait {}ms (was {}ms)",
                new_remaining,
                remaining_ms,
            );
            remaining_ms = new_remaining;
            resolution = WaitResolution::ReAnchored;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn should_list_now_for_any_long_wait() {
        let threshold = 30_000;
        // Above threshold → probe (cross-volume or same-volume).
        assert!(should_list_now(45_000, true, threshold));
        assert!(should_list_now(120_000, false, threshold));
        // Below / at threshold → no probe.
        assert!(!should_list_now(20_000, true, threshold));
        assert!(!should_list_now(30_000, false, threshold));
    }

    #[wasm_bindgen_test]
    fn slot_is_fresh_detects_rollover() {
        // New volume's chunks upload later than our anchor → fresh.
        assert!(slot_is_fresh(100.0, Some(150.0)));
        // Slot empty / no metadata → not fresh.
        assert!(!slot_is_fresh(100.0, None));
        // Recycled older occupant (uploaded long ago) → not fresh.
        assert!(!slot_is_fresh(100.0, Some(50.0)));
        // Equal upload time (no progress) → not fresh.
        assert!(!slot_is_fresh(100.0, Some(100.0)));
    }

    #[wasm_bindgen_test]
    fn target_present_in_listing_fires_on_seq_or_end() {
        // Exact target sequence present → fire.
        assert!(target_present_in_listing(&[1, 2, 3], false, 3));
        // A higher sequence present → fire (target already passed).
        assert!(target_present_in_listing(&[1, 2, 3, 4], false, 3));
        // Target not yet reached → wait.
        assert!(!target_present_in_listing(&[1, 2], false, 3));
        // End chunk present (volume shorter than projected) → fire.
        assert!(target_present_in_listing(&[1, 2], true, 9));
        // Empty listing → wait.
        assert!(!target_present_in_listing(&[], false, 3));
    }

    #[wasm_bindgen_test]
    fn recompute_remaining_wait_clamps_to_zero() {
        // Future poll time → positive remaining wait in ms.
        assert_eq!(recompute_remaining_wait_ms(105.0, 100.0), 5_000);
        // Poll time already passed → clamps to zero (fetch immediately).
        assert_eq!(recompute_remaining_wait_ms(95.0, 100.0), 0);
        // Exactly now → zero.
        assert_eq!(recompute_remaining_wait_ms(100.0, 100.0), 0);
    }
}
