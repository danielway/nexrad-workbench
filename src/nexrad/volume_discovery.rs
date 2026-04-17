//! Fast discovery of the current volume directory in the real-time NEXRAD S3 bucket.
//!
//! The real-time bucket holds 999 round-robin volume directories. At any moment
//! exactly one is being written to; the rest hold older data from prior passes
//! around the ring. Finding the current directory is a prerequisite for
//! streaming, and `nexrad-data`'s `get_latest_volume()` does it in ~10
//! sequential LIST requests via a binary search.
//!
//! This module preserves that binary search (ported verbatim from nexrad-data
//! 1.0.0-rc.7, see [`search`] below) for the cold-start case, and adds a
//! cached-hint fast path on top: on reconnection we probe `hint..hint+K`
//! concurrently (one round trip) and only fall through to the binary search
//! when the hint is stale, absent, or too close to the edge of our probed
//! window to trust.

use chrono::{DateTime, Utc};
use futures_util::future::join_all;
use nexrad_data::aws::realtime::{list_chunks_in_volume, VolumeIndex};
use nexrad_data::result::Result;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

/// Number of concurrent probes forward from the cached hint.
///
/// 8 probes covers roughly 40 minutes of volume progression (each volume ~5 min).
/// If the user reconnects within that window we resolve in one round trip;
/// past it we fall through to the binary search.
const HINT_PROBE_COUNT: usize = 8;

/// Age beyond which the newest hint probe is considered too stale to trust.
const HINT_STALE_HOURS: i64 = 2;

/// Number of real-time volume directories in the S3 bucket.
///
/// Volumes are numbered 1..=999 (see `VolumeIndex::next()` wrapping 999→1).
const VOLUME_COUNT: usize = 999;

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

/// Finds the latest volume directory for the given site.
///
/// If `hint` is provided (from localStorage), tries a fast forward scan first.
/// Falls through to the rotated-array binary search on cache miss or stale hint.
pub async fn find_latest_volume(
    site: &str,
    hint: Option<VolumeIndex>,
) -> Result<VolumeSearchResult> {
    let mut total_requests = 0usize;

    if let Some(hint) = hint {
        let volumes: Vec<_> = (0..HINT_PROBE_COUNT).map(|i| advance(hint, i)).collect();
        let probes = probe_batch(site, &volumes).await;
        total_requests += volumes.len();

        let newest = probes
            .iter()
            .filter_map(|(v, t)| t.map(|t| (*v, t)))
            .max_by_key(|(_, t)| *t);

        if let Some((best_vol, best_time)) = newest {
            let age_hours = (Utc::now() - best_time).num_hours();
            let at_edge = best_vol == *volumes.last().unwrap();
            // Only trust the hint if the newest probe is recent and not at the
            // far edge of our probed window (which would suggest the real
            // current is further ahead than we probed).
            if age_hours < HINT_STALE_HOURS && !at_edge {
                log::info!(
                    "volume_discovery: hint {} → resolved to {} in {} requests",
                    hint.as_number(),
                    best_vol.as_number(),
                    total_requests
                );
                return Ok(VolumeSearchResult {
                    volume: Some(best_vol),
                    requests_made: total_requests,
                });
            }
            log::info!(
                "volume_discovery: hint {} stale or at edge (age={}h, at_edge={}), falling back to binary search",
                hint.as_number(),
                age_hours,
                at_edge
            );
        } else {
            log::info!(
                "volume_discovery: hint {} and neighbors all empty, falling back to binary search",
                hint.as_number()
            );
        }
    }

    // Cold start / stale hint — rotated-array binary search.
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
    log::info!(
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

    // The `search` function is a verbatim port of nexrad-data 1.0.0-rc.7
    // src/aws/realtime/search.rs, which has its own test suite covering
    // rotated arrays with None gaps. Not duplicated here.
}
