//! Storage-quota policy: every byte threshold the cache layer consults,
//! plus the pure decision behind `MainThreadStore::check_and_evict`.
//!
//! The user-configurable cache size itself lives in `StorageSettings`;
//! this module owns the *rules* applied around it so worker ingest, the
//! eviction facade, and the settings UI all agree on the same numbers.

use crate::data::indexeddb::StorageQuotaEstimate;

/// All quota thresholds in one place.
#[derive(Debug, Clone, Copy)]
pub struct QuotaPolicy {
    /// Headroom required beyond an ingest batch before admitting it,
    /// allowing for IDB structured-clone/index overhead.
    pub ingest_headroom_bytes: u64,
    /// Fraction of the *browser* quota below which storage is considered
    /// critically low: proactive eviction triggers and the UI shows a
    /// warning.
    pub browser_low_fraction: f64,
    /// Fraction of the app quota that eviction shrinks the cache down to,
    /// so eviction passes don't re-trigger on the very next ingest.
    pub eviction_target_fraction: f64,
}

impl QuotaPolicy {
    pub const DEFAULT: QuotaPolicy = QuotaPolicy {
        ingest_headroom_bytes: 5 * 1024 * 1024,
        browser_low_fraction: 0.10,
        eviction_target_fraction: 0.80,
    };
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Browser-quota pressure details, surfaced to the UI as a warning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuotaWarning {
    pub remaining_bytes: u64,
    pub browser_quota_bytes: u64,
}

impl QuotaWarning {
    pub fn message(&self) -> String {
        format!(
            "Storage nearly full: {:.0} MB remaining of {:.0} MB browser quota",
            self.remaining_bytes as f64 / (1024.0 * 1024.0),
            self.browser_quota_bytes as f64 / (1024.0 * 1024.0),
        )
    }
}

/// Outcome of a quota check: whether to evict (and down to what size), and
/// whether to warn the user about browser storage pressure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvictionDecision {
    /// Evict down to this size; `None` = no eviction needed.
    pub evict_to: Option<u64>,
    /// Present when the browser quota is critically low.
    pub warning: Option<QuotaWarning>,
}

/// Pure decision for `MainThreadStore::check_and_evict`: evict when the cache
/// exceeds the app-level quota OR the browser's own storage is nearly
/// full (the latter also produces a user-facing warning). When the
/// browser estimate is unavailable only the app-level rule applies.
pub fn decide_eviction(
    current_size: u64,
    app_quota_bytes: u64,
    eviction_target_bytes: u64,
    browser: Option<StorageQuotaEstimate>,
    policy: &QuotaPolicy,
) -> EvictionDecision {
    let over_app_quota = current_size > app_quota_bytes;
    let browser_low = browser
        .is_some_and(|e| (e.remaining() as f64) < e.quota as f64 * policy.browser_low_fraction);
    EvictionDecision {
        evict_to: (over_app_quota || browser_low).then_some(eviction_target_bytes),
        warning: browser.filter(|_| browser_low).map(|e| QuotaWarning {
            remaining_bytes: e.remaining(),
            browser_quota_bytes: e.quota,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const POLICY: QuotaPolicy = QuotaPolicy::DEFAULT;

    fn estimate(quota: u64, usage: u64) -> StorageQuotaEstimate {
        StorageQuotaEstimate { quota, usage }
    }

    #[wasm_bindgen_test]
    fn no_eviction_when_under_both_quotas() {
        let d = decide_eviction(100, 1000, 800, Some(estimate(10_000, 1_000)), &POLICY);
        assert_eq!(d.evict_to, None);
        assert_eq!(d.warning, None);
    }

    #[wasm_bindgen_test]
    fn evicts_when_over_app_quota_only() {
        let d = decide_eviction(1500, 1000, 800, Some(estimate(10_000, 1_000)), &POLICY);
        assert_eq!(d.evict_to, Some(800));
        assert_eq!(d.warning, None, "healthy browser quota must not warn");
    }

    #[wasm_bindgen_test]
    fn evicts_and_warns_when_browser_quota_low() {
        // 5% remaining < 10% threshold.
        let d = decide_eviction(100, 1000, 800, Some(estimate(10_000, 9_500)), &POLICY);
        assert_eq!(d.evict_to, Some(800));
        let w = d.warning.expect("low browser quota must warn");
        assert_eq!(w.remaining_bytes, 500);
        assert_eq!(w.browser_quota_bytes, 10_000);
    }

    #[wasm_bindgen_test]
    fn over_both_evicts_once_and_warns() {
        let d = decide_eviction(1500, 1000, 800, Some(estimate(10_000, 9_500)), &POLICY);
        assert_eq!(d.evict_to, Some(800));
        assert!(d.warning.is_some());
    }

    #[wasm_bindgen_test]
    fn missing_browser_estimate_applies_app_rule_only() {
        let under = decide_eviction(100, 1000, 800, None, &POLICY);
        assert_eq!(under.evict_to, None);
        assert_eq!(under.warning, None);

        let over = decide_eviction(1500, 1000, 800, None, &POLICY);
        assert_eq!(over.evict_to, Some(800));
        assert_eq!(over.warning, None);
    }

    #[wasm_bindgen_test]
    fn browser_threshold_is_exclusive_at_boundary() {
        // Exactly 10% remaining is NOT "low" (strict less-than).
        let d = decide_eviction(100, 1000, 800, Some(estimate(10_000, 9_000)), &POLICY);
        assert_eq!(d.evict_to, None);
        assert_eq!(d.warning, None);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    const POLICY: QuotaPolicy = QuotaPolicy::DEFAULT;

    fn estimate(quota: u64, usage: u64) -> StorageQuotaEstimate {
        StorageQuotaEstimate { quota, usage }
    }

    // --- QuotaPolicy constants / Default impl ---------------------------------

    #[wasm_bindgen_test]
    fn default_policy_constants_are_as_documented() {
        assert_eq!(QuotaPolicy::DEFAULT.ingest_headroom_bytes, 5 * 1024 * 1024);
        // 5 MiB == 5_242_880 bytes (hand-computed).
        assert_eq!(QuotaPolicy::DEFAULT.ingest_headroom_bytes, 5_242_880);
        assert!((QuotaPolicy::DEFAULT.browser_low_fraction - 0.10).abs() < 1e-12);
        assert!((QuotaPolicy::DEFAULT.eviction_target_fraction - 0.80).abs() < 1e-12);
    }

    #[wasm_bindgen_test]
    fn default_trait_matches_const_default() {
        let d = QuotaPolicy::default();
        assert_eq!(
            d.ingest_headroom_bytes,
            QuotaPolicy::DEFAULT.ingest_headroom_bytes
        );
        assert!((d.browser_low_fraction - QuotaPolicy::DEFAULT.browser_low_fraction).abs() < 1e-12);
        assert!(
            (d.eviction_target_fraction - QuotaPolicy::DEFAULT.eviction_target_fraction).abs()
                < 1e-12
        );
    }

    // --- QuotaWarning::message() formatting -----------------------------------

    #[wasm_bindgen_test]
    fn warning_message_formats_megabytes_rounded() {
        // Choose byte counts that divide evenly into whole MiB so {:.0}
        // produces exact, non-rounding-sensitive strings:
        //   3 MiB remaining of 10 MiB quota.
        let w = QuotaWarning {
            remaining_bytes: 3 * 1024 * 1024,
            browser_quota_bytes: 10 * 1024 * 1024,
        };
        assert_eq!(
            w.message(),
            "Storage nearly full: 3 MB remaining of 10 MB browser quota"
        );
    }

    #[wasm_bindgen_test]
    fn warning_message_rounds_sub_megabyte_remaining_to_zero() {
        // 500 bytes / 1 MiB ≈ 0.0005 → "0"; 1 MiB quota → "1".
        let w = QuotaWarning {
            remaining_bytes: 500,
            browser_quota_bytes: 1024 * 1024,
        };
        assert_eq!(
            w.message(),
            "Storage nearly full: 0 MB remaining of 1 MB browser quota"
        );
    }

    // --- decide_eviction: saturation / boundary / custom policy ---------------

    #[wasm_bindgen_test]
    fn browser_remaining_saturates_when_usage_exceeds_quota() {
        // usage > quota → remaining() saturates to 0, which is < any positive
        // threshold, so the browser-low rule fires (evicts + warns) even
        // though current_size is comfortably under the app quota.
        let d = decide_eviction(100, 1000, 800, Some(estimate(1_000, 2_000)), &POLICY);
        assert_eq!(d.evict_to, Some(800));
        let w = d.warning.expect("saturated (zero) remaining must warn");
        assert_eq!(w.remaining_bytes, 0);
        assert_eq!(w.browser_quota_bytes, 1_000);
    }

    #[wasm_bindgen_test]
    fn browser_just_below_threshold_is_low() {
        // 9% remaining (< 10%) → low. quota 10_000, usage 9_100 → remaining 900,
        // threshold = 10_000 * 0.10 = 1000, and 900.0 < 1000.0.
        let d = decide_eviction(100, 1000, 800, Some(estimate(10_000, 9_100)), &POLICY);
        assert_eq!(d.evict_to, Some(800));
        let w = d.warning.expect("9% remaining is below the 10% threshold");
        assert_eq!(w.remaining_bytes, 900);
        assert_eq!(w.browser_quota_bytes, 10_000);
    }

    #[wasm_bindgen_test]
    fn browser_just_above_threshold_is_healthy() {
        // 11% remaining (> 10%) → healthy. usage 8_900 → remaining 1_100 >
        // threshold 1_000.
        let d = decide_eviction(100, 1000, 800, Some(estimate(10_000, 8_900)), &POLICY);
        assert_eq!(d.evict_to, None);
        assert_eq!(d.warning, None);
    }

    #[wasm_bindgen_test]
    fn current_size_equal_to_app_quota_does_not_evict() {
        // over_app_quota uses strict greater-than: equal is not over.
        let d = decide_eviction(1000, 1000, 800, Some(estimate(10_000, 1_000)), &POLICY);
        assert_eq!(d.evict_to, None);
        assert_eq!(d.warning, None);
    }

    #[wasm_bindgen_test]
    fn custom_policy_threshold_changes_browser_low_decision() {
        // With a 50% threshold, 30% remaining is now considered low even
        // though DEFAULT's 10% threshold would treat it as healthy.
        let strict = QuotaPolicy {
            browser_low_fraction: 0.50,
            ..QuotaPolicy::DEFAULT
        };
        let est = estimate(10_000, 7_000); // remaining 3_000 = 30%.
        let strict_d = decide_eviction(100, 1000, 800, Some(est), &strict);
        assert_eq!(strict_d.evict_to, Some(800));
        assert!(strict_d.warning.is_some());

        // Same inputs under the lenient default → healthy.
        let lenient_d = decide_eviction(100, 1000, 800, Some(est), &POLICY);
        assert_eq!(lenient_d.evict_to, None);
        assert_eq!(lenient_d.warning, None);
    }
}
