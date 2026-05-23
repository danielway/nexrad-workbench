//! Pure-logic decision functions used by `IndexedDbStore`.
//!
//! Extracted from `indexeddb.rs` so callers can be tested without a real
//! IDB. Every function here is `pub(super)` and stateless.

use super::{DataError, StorageQuotaEstimate, PREFIX_RANGE_UPPER};
use crate::data::keys::{ScanIndexEntry, ScanKey, SiteId, UnixMillis};
use std::collections::HashMap;

/// Inclusive lower / exclusive-by-construction upper bounds for the
/// scan-index key range covering all entries for `site`.
///
/// Real IDB uses these via `IdbKeyRange::bound`. `\u{FFFF}` sorts after
/// any character that appears in real keys (decimal digits + `|`), so
/// the upper bound captures every `"SITE|<ms>"` without spilling into
/// the next site.
pub(super) fn site_prefix_bounds(site: &SiteId) -> (String, String) {
    let lower = format!("{}|", site.0);
    let upper = format!("{}|{}", site.0, PREFIX_RANGE_UPPER);
    (lower, upper)
}

/// Bounds for the sweep-store key range covering every blob belonging
/// to a single scan (`"SITE|MS|<elev>|<product>"`). Used to delete an
/// entire scan's blobs in one IDB range-delete without enumerating
/// product names.
pub(super) fn scan_prefix_bounds(scan: &ScanKey) -> (String, String) {
    let prefix = format!("{}|", scan.to_storage_key());
    let upper = format!("{}{}", prefix, PREFIX_RANGE_UPPER);
    (prefix, upper)
}

/// Decides whether a `touch_scan` call should be deduplicated against
/// an in-memory record of the previous touch.
///
/// Skip when a previous touch exists AND it was less than
/// `threshold_ms` in the past. A negative delta (clock skew, NTP step)
/// counts as "in the past" → skip — we'd rather lose a touch than
/// thrash IDB on a backwards clock jump.
pub(super) fn should_skip_touch(
    now: UnixMillis,
    last: Option<UnixMillis>,
    threshold_ms: i64,
) -> bool {
    match last {
        Some(last) => (now.0 - last.0) < threshold_ms,
        None => false,
    }
}

/// Decides whether a sweep-blob batch should be admitted given the
/// browser's current storage estimate, with 5 MB of headroom for IDB
/// overhead. Returning `Ok(())` when the estimate is unavailable
/// matches real-IDB behaviour: if we can't ask, we let IDB itself
/// reject the write at commit time.
pub(super) fn decide_quota(
    batch_bytes: u64,
    estimate: Option<StorageQuotaEstimate>,
) -> Result<(), DataError> {
    if batch_bytes == 0 {
        return Ok(());
    }
    let Some(estimate) = estimate else {
        return Ok(());
    };
    let remaining = estimate.remaining();
    let required = batch_bytes + 5 * 1024 * 1024;
    if remaining < required {
        return Err(DataError::QuotaExceeded {
            available_mb: remaining as f64 / (1024.0 * 1024.0),
            required_mb: required as f64 / (1024.0 * 1024.0),
        });
    }
    Ok(())
}

/// Eviction order: oldest `scan_touches` first. Entries with no touch
/// entry sort to position 0 (treated as `UnixMillis(0)`) so they are
/// evicted ahead of any touched scan — the cleanup path for any scan
/// whose touch is missing.
pub(super) fn eviction_order(
    entries: &[ScanIndexEntry],
    touches: &HashMap<ScanKey, UnixMillis>,
) -> Vec<ScanKey> {
    let mut sorted: Vec<&ScanIndexEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| touches.get(&e.scan).copied().unwrap_or(UnixMillis(0)).0);
    sorted.into_iter().map(|e| e.scan.clone()).collect()
}

/// Filters a list of scan-index entries to those within the inclusive
/// `[start, end]` window, sorted by `scan_start`. Used after a
/// site-prefix range scan in `list_scans`.
pub(super) fn filter_scans_by_time_window(
    entries: Vec<ScanIndexEntry>,
    start: UnixMillis,
    end: UnixMillis,
) -> Vec<ScanIndexEntry> {
    let mut scans: Vec<ScanIndexEntry> = entries
        .into_iter()
        .filter(|entry| entry.scan.scan_start >= start && entry.scan.scan_start <= end)
        .collect();
    scans.sort_by_key(|s| s.scan.scan_start.0);
    scans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::keys::{CachedSweep, ExtractedVcp, ScanCompleteness};
    use wasm_bindgen_test::wasm_bindgen_test;

    // --- helpers ---
    fn key(site: &str, ms: i64) -> ScanKey {
        ScanKey::new(site, UnixMillis(ms))
    }
    fn entry(site: &str, ms: i64, size: u64) -> ScanIndexEntry {
        ScanIndexEntry {
            scan: key(site, ms),
            vcp: None,
            file_name: None,
            cached_sweeps: Vec::new(),
            total_size_bytes: size,
        }
    }

    // ===== site_prefix_bounds =====

    #[wasm_bindgen_test]
    fn site_prefix_bounds_format() {
        let (lo, hi) = site_prefix_bounds(&SiteId::new("KDMX"));
        assert_eq!(lo, "KDMX|");
        assert_eq!(hi, format!("KDMX|{}", PREFIX_RANGE_UPPER));
    }

    #[wasm_bindgen_test]
    fn site_prefix_bounds_includes_all_timestamps_lex() {
        // Every realistic `SITE|<13-digit-ms>` key falls strictly
        // between the two bounds.
        let (lo, hi) = site_prefix_bounds(&SiteId::new("KTLX"));
        for ms in ["1000000000000", "1700000000000", "9999999999999"] {
            let k = format!("KTLX|{}", ms);
            assert!(lo.as_str() < k.as_str(), "{} should be > {}", k, lo);
            assert!(k.as_str() < hi.as_str(), "{} should be < {}", k, hi);
        }
    }

    #[wasm_bindgen_test]
    fn site_prefix_bounds_excludes_other_sites() {
        let (lo, hi) = site_prefix_bounds(&SiteId::new("KDMX"));
        // "KDMY|0" must NOT fall inside KDMX's range.
        let other = "KDMY|1700000000000";
        assert!(!(lo.as_str() < other && other < hi.as_str()));
        // The lex predecessor "KDMW|..." also outside.
        let earlier = "KDMW|1700000000000";
        assert!(!(lo.as_str() < earlier && earlier < hi.as_str()));
    }

    // ===== scan_prefix_bounds =====

    #[wasm_bindgen_test]
    fn scan_prefix_bounds_includes_all_elev_product_pairs() {
        let scan = key("KDMX", 1700000000000);
        let (lo, hi) = scan_prefix_bounds(&scan);
        assert_eq!(lo, "KDMX|1700000000000|");
        for sweep_key in [
            "KDMX|1700000000000|1|reflectivity",
            "KDMX|1700000000000|3|velocity",
            "KDMX|1700000000000|17|differential_phase",
        ] {
            assert!(lo.as_str() < sweep_key && sweep_key < hi.as_str());
        }
    }

    #[wasm_bindgen_test]
    fn scan_prefix_bounds_excludes_neighboring_scans() {
        let scan = key("KDMX", 1700000000000);
        let (lo, hi) = scan_prefix_bounds(&scan);
        // Adjacent ms -> different prefix, must not match.
        for foreign in [
            "KDMX|1700000000001|1|reflectivity",
            "KDMX|1699999999999|1|reflectivity",
            "KDMY|1700000000000|1|reflectivity",
        ] {
            assert!(
                !(lo.as_str() < foreign && foreign < hi.as_str()),
                "{} should be outside the scan range",
                foreign
            );
        }
    }

    // ===== should_skip_touch =====

    #[wasm_bindgen_test]
    fn should_skip_touch_no_prior() {
        assert!(!should_skip_touch(UnixMillis(1000), None, 60_000));
    }

    #[wasm_bindgen_test]
    fn should_skip_touch_inside_window() {
        // Last touch 30s ago, threshold 60s — skip.
        assert!(should_skip_touch(
            UnixMillis(60_000),
            Some(UnixMillis(30_000)),
            60_000
        ));
    }

    #[wasm_bindgen_test]
    fn should_skip_touch_at_exact_boundary() {
        // delta == threshold is "not less than" → don't skip.
        assert!(!should_skip_touch(
            UnixMillis(60_000),
            Some(UnixMillis(0)),
            60_000
        ));
    }

    #[wasm_bindgen_test]
    fn should_skip_touch_outside_window() {
        assert!(!should_skip_touch(
            UnixMillis(120_000),
            Some(UnixMillis(0)),
            60_000
        ));
    }

    #[wasm_bindgen_test]
    fn should_skip_touch_clock_skew_backward() {
        // `now < last`: delta is negative, definitely "less than threshold".
        // We treat that as "skip" rather than thrash. Documented behaviour.
        assert!(should_skip_touch(
            UnixMillis(0),
            Some(UnixMillis(60_000)),
            60_000
        ));
    }

    // ===== decide_quota =====

    #[wasm_bindgen_test]
    fn decide_quota_zero_batch_always_ok() {
        let est = StorageQuotaEstimate {
            quota: 100,
            usage: 100,
        };
        assert!(decide_quota(0, Some(est)).is_ok());
    }

    #[wasm_bindgen_test]
    fn decide_quota_no_estimate_passes_through() {
        // Storage API unavailable → defer to IDB itself.
        assert!(decide_quota(1024 * 1024 * 1024, None).is_ok());
    }

    #[wasm_bindgen_test]
    fn decide_quota_ok_with_headroom() {
        let est = StorageQuotaEstimate {
            quota: 100 * 1024 * 1024,
            usage: 0,
        };
        // 1 MB batch + 5 MB headroom = 6 MB; 100 MB available.
        assert!(decide_quota(1024 * 1024, Some(est)).is_ok());
    }

    #[wasm_bindgen_test]
    fn decide_quota_err_when_short_by_headroom() {
        // 4 MB remaining, 0 MB batch + 5 MB headroom = 5 MB required.
        let est = StorageQuotaEstimate {
            quota: 4 * 1024 * 1024,
            usage: 0,
        };
        // batch_bytes > 0 to trigger the check.
        let err = decide_quota(1, Some(est)).unwrap_err();
        assert!(matches!(err, DataError::QuotaExceeded { .. }));
    }

    #[wasm_bindgen_test]
    fn decide_quota_usage_consumed_correctly() {
        // 100 MB quota, 96 MB used → 4 MB remaining; 0+5 required → fail.
        let est = StorageQuotaEstimate {
            quota: 100 * 1024 * 1024,
            usage: 96 * 1024 * 1024,
        };
        assert!(decide_quota(1, Some(est)).is_err());
    }

    // ===== eviction_order =====

    #[wasm_bindgen_test]
    fn eviction_order_empty() {
        let order = eviction_order(&[], &HashMap::new());
        assert!(order.is_empty());
    }

    #[wasm_bindgen_test]
    fn eviction_order_single_entry() {
        let e = entry("KDMX", 100, 0);
        let mut t = HashMap::new();
        t.insert(e.scan.clone(), UnixMillis(50));
        let order = eviction_order(std::slice::from_ref(&e), &t);
        assert_eq!(order, vec![e.scan]);
    }

    #[wasm_bindgen_test]
    fn eviction_order_oldest_touch_first() {
        let a = entry("KDMX", 100, 0);
        let b = entry("KDMX", 200, 0);
        let c = entry("KDMX", 300, 0);
        let mut t = HashMap::new();
        t.insert(a.scan.clone(), UnixMillis(3000));
        t.insert(b.scan.clone(), UnixMillis(1000));
        t.insert(c.scan.clone(), UnixMillis(2000));
        let order = eviction_order(&[a.clone(), b.clone(), c.clone()], &t);
        assert_eq!(order, vec![b.scan, c.scan, a.scan]);
    }

    #[wasm_bindgen_test]
    fn eviction_order_missing_touch_evicts_first() {
        // Two touched entries + one untouched → untouched goes first.
        let touched = entry("KDMX", 100, 0);
        let stranded = entry("KDMX", 200, 0);
        let also_touched = entry("KDMX", 300, 0);
        let mut t = HashMap::new();
        t.insert(touched.scan.clone(), UnixMillis(1000));
        t.insert(also_touched.scan.clone(), UnixMillis(2000));
        // `stranded` has no touch entry.
        let order = eviction_order(
            &[touched.clone(), stranded.clone(), also_touched.clone()],
            &t,
        );
        assert_eq!(order, vec![stranded.scan, touched.scan, also_touched.scan]);
    }

    #[wasm_bindgen_test]
    fn eviction_order_all_missing_keeps_input_order_stably() {
        // No touches at all — all sort to 0; sort_by_key is stable so
        // input order is preserved.
        let a = entry("KDMX", 100, 0);
        let b = entry("KDMX", 200, 0);
        let order = eviction_order(&[a.clone(), b.clone()], &HashMap::new());
        assert_eq!(order, vec![a.scan, b.scan]);
    }

    // ===== filter_scans_by_time_window =====

    #[wasm_bindgen_test]
    fn filter_time_window_inclusive_bounds() {
        let entries = vec![
            entry("KDMX", 100, 0),
            entry("KDMX", 200, 0),
            entry("KDMX", 300, 0),
        ];
        let out = filter_scans_by_time_window(entries, UnixMillis(100), UnixMillis(300));
        // Both endpoints included.
        assert_eq!(out.len(), 3);
    }

    #[wasm_bindgen_test]
    fn filter_time_window_excludes_outside() {
        let entries = vec![
            entry("KDMX", 50, 0),
            entry("KDMX", 150, 0),
            entry("KDMX", 350, 0),
        ];
        let out = filter_scans_by_time_window(entries, UnixMillis(100), UnixMillis(300));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].scan.scan_start.0, 150);
    }

    #[wasm_bindgen_test]
    fn filter_time_window_sorts_ascending_by_start() {
        let entries = vec![
            entry("KDMX", 300, 0),
            entry("KDMX", 100, 0),
            entry("KDMX", 200, 0),
        ];
        let out = filter_scans_by_time_window(entries, UnixMillis(0), UnixMillis(1000));
        let starts: Vec<i64> = out.iter().map(|e| e.scan.scan_start.0).collect();
        assert_eq!(starts, vec![100, 200, 300]);
    }

    // ===== ScanIndexEntry accessors =====

    fn vcp_with(elevations: usize) -> ExtractedVcp {
        ExtractedVcp {
            number: 215,
            elevations: (0..elevations)
                .map(|i| crate::data::keys::ExtractedVcpElevation {
                    angle: 0.5 + i as f32,
                    waveform: "CS".to_string(),
                    prf_number: 1,
                    is_sails: false,
                    is_mrle: false,
                    is_base_tilt: false,
                    azimuth_rate: Some(20.0),
                })
                .collect(),
        }
    }
    fn cached_sweep(elev_num: u8, end: f64) -> CachedSweep {
        CachedSweep {
            start: end - 30.0,
            end,
            elevation: elev_num as f32 * 0.5,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            cached_products: Vec::new(),
        }
    }

    #[wasm_bindgen_test]
    fn entry_has_vcp_reflects_option() {
        let mut e = entry("KDMX", 0, 0);
        assert!(!e.has_vcp());
        e.vcp = Some(vcp_with(5));
        assert!(e.has_vcp());
    }

    #[wasm_bindgen_test]
    fn entry_planned_sweep_count() {
        let mut e = entry("KDMX", 0, 0);
        assert_eq!(e.planned_sweep_count(), None);
        e.vcp = Some(vcp_with(14));
        assert_eq!(e.planned_sweep_count(), Some(14));
    }

    #[wasm_bindgen_test]
    fn entry_cached_sweep_count() {
        let mut e = entry("KDMX", 0, 0);
        assert_eq!(e.cached_sweep_count(), 0);
        e.cached_sweeps = vec![cached_sweep(1, 100.0), cached_sweep(2, 130.0)];
        assert_eq!(e.cached_sweep_count(), 2);
    }

    #[wasm_bindgen_test]
    fn entry_end_timestamp_secs_takes_max() {
        let mut e = entry("KDMX", 0, 0);
        assert_eq!(e.end_timestamp_secs(), None);
        e.cached_sweeps = vec![
            cached_sweep(1, 100.0),
            cached_sweep(2, 220.5), // <- should win
            cached_sweep(3, 200.0),
        ];
        assert_eq!(e.end_timestamp_secs(), Some(220));
    }

    #[wasm_bindgen_test]
    fn entry_completeness_missing_when_empty() {
        let e = entry("KDMX", 0, 0);
        assert_eq!(e.completeness(), ScanCompleteness::Missing);
    }

    #[wasm_bindgen_test]
    fn entry_completeness_partial_no_vcp() {
        let mut e = entry("KDMX", 0, 0);
        e.cached_sweeps = vec![cached_sweep(1, 100.0)];
        assert_eq!(e.completeness(), ScanCompleteness::PartialNoVcp);
    }

    #[wasm_bindgen_test]
    fn entry_completeness_partial_with_vcp() {
        let mut e = entry("KDMX", 0, 0);
        e.vcp = Some(vcp_with(5));
        e.cached_sweeps = vec![cached_sweep(1, 100.0), cached_sweep(2, 130.0)];
        assert_eq!(e.completeness(), ScanCompleteness::PartialWithVcp);
    }

    #[wasm_bindgen_test]
    fn entry_completeness_complete_at_planned() {
        let mut e = entry("KDMX", 0, 0);
        e.vcp = Some(vcp_with(2));
        e.cached_sweeps = vec![cached_sweep(1, 100.0), cached_sweep(2, 130.0)];
        assert_eq!(e.completeness(), ScanCompleteness::Complete);
    }

    #[wasm_bindgen_test]
    fn entry_completeness_complete_when_overshot() {
        // SAILS/MRLE can produce more cuts than vcp.elevations.len()
        // claims; we should still report Complete.
        let mut e = entry("KDMX", 0, 0);
        e.vcp = Some(vcp_with(3));
        e.cached_sweeps = vec![
            cached_sweep(1, 100.0),
            cached_sweep(2, 130.0),
            cached_sweep(3, 160.0),
            cached_sweep(4, 190.0),
        ];
        assert_eq!(e.completeness(), ScanCompleteness::Complete);
    }
}
