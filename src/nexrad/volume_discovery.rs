//! Fast discovery of the current volume directory in the real-time NEXRAD S3 bucket.
//!
//! The real-time bucket holds 999 round-robin volume directories. At any moment
//! exactly one is being written to; the rest hold older data from prior passes
//! around the ring. Finding the current directory is a prerequisite for
//! streaming, and `nexrad-data`'s `get_latest_volume()` does it in ~10
//! sequential LIST requests via a binary search.
//!
//! This module exploits two pieces of information the binary search ignores.
//! First, **every LIST returns a timestamp**, not just a presence bit, so a
//! handful of probes plus an estimated cadence is enough to pinpoint the
//! active volume. Second, the streaming loop already records an EWMA of
//! observed volume cadence and persists it (with the volume number and the
//! wall-clock observation time) into [`VolumeHint`]. With a recent hint we
//! can extrapolate forward and probe a single volume; without one we do a
//! coarse parallel sweep and triangulate from the newest probe + the prior
//! cadence.
//!
//! Both warm and cold paths resolve in 1–2 round trips. The original
//! rotated-array binary search (ported verbatim from nexrad-data 1.0.0-rc.7,
//! see [`search`] below) is kept only as a defensive fallback for the rare
//! case where even the cold sweep finds no valid volumes.

use chrono::{DateTime, Utc};
use futures_util::future::join_all;
use nexrad_data::aws::realtime::{list_chunks_in_volume, VolumeIndex};
use nexrad_data::result::Result;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

use super::timing::VolumeCadenceTracker;

/// Number of volumes around the predicted volume to probe in parallel when
/// the single-shot prediction misses (stale or off-by-N due to a cadence
/// shift). Window is `predicted - WARM_SPREAD_BACK ..= predicted +
/// WARM_SPREAD_FWD`, biased forward because clocks drift slow more often
/// than fast. 7 probes covers ±3 typical and one outlier slot.
const WARM_SPREAD_BACK: i64 = 3;
const WARM_SPREAD_FWD: i64 = 3;

/// Number of probes in cold-start round 1 (wide spread). 8 probes spaced
/// ~125 volumes apart is enough to guarantee at least one hit in the
/// "fresh" portion of the ring (last few hours of the cycle) for any
/// operational site.
const COLD_ROUND1_COUNT: usize = 8;

/// Number of probes in cold-start round 2 (narrow window around the
/// extrapolated prediction). Asymmetric forward bias since the active
/// volume is the *newest* one, so the prediction is more likely to be a
/// volume or two behind than ahead.
const COLD_ROUND2_BACK: i64 = 2;
const COLD_ROUND2_FWD: i64 = 5;

/// Number of real-time volume directories in the S3 bucket.
///
/// Volumes are numbered 1..=999 (see `VolumeIndex::next()` wrapping 999→1).
const VOLUME_COUNT: usize = 999;

/// Maximum staleness (in cadence units) of a probe's timestamp for it to
/// count as "the active volume". Allows ±1.5 cadence of clock skew or
/// prediction drift.
const FRESHNESS_CADENCES: f64 = 1.5;

/// Persisted hint passed to `find_latest_volume` on session start.
///
/// Stored as JSON in localStorage by the streaming loop. Carries enough
/// state to extrapolate the current active volume from elapsed wall-clock,
/// usually in a single probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeHint {
    /// Schema version. Bump on shape change so old caches are ignored.
    pub version: u32,
    /// Volume number observed active at `observed_at_ms`.
    pub volume: u16,
    /// Wall-clock (Unix milliseconds) at the moment this hint was last saved.
    pub observed_at_ms: i64,
    /// EWMA of observed volume duration in seconds, from
    /// [`VolumeCadenceTracker`]. Used both as the extrapolation rate and as
    /// the prior for cold-start triangulation if the hint turns out stale.
    pub cadence_secs: f64,
}

impl VolumeHint {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new(volume: VolumeIndex, observed_at_ms: i64, cadence_secs: f64) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            volume: volume.as_number() as u16,
            observed_at_ms,
            cadence_secs,
        }
    }

    fn as_volume_index(&self) -> Option<VolumeIndex> {
        let n = self.volume as usize;
        if (1..=VOLUME_COUNT).contains(&n) {
            Some(VolumeIndex::new(n))
        } else {
            None
        }
    }

    fn cadence_or_default(&self) -> f64 {
        if self.cadence_secs.is_finite() && self.cadence_secs > 0.0 {
            self.cadence_secs
        } else {
            VolumeCadenceTracker::DEFAULT_SECS
        }
    }
}

/// Result of a volume search including the request count (for network stats).
#[derive(Debug, Clone)]
pub struct VolumeSearchResult {
    pub volume: Option<VolumeIndex>,
    pub requests_made: usize,
}

/// Advance a volume index by `offset`, wrapping 999 → 1 (matching `VolumeIndex::next()`).
fn advance(vol: VolumeIndex, offset: usize) -> VolumeIndex {
    let n = vol.as_number();
    let wrapped = ((n - 1 + offset) % VOLUME_COUNT) + 1;
    VolumeIndex::new(wrapped)
}

/// Signed-offset variant of [`advance`], for the spread-around-prediction
/// helpers that need to go a few volumes backward as well as forward.
fn advance_signed(vol: VolumeIndex, signed_offset: i64) -> VolumeIndex {
    let n = vol.as_number() as i64;
    let v = (n - 1 + signed_offset).rem_euclid(VOLUME_COUNT as i64) + 1;
    VolumeIndex::new(v as usize)
}

/// Probe a single volume's first-chunk upload time.
async fn probe(site: &str, vol: VolumeIndex) -> (VolumeIndex, Option<DateTime<Utc>>) {
    match list_chunks_in_volume(site, vol, 1).await {
        Ok(chunks) => (vol, chunks.first().and_then(|c| c.upload_date_time())),
        Err(_) => (vol, None),
    }
}

/// Probe a batch of volumes concurrently, returning results in input order.
async fn probe_batch(
    site: &str,
    volumes: &[VolumeIndex],
) -> Vec<(VolumeIndex, Option<DateTime<Utc>>)> {
    let futures: Vec<_> = volumes.iter().map(|&v| probe(site, v)).collect();
    join_all(futures).await
}

/// Predictive warm path: extrapolate from `hint` and probe a single volume.
/// On miss, probe a small spread around the prediction. Returns `Some(vol)`
/// on success; `None` to indicate the caller should fall through to the
/// cold-start triangulation.
async fn predict_warm(
    site: &str,
    hint: &VolumeHint,
    total_requests: &mut usize,
) -> Option<VolumeIndex> {
    let hint_vol = hint.as_volume_index()?;
    let cadence = hint.cadence_or_default();
    let now = Utc::now();
    let elapsed_ms = now.timestamp_millis() - hint.observed_at_ms;
    if elapsed_ms < 0 {
        // Clock went backward — hint is unusable for prediction.
        return None;
    }
    let elapsed_secs = (elapsed_ms as f64) / 1000.0;
    let predicted_offset = (elapsed_secs / cadence).round() as i64;
    let predicted_offset_mod = predicted_offset.rem_euclid(VOLUME_COUNT as i64) as usize;
    let predicted = advance(hint_vol, predicted_offset_mod);

    // Round 1: single probe at the predicted volume.
    let (vol, ts) = probe(site, predicted).await;
    *total_requests += 1;
    if let Some(t) = ts {
        let stale_secs = (now - t).num_seconds().abs() as f64;
        if stale_secs < cadence * FRESHNESS_CADENCES {
            log::debug!(
                "volume_discovery: warm prediction hit volume {} (stale {:.0}s, cadence {:.0}s)",
                vol.as_number(),
                stale_secs,
                cadence
            );
            return Some(vol);
        }
    }

    // Round 2: spread of ±N around the prediction. Handles cadence drift,
    // clock skew, and the "we hit a volume but its first chunk hasn't
    // landed yet" edge case where the predicted volume returns None but
    // its neighbour is the live one.
    let spread: Vec<VolumeIndex> = (-WARM_SPREAD_BACK..=WARM_SPREAD_FWD)
        .map(|d| advance_signed(predicted, d))
        .collect();
    let probes = probe_batch(site, &spread).await;
    *total_requests += spread.len();

    let cutoff = now - chrono::Duration::seconds((cadence * FRESHNESS_CADENCES) as i64);
    let best = probes
        .iter()
        .filter_map(|(v, t)| t.map(|t| (*v, t)))
        .filter(|(_, t)| *t > cutoff)
        .max_by_key(|(_, t)| *t)
        .map(|(v, _)| v);

    if let Some(v) = best {
        log::debug!(
            "volume_discovery: warm spread hit volume {} (predicted {})",
            v.as_number(),
            predicted.as_number()
        );
    } else {
        log::debug!(
            "volume_discovery: warm prediction missed (predicted {}); falling through to cold",
            predicted.as_number()
        );
    }
    best
}

/// Cold-start triangulation: 8 wide-spread probes to find the freshest
/// volume in the ring, extrapolate forward using `cadence_secs` (either the
/// stale-hint cadence or `VolumeCadenceTracker::DEFAULT_SECS`), then
/// probe a small forward window to lock onto the active volume.
async fn triangulate_cold(
    site: &str,
    cadence_secs: f64,
    total_requests: &mut usize,
) -> Option<VolumeIndex> {
    // Round 1: 8 probes evenly spaced across the 999-entry ring.
    let step = VOLUME_COUNT / COLD_ROUND1_COUNT;
    let volumes: Vec<VolumeIndex> = (0..COLD_ROUND1_COUNT)
        .map(|i| VolumeIndex::new(i * step + 1))
        .collect();
    let probes = probe_batch(site, &volumes).await;
    *total_requests += volumes.len();

    let (newest_vol, newest_ts) = probes
        .iter()
        .filter_map(|(v, t)| t.map(|t| (*v, t)))
        .max_by_key(|(_, t)| *t)?;

    // Extrapolate using the cadence prior. With 8 probes spaced 125 apart,
    // newest_vol is at most 125 volumes behind the active volume; the
    // extrapolation collapses that to within ±a few volumes.
    let now = Utc::now();
    let elapsed_secs = (now - newest_ts).num_seconds().max(0) as f64;
    let cadence = if cadence_secs.is_finite() && cadence_secs > 0.0 {
        cadence_secs
    } else {
        VolumeCadenceTracker::DEFAULT_SECS
    };
    let predicted_offset = (elapsed_secs / cadence).round() as i64;
    let predicted_offset_mod = predicted_offset.rem_euclid(VOLUME_COUNT as i64) as usize;
    let predicted = advance(newest_vol, predicted_offset_mod);

    // Round 2: probe a small asymmetric window around the prediction,
    // biased forward (active volume is the newest, so prediction tends to
    // sit a volume or two behind it).
    let spread: Vec<VolumeIndex> = (-COLD_ROUND2_BACK..=COLD_ROUND2_FWD)
        .map(|d| advance_signed(predicted, d))
        .collect();
    let r2 = probe_batch(site, &spread).await;
    *total_requests += spread.len();

    let cutoff = now - chrono::Duration::seconds((cadence * FRESHNESS_CADENCES) as i64);
    let best = r2
        .iter()
        .filter_map(|(v, t)| t.map(|t| (*v, t)))
        .filter(|(_, t)| *t > cutoff)
        .max_by_key(|(_, t)| *t)
        .map(|(v, _)| v);

    log::debug!(
        "volume_discovery: cold triangulate (newest probe {} @ -{:.0}s, predicted {}) → {:?}",
        newest_vol.as_number(),
        elapsed_secs,
        predicted.as_number(),
        best.map(|v| v.as_number())
    );
    best
}

/// Finds the latest volume directory for the given site.
///
/// Strategy: (1) if a `VolumeHint` is provided, extrapolate from it and
/// probe one volume (warm path); on miss, probe a small spread. (2) On
/// hint miss or absence, do a wide parallel sweep + extrapolate +
/// narrow probe (cold triangulation). (3) Only if even cold triangulation
/// finds nothing fresh, fall back to the sequential rotated-array binary
/// search for correctness.
pub async fn find_latest_volume(
    site: &str,
    hint: Option<VolumeHint>,
) -> Result<VolumeSearchResult> {
    let mut total_requests = 0usize;
    let cadence_for_cold = hint
        .as_ref()
        .map(|h| h.cadence_or_default())
        .unwrap_or(VolumeCadenceTracker::DEFAULT_SECS);

    if let Some(h) = hint.as_ref() {
        if let Some(v) = predict_warm(site, h, &mut total_requests).await {
            return Ok(VolumeSearchResult {
                volume: Some(v),
                requests_made: total_requests,
            });
        }
    }

    if let Some(v) = triangulate_cold(site, cadence_for_cold, &mut total_requests).await {
        return Ok(VolumeSearchResult {
            volume: Some(v),
            requests_made: total_requests,
        });
    }

    // Defensive fallback: original rotated-array binary search. Reached
    // only when the wide sweep returns no usable timestamps (e.g., site
    // genuinely offline or bucket empty for this site).
    log::debug!(
        "volume_discovery: cold triangulation found nothing fresh, falling back to binary search"
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let found_index = {
        let calls = Arc::clone(&calls);
        search(VOLUME_COUNT, DateTime::<Utc>::MAX_UTC, |i| {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Relaxed);
                let chunks = list_chunks_in_volume(site, VolumeIndex::new(i + 1), 1).await?;
                Ok(chunks.first().and_then(|c| c.upload_date_time()))
            }
        })
        .await?
    };
    total_requests += calls.load(Relaxed);

    let volume = found_index.map(|i| VolumeIndex::new(i + 1));
    log::debug!(
        "volume_discovery: binary search resolved to {:?} in {} requests",
        volume.as_ref().map(|v| v.as_number()),
        total_requests
    );
    Ok(VolumeSearchResult {
        volume,
        requests_made: total_requests,
    })
}

// ── Rotated-array binary search ─────────────────────────────────────────
//
// Ported verbatim from nexrad-data 1.0.0-rc.7
// (`src/aws/realtime/search.rs`), which handles the rotated sorted array
// with arbitrary None gaps at the pivot point. Kept local so we can iterate
// on the discovery strategy independently of the upstream crate. Upstream
// when stable.

/// Performs an efficient search of elements to locate the nearest element to `target` without going
/// over. Assumes there are `element_count` elements in a rotated sorted array with zero or many
/// `None` values at the pivot point. Returns `None` if there are no values less than the `target`.
async fn search<F, V>(
    element_count: usize,
    target: V,
    mut f: impl FnMut(usize) -> F,
) -> Result<Option<usize>>
where
    F: Future<Output = Result<Option<V>>>,
    V: PartialOrd + Clone,
{
    if element_count == 0 {
        return Ok(None);
    }

    let some_target = Some(&target);
    let mut nearest = None;

    let mut first_value = f(0).await?;
    let mut first_value_ref = first_value.as_ref();

    if first_value_ref == some_target {
        return Ok(Some(0));
    }

    let mut low = 0;
    let mut high = element_count;

    // First, locate any value in the array to use as a reference point via repeated bisection.
    let mut queue = VecDeque::from([(0, element_count - 1)]);
    while !queue.is_empty() {
        if let Some((start, end)) = queue.pop_front() {
            if start > end {
                continue;
            }

            let mid = (start + end) / 2;
            let mid_value = f(mid).await?;
            let mid_value_ref = mid_value.as_ref();

            // If this value is None, continue the bisection
            if mid_value_ref.is_none() {
                queue.push_back((mid + 1, end));
                if mid > 0 {
                    queue.push_back((start, mid - 1));
                }
                continue;
            }

            if mid_value_ref <= some_target {
                nearest = Some(mid);
            }

            if mid_value_ref == some_target {
                return Ok(nearest);
            }

            if should_search_right(first_value_ref, mid_value_ref, some_target) {
                low = mid + 1;
            } else {
                high = mid;
            }
        }

        break;
    }

    if low >= high {
        return Ok(nearest);
    }

    // Move the low pointer to the first non-None value
    first_value = f(low).await?;
    first_value_ref = first_value.as_ref();

    // Now that we have a reference point, we can perform a binary search for the target
    while low < high {
        let mid = low + (high - low) / 2;

        let value = f(mid).await?;
        let value_ref = value.as_ref();

        if value_ref.is_some() && value_ref <= some_target {
            nearest = Some(mid);
        }

        if value_ref == some_target {
            return Ok(Some(mid));
        }

        if should_search_right(first_value_ref, value_ref, some_target) {
            low = mid + 1;
        } else {
            high = mid;
        }
    }

    Ok(nearest)
}

/// Returns `true` if the search should continue right, `false` if it should continue left.
fn should_search_right<V>(first: V, value: V, target: V) -> bool
where
    V: PartialOrd,
{
    let first_wrapped = first > value;
    let target_wrapped = target < first;

    if value < target {
        !first_wrapped || target_wrapped
    } else {
        first_wrapped && !target_wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_no_wrap() {
        assert_eq!(advance(VolumeIndex::new(5), 3).as_number(), 8);
    }

    #[test]
    fn advance_wraps_at_999() {
        assert_eq!(advance(VolumeIndex::new(998), 3).as_number(), 2);
        assert_eq!(advance(VolumeIndex::new(999), 1).as_number(), 1);
    }

    #[test]
    fn advance_signed_negative_wraps() {
        assert_eq!(advance_signed(VolumeIndex::new(2), -3).as_number(), 998);
        assert_eq!(advance_signed(VolumeIndex::new(1), -1).as_number(), 999);
    }

    #[test]
    fn advance_signed_positive_wraps() {
        assert_eq!(advance_signed(VolumeIndex::new(998), 3).as_number(), 2);
    }

    #[test]
    fn volume_hint_round_trips() {
        let hint = VolumeHint::new(VolumeIndex::new(123), 1_700_000_000_000, 280.0);
        let json = serde_json::to_string(&hint).unwrap();
        let parsed: VolumeHint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.volume, 123);
        assert_eq!(parsed.observed_at_ms, 1_700_000_000_000);
        assert!((parsed.cadence_secs - 280.0).abs() < 1e-9);
        assert_eq!(parsed.version, VolumeHint::CURRENT_VERSION);
    }

    // The `search` function is a verbatim port of nexrad-data 1.0.0-rc.7
    // src/aws/realtime/search.rs, which has its own test suite covering
    // rotated arrays with None gaps. Not duplicated here.
}
