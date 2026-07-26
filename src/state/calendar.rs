//! Per-day coverage aggregation for the Archive zoom tier (spec §6.4).
//!
//! The Archive tier replaces the deprecated year-wide strip zoom with a
//! calendar-style coverage heatmap (GitHub-contributions grammar): a run of
//! day cells, each toned by **how much data exists that day** (server
//! availability) and **how much is cached locally** (downloaded). Tapping a day
//! zooms into the Macro tier centred on that day.
//!
//! This module is the *pure data* behind the heatmap. It derives per-day
//! buckets from the in-memory timeline state the strip already holds — the
//! cached scans ([`RadarTimeline`]) for the CACHE tone and the archive shadow
//! boundaries ([`ScanBoundary`], the listing's coverage) for the AVAILABILITY
//! tone. It adds **no IDB round-trips and no async**; days outside the listed
//! window honestly read as empty rather than fabricating coverage (the listing
//! only spans ranges the archive index has fetched — that's acceptable, the
//! calendar shows what is genuinely known).
//!
//! **Day bucketing is by UTC day** (`floor(ts / 86400)`), matching the
//! millisecond storage-key convention; it is deterministic and timezone-free,
//! so the aggregation is unit-testable without a clock. The renderer labels day
//! cells from the same UTC day start.

use crate::core::RadarTimeline;
use crate::core::ScanBoundary;
use crate::state::SavedEvents;

/// Seconds in a UTC day. Day buckets are `[day_start, day_start + DAY_SECS)`.
pub(crate) const DAY_SECS: f64 = 86_400.0;

/// Hard cap on how many day cells the aggregator emits, so an absurd span
/// (e.g. an old URL with a microscopic zoom) can't allocate a runaway vector.
/// The Archive tier's widest sensible view is months, not millennia; past this
/// the heatmap is unreadable anyway.
pub(crate) const MAX_DAY_BUCKETS: usize = 800;

/// One day's coverage in the Archive heatmap.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct DayBucket {
    /// UTC day start (Unix seconds) — the cell's identity and the tap-to-zoom
    /// target's day origin.
    pub day_start: f64,
    /// Fraction `0..1` of the day for which data is *known to exist* on the
    /// server (the union of archive-listing spans and cached-scan spans clamped
    /// to the day). 0 ⇒ no listing/cache touches this day (unknown/empty).
    pub availability_frac: f32,
    /// Fraction `0..1` of the day covered by *downloaded* (cached) scans. Always
    /// ≤ `availability_frac` (cached data is also available data).
    pub cache_frac: f32,
    /// A saved event for the current site starts or overlaps this day.
    pub has_events: bool,
}

impl DayBucket {
    /// An empty (no data known) bucket for `day_start`.
    fn empty(day_start: f64) -> Self {
        Self {
            day_start,
            availability_frac: 0.0,
            cache_frac: 0.0,
            has_events: false,
        }
    }
}

/// The UTC day start (Unix seconds) containing `ts`.
pub(crate) fn utc_day_start(ts: f64) -> f64 {
    (ts / DAY_SECS).floor() * DAY_SECS
}

/// Aggregate the timeline sources into per-day buckets spanning the visible
/// Archive window `[view_start, view_end]` (Unix seconds).
///
/// One bucket per UTC day from `view_start`'s day through `view_end`'s day
/// (inclusive), capped at [`MAX_DAY_BUCKETS`]. Days with no listing and no cache
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
pub(crate) fn aggregate_day_buckets(
    cache: &RadarTimeline,
    shadows: &[ScanBoundary],
    saved_events: &SavedEvents,
    current_site: &str,
    view_start: f64,
    view_end: f64,
) -> Vec<DayBucket> {
    let first_day = utc_day_start(view_start.min(view_end));
    let last_day = utc_day_start(view_start.max(view_end));
    let day_count = (((last_day - first_day) / DAY_SECS).round() as i64 + 1)
        .clamp(1, MAX_DAY_BUCKETS as i64) as usize;

    // Availability and cache coverage accumulators, in seconds-per-day. Kept as
    // span lists per day then unioned, so overlapping spans don't over-count.
    let mut avail_spans: Vec<Vec<(f64, f64)>> = vec![Vec::new(); day_count];
    let mut cache_spans: Vec<Vec<(f64, f64)>> = vec![Vec::new(); day_count];

    let day_index = |ts: f64| -> Option<usize> {
        let idx = ((utc_day_start(ts) - first_day) / DAY_SECS).round() as i64;
        (0..day_count as i64).contains(&idx).then_some(idx as usize)
    };

    // Push a [start, end] span into per-day accumulators, splitting it at day
    // boundaries so each day only sees its own slice.
    let push_span = |spans: &mut [Vec<(f64, f64)>], start: f64, end: f64| {
        if end <= start {
            return;
        }
        let mut cursor = start.max(first_day);
        let stop = end.min(last_day + DAY_SECS);
        while cursor < stop {
            let day = utc_day_start(cursor);
            let day_end = day + DAY_SECS;
            let slice_end = stop.min(day_end);
            if let Some(idx) = day_index(cursor) {
                spans[idx].push((cursor, slice_end));
            }
            cursor = day_end;
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
    for b in shadows {
        push_span(&mut avail_spans, b.start as f64, b.end as f64);
    }

    let mut buckets: Vec<DayBucket> = (0..day_count)
        .map(|i| {
            let day_start = first_day + i as f64 * DAY_SECS;
            let avail = union_seconds(&mut avail_spans[i]);
            let cached = union_seconds(&mut cache_spans[i]);
            let mut b = DayBucket::empty(day_start);
            b.availability_frac = (avail / DAY_SECS).clamp(0.0, 1.0) as f32;
            // Cache can't exceed availability (it is a subset), but float slop
            // could nudge it past; clamp to keep the visual invariant.
            b.cache_frac = (cached / DAY_SECS).clamp(0.0, b.availability_frac as f64) as f32;
            b
        })
        .collect();

    // Saved-event flag: an event for the current site that overlaps the day.
    for event in &saved_events.events {
        if event.site_id != current_site {
            continue;
        }
        let (es, ee) = (
            event.start_time.min(event.end_time),
            event.start_time.max(event.end_time),
        );
        let mut cursor = utc_day_start(es.max(first_day));
        while cursor <= ee && cursor <= last_day {
            if let Some(idx) = day_index(cursor) {
                buckets[idx].has_events = true;
            }
            cursor += DAY_SECS;
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
        assert_eq!(buckets[0].day_start, DAY0 + DAY_SECS);
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
        assert_eq!(buckets[0].day_start, DAY0);
        assert_eq!(buckets[3].day_start, DAY0 + 3.0 * DAY_SECS);
    }

    #[wasm_bindgen_test]
    fn absurd_span_is_capped() {
        let cache = RadarTimeline { scans: Vec::new() };
        // A 10000-year span would be ~3.6M days; capped at MAX_DAY_BUCKETS.
        let buckets = aggregate_day_buckets(
            &cache,
            &[],
            &no_events(),
            "KDMX",
            DAY0,
            DAY0 + 10_000.0 * 365.0 * DAY_SECS,
        );
        assert_eq!(buckets.len(), MAX_DAY_BUCKETS);
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
    fn day_fully_covered_clamps_availability_to_one() {
        // A shadow span longer than the whole day; its day-slice is exactly one
        // day of seconds, so availability_frac == 1.0 (and cache_frac == 0).
        let cache = RadarTimeline { scans: Vec::new() };
        let shadows = vec![ScanBoundary {
            start: (DAY0 - DAY_SECS) as i64,
            end: (DAY0 + 2.0 * DAY_SECS) as i64,
        }];
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
        let shadows = vec![ScanBoundary {
            start: (DAY0 + 10000.0) as i64,
            end: (DAY0 + 11000.0) as i64,
        }];
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
        // Cache [1000,4600] and shadow [3000,6000] overlap; availability union is
        // [1000,6000]=5000s (NOT 3600+3000).
        let cache = RadarTimeline {
            scans: vec![scan_span(DAY0 + 1000.0, DAY0 + 4600.0, true)],
        };
        let shadows = vec![ScanBoundary {
            start: (DAY0 + 3000.0) as i64,
            end: (DAY0 + 6000.0) as i64,
        }];
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

    // --- DayBucket equality / Copy semantics on an empty day ---

    #[wasm_bindgen_test]
    fn empty_day_buckets_compare_equal() {
        let cache = RadarTimeline { scans: Vec::new() };
        let a = aggregate_day_buckets(&cache, &[], &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        let b = aggregate_day_buckets(&cache, &[], &no_events(), "KDMX", DAY0, DAY0 + 100.0);
        assert_eq!(a[0], b[0]);
        // Copy: the bucket can be duplicated and remains equal.
        let copied = a[0];
        assert_eq!(copied, a[0]);
        assert_eq!(copied.day_start, DAY0);
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
}
