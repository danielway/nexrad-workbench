//! Fast discovery of the current volume directory in the real-time NEXRAD S3 bucket.
//!
//! The real-time bucket holds 999 round-robin volume directories. At any moment exactly
//! one is being written to; the rest hold older data from prior passes around the ring.
//! Finding the current directory is a prerequisite for streaming — `nexrad-data`'s
//! `get_latest_volume()` does a sequential binary search (~10 serial LIST requests).
//!
//! This module replaces that with two faster strategies:
//!
//! 1. **Cached-hint forward scan** (reconnection): Probe `hint..hint+K` concurrently.
//!    The current volume is whichever probed volume has the newest first-chunk
//!    timestamp. One round trip for the common case.
//!
//! 2. **Parallel coarse+fine search** (cold start / stale hint): Probe ~10 evenly
//!    spaced volumes concurrently, narrow to the region around the newest, probe
//!    ~10 more within that region. Two round trips instead of ten.

use chrono::{DateTime, Utc};
use futures_util::future::join_all;
use nexrad_data::aws::realtime::{list_chunks_in_volume, VolumeIndex};
use nexrad_data::result::Result;

/// Number of concurrent probes forward from the cached hint.
///
/// 8 probes covers roughly 40 minutes of volume progression (each volume ~5 min).
/// If the user reconnects within that window we resolve in one round trip; past
/// it we fall through to the cold-start search.
const HINT_PROBE_COUNT: usize = 8;

/// Number of concurrent probes in the cold-start coarse sweep.
const COARSE_PROBE_COUNT: usize = 10;

/// Half-width of the fine-sweep window (probes cover [argmax - WIDTH, argmax + WIDTH]).
const FINE_WINDOW_HALF: usize = 60;

/// Number of concurrent probes in the cold-start fine sweep.
const FINE_PROBE_COUNT: usize = 10;

/// Age beyond which a "newest" timestamp is considered too stale to be the current volume.
/// Used to detect when the cached hint has fallen behind.
const STALE_THRESHOLD_HOURS: i64 = 2;

/// Result of a volume search including the request count (for network stats).
#[derive(Debug, Clone)]
pub struct VolumeSearchResult {
    pub volume: Option<VolumeIndex>,
    pub requests_made: usize,
}

/// Advance a volume index by `offset`, wrapping 999 → 1 (matching `VolumeIndex::next()`).
fn advance(vol: VolumeIndex, offset: usize) -> VolumeIndex {
    let n = vol.as_number();
    // Volumes are 1..=999, cycle length 999.
    let wrapped = ((n - 1 + offset) % 999) + 1;
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

/// Pick the volume with the newest upload timestamp from a batch of probes.
fn argmax_newest(
    probes: &[(VolumeIndex, Option<DateTime<Utc>>)],
) -> Option<(VolumeIndex, DateTime<Utc>)> {
    probes
        .iter()
        .filter_map(|(v, t)| t.map(|t| (*v, t)))
        .max_by_key(|(_, t)| *t)
}

/// Finds the latest volume directory for the given site.
///
/// If `hint` is provided (from localStorage), tries a fast forward scan first.
/// Falls through to a parallel coarse+fine search on cache miss or stale hint.
pub async fn find_latest_volume(
    site: &str,
    hint: Option<VolumeIndex>,
) -> Result<VolumeSearchResult> {
    let mut total_requests = 0usize;

    if let Some(hint) = hint {
        let volumes: Vec<_> = (0..HINT_PROBE_COUNT).map(|i| advance(hint, i)).collect();
        let probes = probe_batch(site, &volumes).await;
        total_requests += volumes.len();

        if let Some((best_vol, best_time)) = argmax_newest(&probes) {
            let age_hours = (Utc::now() - best_time).num_hours();
            let at_edge = best_vol == *volumes.last().unwrap();
            // Trust the hint only if the newest probe is recent and not at the very edge
            // of our probed window (which would suggest the real current is further ahead).
            if age_hours < STALE_THRESHOLD_HOURS && !at_edge {
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
                "volume_discovery: hint {} stale or at edge (age={}h, at_edge={}), falling back to cold search",
                hint.as_number(),
                age_hours,
                at_edge
            );
        } else {
            log::info!(
                "volume_discovery: hint {} and neighbors all empty, falling back to cold search",
                hint.as_number()
            );
        }
    }

    // Cold start: coarse sweep across the full range.
    let step = 999 / COARSE_PROBE_COUNT;
    let coarse_volumes: Vec<_> = (0..COARSE_PROBE_COUNT)
        .map(|i| VolumeIndex::new(1 + i * step))
        .collect();
    let coarse_probes = probe_batch(site, &coarse_volumes).await;
    total_requests += coarse_volumes.len();

    let coarse_best = match argmax_newest(&coarse_probes) {
        Some(b) => b,
        None => {
            log::warn!(
                "volume_discovery: coarse sweep found no volumes with data for {}",
                site
            );
            return Ok(VolumeSearchResult {
                volume: None,
                requests_made: total_requests,
            });
        }
    };

    // Fine sweep around the coarse argmax.
    let center = coarse_best.0.as_number();
    let start = if center > FINE_WINDOW_HALF {
        center - FINE_WINDOW_HALF
    } else {
        // Wrap backward: (center - FINE_WINDOW_HALF) mod 999 in 1-based space
        999 - (FINE_WINDOW_HALF - center)
    };
    let fine_step = ((2 * FINE_WINDOW_HALF) / FINE_PROBE_COUNT).max(1);
    let fine_volumes: Vec<_> = (0..FINE_PROBE_COUNT)
        .map(|i| advance(VolumeIndex::new(start), i * fine_step))
        .collect();
    let fine_probes = probe_batch(site, &fine_volumes).await;
    total_requests += fine_volumes.len();

    // Combine coarse + fine and pick the argmax overall.
    let mut all_probes = coarse_probes;
    all_probes.extend(fine_probes);
    let final_best = argmax_newest(&all_probes);

    log::info!(
        "volume_discovery: cold search resolved to {:?} in {} requests",
        final_best.as_ref().map(|(v, _)| v.as_number()),
        total_requests
    );

    Ok(VolumeSearchResult {
        volume: final_best.map(|(v, _)| v),
        requests_made: total_requests,
    })
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
}
