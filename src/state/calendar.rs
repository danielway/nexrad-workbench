//! Coverage aggregation for the Archive zoom tier (spec §6.4).
//!
//! The Archive tier replaces the deprecated year-wide *linear* strip zoom with a
//! calendar-style coverage heatmap (GitHub-contributions grammar): a run of
//! cells, each toned by **how much data exists in that period** (server
//! availability) and **how much is cached locally** (downloaded). Tapping a cell
//! frames it, one rung finer.
//!
//! **Cells coarsen with the span** — day → week → month → quarter
//! ([`BucketGranularity`]) — which is what lets one continuous timeline reach
//! from a single volume out to the whole NEXRAD era without the lane becoming a
//! shimmer of sub-pixel ticks. Boundaries are calendar-aligned (real months, not
//! 30-day blocks) so labels never drift off the periods they name, and buckets
//! are therefore **variable width**: every fraction is taken against the
//! bucket's own span, and the renderer maps both edges through the shared
//! `ts_to_x`.
//!
//! This module is the *pure data* behind the heatmap. It derives buckets from
//! the in-memory timeline state the strip already holds — the cached scans
//! ([`RadarTimeline`]) for the CACHE tone and the archive shadow boundaries
//! ([`ScanBoundary`], the listing's coverage) for the AVAILABILITY tone. It adds
//! **no IDB round-trips and no async**; periods outside the listed window
//! honestly read as empty rather than fabricating coverage (the listing only
//! spans ranges the archive index has fetched — that's acceptable, the calendar
//! shows what is genuinely known).
//!
//! **Bucketing is UTC-aligned**, matching the millisecond storage-key
//! convention; it is deterministic and timezone-free, so the aggregation is
//! unit-testable without a clock.

use crate::core::BucketGranularity;
use crate::core::RadarTimeline;
use crate::core::ScanBoundary;
use crate::state::SavedEvents;

/// Seconds in a UTC day. Day buckets are `[day_start, day_start + DAY_SECS)`.
pub(crate) const DAY_SECS: f64 = 86_400.0;

/// Hard cap on how many cells the aggregator emits, so an absurd span (e.g. an
/// old URL with a microscopic zoom) can't allocate a runaway vector.
///
/// With the granularity ladder this is a pure safety net rather than the
/// operative limit: the rung is chosen so cells stay ~5-8px, which bounds the
/// count at `width / 5` on its own.
pub(crate) const MAX_BUCKETS: usize = 800;

/// One bucket's coverage in the Archive heatmap.
///
/// Buckets are variable width — a month is 28-31 days, and the renderer maps
/// both edges through the shared `ts_to_x`, so nothing downstream needs to
/// assume a fixed size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct TimeBucket {
    /// Bucket start (Unix seconds) — the cell's identity and the drill-down
    /// target's origin.
    pub start: f64,
    /// Bucket end (Unix seconds), exclusive.
    pub end: f64,
    /// Fraction `0..1` of the bucket for which data is *known to exist* on the
    /// server (the union of archive-listing spans and cached-scan spans clamped
    /// to the bucket). 0 ⇒ nothing touches this bucket (unknown/empty).
    pub availability_frac: f32,
    /// Fraction `0..1` of the bucket covered by *downloaded* (cached) scans.
    /// Always ≤ `availability_frac` (cached data is also available data).
    pub cache_frac: f32,
    /// Absolute covered seconds, kept alongside the fractions because at coarse
    /// rungs a real amount of data rounds to "0%" — a tooltip saying
    /// "6.2 h cached of 168 h" is honest where "0%" is not.
    pub available_secs: f64,
    /// Absolute cached seconds (see [`Self::available_secs`]).
    pub cached_secs: f64,
    /// A saved event for the current site starts or overlaps this bucket.
    pub has_events: bool,
}

impl TimeBucket {
    /// An empty (no data known) bucket spanning `[start, end)`.
    fn empty(start: f64, end: f64) -> Self {
        Self {
            start,
            end,
            availability_frac: 0.0,
            cache_frac: 0.0,
            available_secs: 0.0,
            cached_secs: 0.0,
            has_events: false,
        }
    }

    /// Bucket width in seconds (always > 0 by construction).
    pub(crate) fn span(&self) -> f64 {
        (self.end - self.start).max(1.0)
    }
}

/// The UTC day start (Unix seconds) containing `ts`.
pub(crate) fn utc_day_start(ts: f64) -> f64 {
    (ts / DAY_SECS).floor() * DAY_SECS
}

/// The UTC Monday 00:00 (Unix seconds) of the week containing `ts`.
///
/// Integer day arithmetic, no chrono and no clock: epoch day 0 is a Thursday,
/// so `(days + 3) mod 7` is the offset back to Monday.
pub(crate) fn utc_week_start(ts: f64) -> f64 {
    let days = (ts / DAY_SECS).floor();
    let monday_days = days - (days + 3.0).rem_euclid(7.0);
    monday_days * DAY_SECS
}

/// The start of the real UTC calendar month containing `ts`.
pub(crate) fn utc_month_start(ts: f64) -> f64 {
    month_start_from_ymd(ts, 1)
}

/// The start of the calendar quarter (Jan/Apr/Jul/Oct) containing `ts`.
pub(crate) fn utc_quarter_start(ts: f64) -> f64 {
    month_start_from_ymd(ts, 3)
}

/// Start of the `step`-month-aligned block containing `ts`.
///
/// Real calendar months, not fixed 30-day blocks: a fixed width drifts off the
/// months it claims to name, so the labels would stop matching the cells within
/// a couple of years.
fn month_start_from_ymd(ts: f64, step: u32) -> f64 {
    use chrono::{Datelike, NaiveDate};
    let Some(dt) = chrono::DateTime::from_timestamp(ts.floor() as i64, 0) else {
        return utc_day_start(ts);
    };
    let d = dt.date_naive();
    let month0 = (d.month() - 1) / step * step;
    NaiveDate::from_ymd_opt(d.year(), month0 + 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc().timestamp() as f64)
        .unwrap_or_else(|| utc_day_start(ts))
}

/// Start of the bucket containing `ts` at `granularity`.
pub(crate) fn bucket_start(ts: f64, granularity: BucketGranularity) -> f64 {
    match granularity {
        BucketGranularity::Day => utc_day_start(ts),
        BucketGranularity::Week => utc_week_start(ts),
        BucketGranularity::Month => utc_month_start(ts),
        BucketGranularity::Quarter => utc_quarter_start(ts),
    }
}

/// Start of the bucket immediately after the one beginning at `start`.
///
/// Stepping by "one nominal span then re-align" rather than adding a fixed
/// number of seconds is what keeps months correct across their 28/29/30/31-day
/// variation and across leap years.
pub(crate) fn next_bucket_start(start: f64, granularity: BucketGranularity) -> f64 {
    match granularity {
        BucketGranularity::Day => start + DAY_SECS,
        BucketGranularity::Week => start + 7.0 * DAY_SECS,
        // Step into the middle of the following month, then re-align — safe for
        // any month length without per-month arithmetic.
        BucketGranularity::Month => utc_month_start(start + 31.0 * DAY_SECS + DAY_SECS),
        BucketGranularity::Quarter => utc_quarter_start(start + 93.0 * DAY_SECS + DAY_SECS),
    }
}

/// The bucket boundaries covering `[view_start, view_end]` at `granularity`.
///
/// Returns `n + 1` ascending edges for `n` buckets, capped at [`MAX_BUCKETS`].
pub(crate) fn bucket_boundaries(
    view_start: f64,
    view_end: f64,
    granularity: BucketGranularity,
) -> Vec<f64> {
    let lo = view_start.min(view_end);
    let hi = view_start.max(view_end);
    let mut edges = vec![bucket_start(lo, granularity)];
    while *edges.last().unwrap() <= hi && edges.len() <= MAX_BUCKETS {
        let next = next_bucket_start(*edges.last().unwrap(), granularity);
        // Defensive: a non-advancing step would spin forever.
        if next <= *edges.last().unwrap() {
            break;
        }
        edges.push(next);
    }
    if edges.len() < 2 {
        let start = edges[0];
        edges.push(next_bucket_start(start, granularity));
    }
    edges
}

/// Day-granularity convenience wrapper over [`aggregate_buckets`].
///
/// Production code always passes the zoom-derived granularity; this keeps the
/// day-specific tests reading as they did before the ladder existed.
///
/// Aggregate the timeline sources into per-day buckets spanning the visible
/// Archive window `[view_start, view_end]` (Unix seconds).
///
/// One bucket per UTC day from `view_start`'s day through `view_end`'s day
/// (inclusive), capped at [`MAX_BUCKETS`]. Days with no listing and no cache
/// read empty — the calendar does not invent availability.
///
/// - **availability**: the union of archive shadow-boundary spans and cached
///   scan display spans, intersected with each day. Shadow boundaries are the
///   listing's coverage; cached scans count as available too (we have them, so
///   they exist).
/// - **cache**: cached scan spans only, intersected with each day.
///
/// Spans are unioned per day (overlaps don't double-count) so a fraction never
/// exceeds 1.0.
#[cfg(test)]
pub(crate) fn aggregate_day_buckets(
    cache: &RadarTimeline,
    shadows: &[ScanBoundary],
    saved_events: &SavedEvents,
    current_site: &str,
    view_start: f64,
    view_end: f64,
) -> Vec<TimeBucket> {
    aggregate_buckets(
        cache,
        shadows,
        saved_events,
        current_site,
        view_start,
        view_end,
        BucketGranularity::Day,
    )
}

/// Aggregate the timeline sources into coverage buckets at `granularity`.
///
/// The generalization of [`aggregate_day_buckets`]: identical semantics, but
/// the cell size follows the zoom ladder so the same lane stays legible from a
/// few days out to the whole NEXRAD era. Buckets are calendar-aligned and
/// therefore variable width, so every fraction is taken against that bucket's
/// own span rather than a fixed day.
pub(crate) fn aggregate_buckets(
    cache: &RadarTimeline,
    shadows: &[ScanBoundary],
    saved_events: &SavedEvents,
    current_site: &str,
    view_start: f64,
    view_end: f64,
    granularity: BucketGranularity,
) -> Vec<TimeBucket> {
    let edges = bucket_boundaries(view_start, view_end, granularity);
    let bucket_count = edges.len() - 1;
    let first_edge = edges[0];
    let last_edge = edges[bucket_count];

    // Availability and cache coverage accumulators, in seconds-per-bucket. Kept
    // as span lists then unioned, so overlapping spans don't over-count.
    let mut avail_spans: Vec<Vec<(f64, f64)>> = vec![Vec::new(); bucket_count];
    let mut cache_spans: Vec<Vec<(f64, f64)>> = vec![Vec::new(); bucket_count];

    // Binary search rather than a fixed-width division: bucket edges are no
    // longer evenly spaced once months are in play.
    let bucket_index = |ts: f64| -> Option<usize> {
        if ts < first_edge || ts >= last_edge {
            return None;
        }
        let idx = edges.partition_point(|&e| e <= ts).saturating_sub(1);
        (idx < bucket_count).then_some(idx)
    };

    // Push a [start, end] span into the accumulators, splitting it at bucket
    // boundaries so each bucket only sees its own slice.
    let push_span = |spans: &mut [Vec<(f64, f64)>], start: f64, end: f64| {
        if end <= start {
            return;
        }
        let mut cursor = start.max(first_edge);
        let stop = end.min(last_edge);
        while cursor < stop {
            let Some(idx) = bucket_index(cursor) else {
                break;
            };
            let bucket_end = edges[idx + 1];
            let slice_end = stop.min(bucket_end);
            spans[idx].push((cursor, slice_end));
            cursor = bucket_end;
        }
    };

    // Cached scans → both cache AND availability (we have them, so they exist).
    for scan in &cache.scans {
        let s = scan.start_time;
        let e = scan.display_end_time().max(scan.end_time);
        push_span(&mut cache_spans, s, e);
        push_span(&mut avail_spans, s, e);
    }

    // Archive shadow boundaries (listing coverage) → availability only.
    //
    // Capped for plausibility. `ScanBoundary.end` is derived as the *next
    // file's timestamp*, unconditionally, so the boundary immediately before a
    // radar outage spans the whole outage — and pushed raw it would credit the
    // day with hours of availability that never existed. The heatmap's whole
    // job is showing where data is, so an inflated wash is a direct lie.
    for b in shadows {
        let start = b.start as f64;
        let extent = crate::core::block_extent(
            start,
            start,
            None,
            b.end as f64,
            crate::FALLBACK_SCAN_DURATION_SECS as f64,
        );
        push_span(&mut avail_spans, start, extent.expected_end);
    }

    let mut buckets: Vec<TimeBucket> = (0..bucket_count)
        .map(|i| {
            let mut b = TimeBucket::empty(edges[i], edges[i + 1]);
            let span = b.span();
            b.available_secs = union_seconds(&mut avail_spans[i]);
            b.cached_secs = union_seconds(&mut cache_spans[i]);
            b.availability_frac = (b.available_secs / span).clamp(0.0, 1.0) as f32;
            // Cache can't exceed availability (it is a subset), but float slop
            // could nudge it past; clamp to keep the visual invariant.
            b.cache_frac = (b.cached_secs / span).clamp(0.0, b.availability_frac as f64) as f32;
            b
        })
        .collect();

    // Saved-event flag: an event for the current site that overlaps the bucket.
    for event in &saved_events.events {
        if event.site_id != current_site {
            continue;
        }
        let (es, ee) = (
            event.start_time.min(event.end_time),
            event.start_time.max(event.end_time),
        );
        let mut cursor = es.max(first_edge);
        while cursor <= ee && cursor < last_edge {
            let Some(idx) = bucket_index(cursor) else {
                break;
            };
            buckets[idx].has_events = true;
            cursor = edges[idx + 1];
        }
    }

    buckets
}

/// Total covered seconds of a set of `(start, end)` spans, unioning overlaps.
/// Mutates the slice (sorts it) — callers pass throwaway per-day vectors.
fn union_seconds(spans: &mut [(f64, f64)]) -> f64 {
    if spans.is_empty() {
        return 0.0;
    }
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut total = 0.0;
    let (mut cur_s, mut cur_e) = spans[0];
    for &(s, e) in &spans[1..] {
        if s > cur_e {
            total += cur_e - cur_s;
            cur_s = s;
            cur_e = e;
        } else if e > cur_e {
            cur_e = e;
        }
    }
    total + (cur_e - cur_s)
}

/// Tap-a-day → Macro zoom target (spec §6.4 "tapping a day zooms into Macro
/// there"). Given the day to land on and the current strip width, returns the
/// `(view_start, zoom)` that centres `day_start`'s day in a Macro-tier view.
///
/// The zoom is chosen so the visible span sits comfortably inside the Macro
/// band (below the Archive-entry span, above the Micro boundary): we show a
/// window of [`MACRO_LANDING_SPAN_SECS`] centred on the day's midpoint. Routing
/// the returned zoom through [`crate::core::PlaybackState::set_timeline_zoom`]
/// lands the tier machine in Macro.
pub(crate) fn day_tap_macro_view(day_start: f64, width_px: f64) -> (f64, f64) {
    let span = MACRO_LANDING_SPAN_SECS;
    let zoom = (width_px / span).max(f64::MIN_POSITIVE);
    let day_mid = day_start + DAY_SECS / 2.0;
    let view_start = day_mid - span / 2.0;
    (view_start, zoom)
}

/// Tap-a-bucket → the `(view_start, zoom)` to drill into.
///
/// **One rung per tap**, not straight to Macro. A quarter cell holds ~90 days;
/// jumping from it to a single day is a 90x leap that throws away every scrap
/// of context the user was navigating by. Quarter → Month → Week → Day → Macro
/// keeps the tapped cell filling the strip at each step, so the gesture reads
/// as "look closer" rather than "teleport".
///
/// The returned zoom is deliberately routed through
/// [`crate::core::PlaybackState::set_timeline_zoom`] by the caller, so the tier
/// and granularity state machines both advance from it.
pub(crate) fn bucket_tap_target(
    bucket: &TimeBucket,
    granularity: BucketGranularity,
    width_px: f64,
) -> (f64, f64) {
    // The finest rung hands off to the linear Macro tier.
    if granularity == BucketGranularity::Day {
        return day_tap_macro_view(bucket.start, width_px);
    }
    // Otherwise show exactly this bucket, which puts the next rung's cells at a
    // comfortable size by construction.
    let span = (bucket.end - bucket.start).max(DAY_SECS);
    let zoom = (width_px / span).max(f64::MIN_POSITIVE);
    (bucket.start, zoom)
}

/// Visible span (seconds) a day-tap lands on in Macro. Half a day on either
/// side of the tapped day's midpoint (≈1 day visible) sits safely below the
/// Archive-enter span (60 h) and well above the Micro boundary, so the tier
/// machine settles in Macro and the tapped day fills the strip.
pub(crate) const MACRO_LANDING_SPAN_SECS: f64 = DAY_SECS;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Scan, Sweep};
    use crate::state::SavedEvent;
    use wasm_bindgen_test::wasm_bindgen_test;

    // A fixed UTC day start for tests: 2026-06-01 00:00:00 UTC = 1_780_272_000.
    // (Any multiple of DAY_SECS works; the value is just a readable anchor.)
    const DAY0: f64 = 1_780_272_000.0;

    fn scan_span(start: f64, end: f64, cached: bool) -> Scan {
        let products: Vec<String> = if cached {
            vec!["reflectivity".to_string()]
        } else {
            Vec::new()
        };
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: start,
            vcp: 215,
            vcp_pattern: None,
            sweeps: vec![Sweep {
                start_time: start,
                end_time: end,
                elevation: 0.5,
                elevation_number: 1,
                start_azimuth: 0.0,
                radials: Vec::new(),
                cached_products: products,
            }],
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn no_events() -> SavedEvents {
        SavedEvents::default()
    }

    #[wasm_bindgen_test]
    fn utc_day_start_floors_to_midnight() {
        assert_eq!(utc_day_start(DAY0), DAY0);
        assert_eq!(utc_day_start(DAY0 + 3600.0), DAY0);
        assert_eq!(utc_day_start(DAY0 + DAY_SECS - 1.0), DAY0);
        assert_eq!(utc_day_start(DAY0 + DAY_SECS), DAY0 + DAY_SECS);
    }

    #[wasm_bindgen_test]
    fn empty_day_has_no_coverage() {
        // A cached scan only on DAY0; query DAY1 which has nothing.
        let cache = RadarTimeline {
            scans: vec![scan_span(DAY0 + 1000.0, DAY0 + 1300.0, true)],
        };
        let buckets = aggregate_day_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            DAY0 + DAY_SECS,
            DAY0 + DAY_SECS + 7200.0,
        );
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].start, DAY0 + DAY_SECS);
        assert_eq!(buckets[0].availability_frac, 0.0);
        assert_eq!(buckets[0].cache_frac, 0.0);
        assert!(!buckets[0].has_events);
    }

    #[wasm_bindgen_test]
    fn cached_day_has_cache_and_availability() {
        // One cached scan covering 3600s of DAY0.
        let cache = RadarTimeline {
            scans: vec![scan_span(DAY0 + 1000.0, DAY0 + 4600.0, true)],
        };
        let buckets = aggregate_day_buckets(&cache, &[], &no_events(), "KDMX", DAY0, DAY0 + 7200.0);
        assert_eq!(buckets.len(), 1);
        let b = buckets[0];
        // 3600 / 86400 ≈ 0.0417 for BOTH cache and availability.
        let expected = (3600.0 / DAY_SECS) as f32;
        assert!((b.cache_frac - expected).abs() < 1e-4, "{}", b.cache_frac);
        assert!(
            (b.availability_frac - expected).abs() < 1e-4,
            "{}",
            b.availability_frac
        );
    }

    #[wasm_bindgen_test]
    fn availability_only_day_has_zero_cache() {
        // Shadow boundary (listing coverage), no cached scan → availability>0,
        // cache==0.
        let cache = RadarTimeline { scans: Vec::new() };
        let shadows = vec![ScanBoundary {
            start: (DAY0 + 1000.0) as i64,
            end: (DAY0 + 4600.0) as i64,
        }];
        let buckets =
            aggregate_day_buckets(&cache, &shadows, &no_events(), "KDMX", DAY0, DAY0 + 7200.0);
        let b = buckets[0];
        assert!(b.availability_frac > 0.0);
        assert_eq!(b.cache_frac, 0.0);
    }

    #[wasm_bindgen_test]
    fn overlapping_spans_do_not_double_count() {
        // Two overlapping cached scans on DAY0: [1000,4600] and [3000,6000].
        // Union = [1000, 6000] = 5000s, NOT 3600+3000=6600s.
        let cache = RadarTimeline {
            scans: vec![
                scan_span(DAY0 + 1000.0, DAY0 + 4600.0, true),
                scan_span(DAY0 + 3000.0, DAY0 + 6000.0, true),
            ],
        };
        let buckets = aggregate_day_buckets(&cache, &[], &no_events(), "KDMX", DAY0, DAY0 + 7200.0);
        let expected = (5000.0 / DAY_SECS) as f32;
        assert!(
            (buckets[0].cache_frac - expected).abs() < 1e-4,
            "{}",
            buckets[0].cache_frac
        );
    }

    #[wasm_bindgen_test]
    fn span_crossing_midnight_splits_across_days() {
        // A scan from DAY0 23:00 running 2h into DAY1 01:00.
        let cache = RadarTimeline {
            scans: vec![scan_span(
                DAY0 + DAY_SECS - 3600.0,
                DAY0 + DAY_SECS + 3600.0,
                true,
            )],
        };
        let buckets = aggregate_day_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            DAY0,
            DAY0 + DAY_SECS + 100.0,
        );
        assert_eq!(buckets.len(), 2);
        // Each day gets 3600s.
        let expected = (3600.0 / DAY_SECS) as f32;
        assert!((buckets[0].cache_frac - expected).abs() < 1e-4);
        assert!((buckets[1].cache_frac - expected).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn event_day_is_flagged_for_matching_site_only() {
        let cache = RadarTimeline { scans: Vec::new() };
        let mut events = SavedEvents::default();
        events.events.push(SavedEvent {
            id: 1,
            name: "Tornado".to_string(),
            site_id: "KDMX".to_string(),
            start_time: DAY0 + 5000.0,
            end_time: DAY0 + 9000.0,
        });
        events.events.push(SavedEvent {
            id: 2,
            name: "Other site".to_string(),
            site_id: "KTLX".to_string(),
            start_time: DAY0 + 5000.0,
            end_time: DAY0 + 9000.0,
        });
        let buckets = aggregate_day_buckets(&cache, &[], &events, "KDMX", DAY0, DAY0 + 7200.0);
        assert!(buckets[0].has_events); // KDMX event flags the day
                                        // The KTLX event must not flag it for KDMX.
        let buckets_ktlx = aggregate_day_buckets(&cache, &[], &events, "KZZZ", DAY0, DAY0 + 7200.0);
        assert!(!buckets_ktlx[0].has_events);
    }

    #[wasm_bindgen_test]
    fn bucket_count_spans_first_through_last_day_inclusive() {
        let cache = RadarTimeline { scans: Vec::new() };
        // view from DAY0 to DAY0+3 days → 4 day cells inclusive.
        let buckets = aggregate_day_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            DAY0 + 100.0,
            DAY0 + 3.0 * DAY_SECS + 100.0,
        );
        assert_eq!(buckets.len(), 4);
        assert_eq!(buckets[0].start, DAY0);
        assert_eq!(buckets[3].start, DAY0 + 3.0 * DAY_SECS);
    }

    #[wasm_bindgen_test]
    fn absurd_span_is_capped() {
        let cache = RadarTimeline { scans: Vec::new() };
        // A 10000-year span would be ~3.6M days; capped at MAX_BUCKETS.
        let buckets = aggregate_day_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            DAY0,
            DAY0 + 10_000.0 * 365.0 * DAY_SECS,
        );
        assert_eq!(buckets.len(), MAX_BUCKETS);
    }

    #[wasm_bindgen_test]
    fn day_tap_centres_day_in_macro_view() {
        let width = 1000.0;
        let (view_start, zoom) = day_tap_macro_view(DAY0, width);
        // The visible span equals the macro landing span.
        let span = width / zoom;
        assert!((span - MACRO_LANDING_SPAN_SECS).abs() < 1e-3);
        // The day's midpoint sits at the centre of the view.
        let day_mid = DAY0 + DAY_SECS / 2.0;
        let view_mid = view_start + span / 2.0;
        assert!((view_mid - day_mid).abs() < 1e-3);
    }

    /// The tap-target zoom must seed the Macro tier (below Archive-enter span,
    /// at/above Micro boundary handling) — the gesture's whole point.
    #[wasm_bindgen_test]
    fn day_tap_zoom_lands_in_macro_tier() {
        use crate::core::TimelineTier;
        let width = 1000.0;
        let (_view_start, zoom) = day_tap_macro_view(DAY0, width);
        let tier = crate::core::PlaybackState::seed_tier(zoom, width);
        assert_eq!(tier, TimelineTier::Macro);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::{Scan, Sweep};
    use crate::state::SavedEvent;
    use wasm_bindgen_test::wasm_bindgen_test;

    // A fixed UTC day start: 2026-06-01 00:00:00 UTC = 1_780_272_000 (multiple
    // of DAY_SECS). Re-declared here since the sibling `mod tests` helpers are
    // private to that module.
    const DAY0: f64 = 1_780_272_000.0;

    fn scan_span(start: f64, end: f64, cached: bool) -> Scan {
        let products: Vec<String> = if cached {
            vec!["reflectivity".to_string()]
        } else {
            Vec::new()
        };
        Scan {
            start_time: start,
            end_time: end,
            key_timestamp: start,
            vcp: 215,
            vcp_pattern: None,
            sweeps: vec![Sweep {
                start_time: start,
                end_time: end,
                elevation: 0.5,
                elevation_number: 1,
                start_azimuth: 0.0,
                radials: Vec::new(),
                cached_products: products,
            }],
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn no_events() -> SavedEvents {
        SavedEvents::default()
    }

    // --- utc_day_start edge / negative / exact-boundary cases ---

    #[wasm_bindgen_test]
    fn utc_day_start_at_zero_is_zero() {
        // The epoch itself is a day boundary.
        assert_eq!(utc_day_start(0.0), 0.0);
        // Just inside the first day still floors to 0.
        assert_eq!(utc_day_start(1.0), 0.0);
        assert_eq!(utc_day_start(DAY_SECS - 1.0), 0.0);
    }

    #[wasm_bindgen_test]
    fn utc_day_start_negative_floors_toward_minus_infinity() {
        // floor(-0.5) = -1, so a tiny negative ts belongs to the day [-DAY_SECS, 0).
        assert_eq!(utc_day_start(-1.0), -DAY_SECS);
        // Exact negative boundary maps to itself.
        assert_eq!(utc_day_start(-DAY_SECS), -DAY_SECS);
        // Mid negative day.
        assert_eq!(utc_day_start(-DAY_SECS + 100.0), -DAY_SECS);
    }

    // --- aggregate_day_buckets: argument-order independence ---

    #[wasm_bindgen_test]
    fn reversed_view_bounds_yield_same_buckets() {
        // The aggregator uses min/max on the bounds, so swapping start/end must
        // produce identical buckets.
        let cache = RadarTimeline {
            scans: vec![scan_span(DAY0 + 1000.0, DAY0 + 4600.0, true)],
        };
        let forward = aggregate_day_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            DAY0,
            DAY0 + 2.0 * DAY_SECS,
        );
        let reversed = aggregate_day_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            DAY0 + 2.0 * DAY_SECS,
            DAY0,
        );
        assert_eq!(forward.len(), reversed.len());
        assert_eq!(forward, reversed);
    }

    // --- availability clamps to 1.0 for an over-full day ---

    #[wasm_bindgen_test]
    fn a_shadow_spanning_an_outage_does_not_inflate_availability() {
        // ScanBoundary.end is the NEXT file's timestamp, unconditionally — so
        // the last boundary before a radar outage spans the entire outage.
        // Pushed raw, one such boundary credited the day with 6 hours of
        // availability that never existed, which is a direct lie in a heatmap
        // whose only job is showing where data is.
        let cache = RadarTimeline { scans: Vec::new() };
        let shadows = vec![ScanBoundary {
            start: (DAY0 + 3600.0) as i64,
            end: (DAY0 + 3600.0 + 6.0 * 3600.0) as i64,
        }];
        let buckets =
            aggregate_day_buckets(&cache, &shadows, &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        // One volume's worth of credit, not six hours.
        let six_hours_frac = (6.0 * 3600.0 / DAY_SECS) as f32;
        assert!(
            buckets[0].availability_frac < six_hours_frac / 10.0,
            "{}",
            buckets[0].availability_frac
        );
        assert!(buckets[0].availability_frac > 0.0);
    }

    #[wasm_bindgen_test]
    fn day_fully_covered_clamps_availability_to_one() {
        // Continuous listing coverage across the whole day (and past both
        // edges), so availability_frac == 1.0 and cache_frac == 0.
        //
        // Built from back-to-back 300s boundaries because that is what a real
        // listing produces — one per file. A single multi-day boundary would be
        // capped as an outage now (see `block_extent`), which is the point.
        let cache = RadarTimeline { scans: Vec::new() };
        let step = 300.0;
        let shadows: Vec<ScanBoundary> = (0..(3.0 * DAY_SECS / step) as i64)
            .map(|i| {
                let s = DAY0 - DAY_SECS + i as f64 * step;
                ScanBoundary {
                    start: s as i64,
                    end: (s + step) as i64,
                }
            })
            .collect();
        let buckets =
            aggregate_day_buckets(&cache, &shadows, &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        assert_eq!(buckets.len(), 1);
        assert!(
            (buckets[0].availability_frac - 1.0).abs() < 1e-6,
            "{}",
            buckets[0].availability_frac
        );
        assert_eq!(buckets[0].cache_frac, 0.0);
    }

    // --- cache_frac <= availability_frac invariant across both sources ---

    #[wasm_bindgen_test]
    fn cache_frac_never_exceeds_availability_frac() {
        // Cache covers [1000,7600]=6600s; a small disjoint shadow adds more
        // availability. Cache stays a subset of availability.
        let cache = RadarTimeline {
            scans: vec![scan_span(DAY0 + 1000.0, DAY0 + 7600.0, true)],
        };
        // 1000s of disjoint listing coverage, as consecutive volume-sized
        // boundaries (a single 1000s boundary would now be capped at 450s).
        let shadows: Vec<ScanBoundary> = (0..4)
            .map(|i| {
                let s = DAY0 + 10000.0 + i as f64 * 250.0;
                ScanBoundary {
                    start: s as i64,
                    end: (s + 250.0) as i64,
                }
            })
            .collect();
        let buckets =
            aggregate_day_buckets(&cache, &shadows, &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        let b = buckets[0];
        // Cache = 6600s, availability = 6600 + 1000 = 7600s.
        let expected_cache = (6600.0 / DAY_SECS) as f32;
        let expected_avail = (7600.0 / DAY_SECS) as f32;
        assert!(
            (b.cache_frac - expected_cache).abs() < 1e-4,
            "{}",
            b.cache_frac
        );
        assert!(
            (b.availability_frac - expected_avail).abs() < 1e-4,
            "{}",
            b.availability_frac
        );
        assert!(b.cache_frac <= b.availability_frac);
    }

    // --- zero-length / inverted span contributes nothing ---

    #[wasm_bindgen_test]
    fn zero_length_scan_span_contributes_no_coverage() {
        // A scan with end == start (and no VCP projection) is ignored.
        let cache = RadarTimeline {
            scans: vec![scan_span(DAY0 + 2000.0, DAY0 + 2000.0, true)],
        };
        let buckets = aggregate_day_buckets(&cache, &[], &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        assert_eq!(buckets[0].cache_frac, 0.0);
        assert_eq!(buckets[0].availability_frac, 0.0);
    }

    #[wasm_bindgen_test]
    fn inverted_shadow_span_contributes_no_coverage() {
        // end < start → push_span early-returns, no availability.
        let cache = RadarTimeline { scans: Vec::new() };
        let shadows = vec![ScanBoundary {
            start: (DAY0 + 5000.0) as i64,
            end: (DAY0 + 1000.0) as i64,
        }];
        let buckets =
            aggregate_day_buckets(&cache, &shadows, &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        assert_eq!(buckets[0].availability_frac, 0.0);
    }

    // --- union across the two sources: cache + overlapping shadow don't double count ---

    #[wasm_bindgen_test]
    fn cache_and_overlapping_shadow_union_in_availability() {
        // Cache [1000,4600] and shadow coverage [3000,6000] overlap;
        // availability union is [1000,6000]=5000s (NOT 3600+3000). The shadow
        // side is consecutive volume-sized boundaries, since one 3000s boundary
        // would now be capped as an outage.
        let cache = RadarTimeline {
            scans: vec![scan_span(DAY0 + 1000.0, DAY0 + 4600.0, true)],
        };
        let shadows: Vec<ScanBoundary> = (0..10)
            .map(|i| {
                let s = DAY0 + 3000.0 + i as f64 * 300.0;
                ScanBoundary {
                    start: s as i64,
                    end: (s + 300.0) as i64,
                }
            })
            .collect();
        let buckets =
            aggregate_day_buckets(&cache, &shadows, &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        let expected_avail = (5000.0 / DAY_SECS) as f32;
        assert!(
            (buckets[0].availability_frac - expected_avail).abs() < 1e-4,
            "{}",
            buckets[0].availability_frac
        );
    }

    // --- adjacent (touching) spans merge in union_seconds (via aggregate) ---

    #[wasm_bindgen_test]
    fn adjacent_touching_spans_merge_without_gap() {
        // Two cached scans that touch end-to-start: [1000,4000] and [4000,7000].
        // Union is [1000,7000]=6000s, contiguous (s == cur_e is not a gap).
        let cache = RadarTimeline {
            scans: vec![
                scan_span(DAY0 + 1000.0, DAY0 + 4000.0, true),
                scan_span(DAY0 + 4000.0, DAY0 + 7000.0, true),
            ],
        };
        let buckets = aggregate_day_buckets(&cache, &[], &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        let expected = (6000.0 / DAY_SECS) as f32;
        assert!(
            (buckets[0].cache_frac - expected).abs() < 1e-4,
            "{}",
            buckets[0].cache_frac
        );
    }

    // --- multi-day event flagging ---

    #[wasm_bindgen_test]
    fn event_spanning_multiple_days_flags_each_day() {
        // Event from DAY0+12h through DAY0+2days+12h covers days 0, 1, 2.
        let cache = RadarTimeline { scans: Vec::new() };
        let mut events = SavedEvents::default();
        events.events.push(SavedEvent {
            id: 7,
            name: "Multi-day".to_string(),
            site_id: "KDMX".to_string(),
            start_time: DAY0 + DAY_SECS / 2.0,
            end_time: DAY0 + 2.0 * DAY_SECS + DAY_SECS / 2.0,
        });
        let buckets =
            aggregate_day_buckets(&cache, &[], &events, "KDMX", DAY0, DAY0 + 3.0 * DAY_SECS);
        assert_eq!(buckets.len(), 4);
        assert!(buckets[0].has_events);
        assert!(buckets[1].has_events);
        assert!(buckets[2].has_events);
        // Day 3 is past the event end → not flagged.
        assert!(!buckets[3].has_events);
    }

    #[wasm_bindgen_test]
    fn event_with_reversed_times_still_flags() {
        // start_time > end_time → the aggregator min/max-normalises them.
        let cache = RadarTimeline { scans: Vec::new() };
        let mut events = SavedEvents::default();
        events.events.push(SavedEvent {
            id: 9,
            name: "Reversed".to_string(),
            site_id: "KDMX".to_string(),
            start_time: DAY0 + 9000.0,
            end_time: DAY0 + 5000.0,
        });
        let buckets = aggregate_day_buckets(&cache, &[], &events, "KDMX", DAY0, DAY0 + 100.0);
        assert!(buckets[0].has_events);
    }

    #[wasm_bindgen_test]
    fn event_outside_view_window_does_not_flag() {
        // Event entirely on a later day, view only covers DAY0.
        let cache = RadarTimeline { scans: Vec::new() };
        let mut events = SavedEvents::default();
        events.events.push(SavedEvent {
            id: 11,
            name: "Far".to_string(),
            site_id: "KDMX".to_string(),
            start_time: DAY0 + 5.0 * DAY_SECS,
            end_time: DAY0 + 5.0 * DAY_SECS + 3600.0,
        });
        let buckets = aggregate_day_buckets(&cache, &[], &events, "KDMX", DAY0, DAY0 + 100.0);
        assert_eq!(buckets.len(), 1);
        assert!(!buckets[0].has_events);
    }

    // --- TimeBucket equality / Copy semantics on an empty day ---

    #[wasm_bindgen_test]
    fn empty_day_buckets_compare_equal() {
        let cache = RadarTimeline { scans: Vec::new() };
        let a = aggregate_day_buckets(&cache, &[], &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        let b = aggregate_day_buckets(&cache, &[], &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        assert_eq!(a[0], b[0]);
        // Copy: the bucket can be duplicated and remains equal.
        let copied = a[0];
        assert_eq!(copied, a[0]);
        assert_eq!(copied.start, DAY0);
    }

    // --- day_tap_macro_view: zoom scales with width; view_start width-independent ---

    #[wasm_bindgen_test]
    fn day_tap_zoom_scales_linearly_with_width() {
        let (vs1, z1) = day_tap_macro_view(DAY0, 1000.0);
        let (vs2, z2) = day_tap_macro_view(DAY0, 2000.0);
        // zoom = width / span, so doubling width doubles zoom.
        assert!((z2 - 2.0 * z1).abs() < 1e-9, "{} {}", z1, z2);
        // view_start depends only on the day, not the pixel width.
        assert!((vs1 - vs2).abs() < 1e-9, "{} {}", vs1, vs2);
        // Concrete: span == DAY_SECS, so z1 == 1000 / DAY_SECS.
        assert!((z1 - 1000.0 / DAY_SECS).abs() < 1e-12, "{}", z1);
    }

    #[wasm_bindgen_test]
    fn macro_landing_span_equals_one_day() {
        // The landing span constant is exactly one UTC day.
        assert_eq!(MACRO_LANDING_SPAN_SECS, DAY_SECS);
        // And the view places the day midpoint at the centre: view_start is half
        // a day before the day midpoint, i.e. at the day start itself.
        let (view_start, _zoom) = day_tap_macro_view(DAY0, 800.0);
        assert!((view_start - DAY0).abs() < 1e-6, "{}", view_start);
    }

    // ---- calendar-aligned bucket boundaries ------------------------------

    fn ymd(y: i32, m: u32, d: u32) -> f64 {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64
    }

    #[wasm_bindgen_test]
    fn week_buckets_start_on_monday_utc() {
        // Epoch day 0 is a Thursday, so the integer offset has to be right or
        // every week label is off by a few days.
        assert_eq!(utc_week_start(0.0), ymd(1969, 12, 29));
        // 2026-07-29 is a Wednesday; it snaps back to Monday the 27th.
        assert_eq!(utc_week_start(ymd(2026, 7, 29)), ymd(2026, 7, 27));
        // A Monday is its own week start (idempotent).
        assert_eq!(utc_week_start(ymd(2026, 7, 27)), ymd(2026, 7, 27));
        // And the invariant holds across a whole year of arbitrary days.
        use chrono::Datelike;
        for offset in 0..370 {
            let ts = ymd(2026, 1, 1) + offset as f64 * DAY_SECS;
            let monday = utc_week_start(ts);
            let wd = chrono::DateTime::from_timestamp(monday as i64, 0)
                .unwrap()
                .weekday();
            assert_eq!(wd, chrono::Weekday::Mon, "offset {offset}");
            assert!(monday <= ts && ts - monday < 7.0 * DAY_SECS);
        }
    }

    #[wasm_bindgen_test]
    fn month_buckets_are_calendar_aligned_and_variable_width() {
        // Fixed 30-day blocks would drift off the months they name; real
        // months don't.
        assert_eq!(utc_month_start(ymd(2024, 2, 17)), ymd(2024, 2, 1));
        // February 2024 is a leap month: 29 days.
        let feb = ymd(2024, 2, 1);
        assert_eq!(
            next_bucket_start(feb, BucketGranularity::Month) - feb,
            29.0 * DAY_SECS
        );
        // January is 31.
        let jan = ymd(2024, 1, 1);
        assert_eq!(
            next_bucket_start(jan, BucketGranularity::Month) - jan,
            31.0 * DAY_SECS
        );
    }

    #[wasm_bindgen_test]
    fn month_stepping_crosses_the_year_boundary() {
        let dec = ymd(2025, 12, 1);
        assert_eq!(
            next_bucket_start(dec, BucketGranularity::Month),
            ymd(2026, 1, 1)
        );
    }

    #[wasm_bindgen_test]
    fn quarters_align_to_jan_apr_jul_oct() {
        assert_eq!(utc_quarter_start(ymd(2026, 2, 14)), ymd(2026, 1, 1));
        assert_eq!(utc_quarter_start(ymd(2026, 5, 1)), ymd(2026, 4, 1));
        assert_eq!(utc_quarter_start(ymd(2026, 9, 30)), ymd(2026, 7, 1));
        assert_eq!(utc_quarter_start(ymd(2026, 12, 31)), ymd(2026, 10, 1));
        let q4 = ymd(2026, 10, 1);
        assert_eq!(
            next_bucket_start(q4, BucketGranularity::Quarter),
            ymd(2027, 1, 1)
        );
    }

    #[wasm_bindgen_test]
    fn boundaries_are_ascending_and_cover_the_view() {
        for g in [
            BucketGranularity::Day,
            BucketGranularity::Week,
            BucketGranularity::Month,
            BucketGranularity::Quarter,
        ] {
            let lo = ymd(2024, 3, 15);
            let hi = ymd(2025, 8, 2);
            let edges = bucket_boundaries(lo, hi, g);
            assert!(edges.len() >= 2, "{g:?}");
            assert!(edges[0] <= lo, "{g:?}");
            assert!(*edges.last().unwrap() > hi, "{g:?}");
            assert!(edges.windows(2).all(|w| w[1] > w[0]), "{g:?}");
        }
    }

    #[wasm_bindgen_test]
    fn bucket_count_is_bounded_at_every_granularity() {
        // A 36-year span must never allocate a runaway vector, whichever rung
        // it is asked for.
        let lo = ymd(1991, 6, 5);
        let hi = ymd(2027, 1, 1);
        for g in [
            BucketGranularity::Day,
            BucketGranularity::Week,
            BucketGranularity::Month,
            BucketGranularity::Quarter,
        ] {
            let edges = bucket_boundaries(lo, hi, g);
            assert!(edges.len() <= MAX_BUCKETS + 1, "{g:?}: {}", edges.len());
        }
    }

    #[wasm_bindgen_test]
    fn the_whole_era_at_the_terminal_rung_fits_well_inside_the_cap() {
        // The ladder, not the cap, is what keeps the count sane: ~144 quarters
        // across the era.
        let edges = bucket_boundaries(ymd(1991, 6, 5), ymd(2027, 1, 1), BucketGranularity::Quarter);
        assert!(edges.len() - 1 > 100);
        assert!(edges.len() - 1 < 200);
    }

    #[wasm_bindgen_test]
    fn fractions_use_the_buckets_own_span_not_a_fixed_day() {
        // A fully-covered week must read 1.0, not 7.0 — the denominator has to
        // follow the variable bucket width.
        let week_start = utc_week_start(ymd(2026, 7, 29));
        let cache = RadarTimeline {
            scans: vec![scan_span(week_start, week_start + 7.0 * DAY_SECS, true)],
        };
        let buckets = aggregate_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            week_start,
            week_start + 100.0,
            BucketGranularity::Week,
        );
        assert!((buckets[0].availability_frac - 1.0).abs() < 1e-4);
        assert!((buckets[0].cache_frac - 1.0).abs() < 1e-4);
    }

    #[wasm_bindgen_test]
    fn absolute_seconds_survive_coarse_rungs() {
        // At a quarter rung a few cached hours round to ~0%, so the fraction
        // alone can't tell an empty bucket from a lightly-populated one. The
        // absolutes are what the tooltip reports.
        let q = ymd(2026, 7, 1);
        let cache = RadarTimeline {
            scans: vec![scan_span(q + 1000.0, q + 1000.0 + 3600.0, true)],
        };
        let buckets = aggregate_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            q,
            q + 100.0,
            BucketGranularity::Quarter,
        );
        assert!(buckets[0].cache_frac < 0.001);
        assert!((buckets[0].cached_secs - 3600.0).abs() < 1.0);
    }

    // ---- drill-down ladder ------------------------------------------------

    #[wasm_bindgen_test]
    fn tapping_drills_one_rung_at_a_time() {
        // A quarter cell holds ~90 days; jumping straight to a single day is a
        // 90x leap that discards all context. Each tap should frame exactly
        // the tapped bucket instead.
        let width = 1200.0;
        for (g, start, end) in [
            (
                BucketGranularity::Quarter,
                ymd(2026, 7, 1),
                ymd(2026, 10, 1),
            ),
            (BucketGranularity::Month, ymd(2026, 7, 1), ymd(2026, 8, 1)),
            (
                BucketGranularity::Week,
                ymd(2026, 7, 27),
                ymd(2026, 7, 27) + 7.0 * DAY_SECS,
            ),
        ] {
            let bucket = TimeBucket::empty(start, end);
            let (view_start, zoom) = bucket_tap_target(&bucket, g, width);
            assert!((view_start - start).abs() < 1e-6, "{g:?}");
            // The landed view shows exactly this bucket.
            assert!(((width / zoom) - (end - start)).abs() < 1.0, "{g:?}");
        }
    }

    #[wasm_bindgen_test]
    fn tapping_the_finest_rung_hands_off_to_macro() {
        let bucket = TimeBucket::empty(DAY0, DAY0 + DAY_SECS);
        let (view_start, zoom) = bucket_tap_target(&bucket, BucketGranularity::Day, 800.0);
        let (macro_start, macro_zoom) = day_tap_macro_view(DAY0, 800.0);
        assert!((view_start - macro_start).abs() < 1e-6);
        assert!((zoom - macro_zoom).abs() < 1e-12);
    }

    #[wasm_bindgen_test]
    fn a_coarse_tap_stays_in_the_calendar_and_gains_detail() {
        // Framing the tapped bucket keeps the user inside the Archive tier —
        // they see what they tapped, rendered at a finer rung — rather than
        // being thrown straight down to a single day in Macro. From a quarter
        // that means the quarter as day cells, which is exactly "look closer".
        let width = 1200.0;
        for (tapped, start, end) in [
            (
                BucketGranularity::Quarter,
                ymd(2026, 7, 1),
                ymd(2026, 10, 1),
            ),
            (BucketGranularity::Month, ymd(2026, 7, 1), ymd(2026, 8, 1)),
        ] {
            let bucket = TimeBucket::empty(start, end);
            let (_, zoom) = bucket_tap_target(&bucket, tapped, width);
            let landed = BucketGranularity::seed(zoom);
            // Strictly finer than what was tapped.
            assert!(
                landed.nominal_secs() < tapped.nominal_secs(),
                "{tapped:?} -> {landed:?}"
            );
            // Still a calendar span, not a jump into the linear tiers.
            assert!(width / zoom > 60.0 * 3600.0, "{tapped:?}");
            // And the cells it lands on are legible.
            assert!(landed.nominal_secs() * zoom >= 5.0, "{tapped:?}");
        }
    }
}
