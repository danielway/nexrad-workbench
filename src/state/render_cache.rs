//! Per-frame render-cache state.
//!
//! Tracks camera motion so expensive rendering work (label layout, centroid
//! math) can be deferred until the user has stopped panning/zooming, plus a
//! handful of small caches that smooth over per-frame recomputation.

use crate::geo::ProjectionFingerprint;

/// Default settle window: how long the camera must be stable before the
/// label tier rebuilds its cache. 180 ms feels responsive without thrashing
/// the cache during a continuous pan/zoom gesture.
pub const SETTLE_WINDOW_SECS: f64 = 0.18;

/// Tracks whether the map camera (projection) has come to rest. Consumers
/// (e.g. the label tier in the geo renderer) check `is_settled()` to decide
/// whether to do expensive recomputation this frame.
#[derive(Debug, Clone)]
pub struct CameraMotion {
    last_fingerprint: Option<ProjectionFingerprint>,
    last_change_secs: f64,
    settle_window_secs: f64,
}

impl Default for CameraMotion {
    fn default() -> Self {
        Self {
            last_fingerprint: None,
            last_change_secs: 0.0,
            settle_window_secs: SETTLE_WINDOW_SECS,
        }
    }
}

impl CameraMotion {
    /// Record the projection observed this frame.
    pub fn observe(&mut self, fp: ProjectionFingerprint, now_secs: f64) {
        if self.last_fingerprint != Some(fp) {
            self.last_fingerprint = Some(fp);
            self.last_change_secs = now_secs;
        }
    }

    /// True iff the camera has been at the most-recently-observed projection
    /// for at least `settle_window_secs`. Always true once any fingerprint
    /// has been observed *and* enough time has passed since the last change.
    pub fn is_settled(&self, now_secs: f64) -> bool {
        self.last_fingerprint.is_some()
            && (now_secs - self.last_change_secs) >= self.settle_window_secs
    }

    /// Seconds remaining until settle, given a wall-clock `now`. Returns
    /// `None` if already settled (or never observed).
    pub fn time_until_settle(&self, now_secs: f64) -> Option<f64> {
        let target = self.last_change_secs + self.settle_window_secs;
        if self.last_fingerprint.is_some() && now_secs < target {
            Some(target - now_secs)
        } else {
            None
        }
    }
}

/// Inputs that determine the previous-sweep search result. When any of these
/// change, the cached `find_prev_sweep` result must be recomputed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PrevSweepCacheKey {
    pub playback_ts_bits: u64,
    pub displayed_elev: u8,
    pub is_auto: bool,
    pub scan_count: usize,
}

/// Bundle of small caches used by the per-frame render path.
#[derive(Debug, Default, Clone)]
pub struct RenderCache {
    pub camera_motion: CameraMotion,

    /// Last value of `is_dark` pushed to `egui::Context::set_visuals`. Used
    /// to skip the per-frame `Visuals` reconstruction unless the theme
    /// actually changed.
    pub last_dark: Option<bool>,

    /// Cached key + result for `PlaybackManager::find_prev_sweep`. Reused
    /// across frames while the inputs are unchanged.
    pub prev_sweep_cache_key: Option<PrevSweepCacheKey>,
    pub prev_sweep_cache_value: Option<(f64, u8, f32, f64, f64)>,
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::geo::MapProjection;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Two projections centered at distinct lat/lon yield distinct fingerprints;
    /// the same center yields equal fingerprints. Pure math, no browser.
    fn fp(center_lat: f64, center_lon: f64) -> ProjectionFingerprint {
        MapProjection::new(center_lat, center_lon).fingerprint()
    }

    #[wasm_bindgen_test]
    fn settle_window_secs_constant() {
        assert!((SETTLE_WINDOW_SECS - 0.18).abs() < 1e-12);
    }

    #[wasm_bindgen_test]
    fn distinct_centers_produce_distinct_fingerprints() {
        assert_ne!(fp(39.0, -98.0), fp(40.0, -98.0));
        assert_eq!(fp(39.0, -98.0), fp(39.0, -98.0));
    }

    #[wasm_bindgen_test]
    fn default_camera_never_settled_until_observed() {
        let cm = CameraMotion::default();
        // No fingerprint observed yet: never settled regardless of elapsed time.
        assert!(!cm.is_settled(0.0));
        assert!(!cm.is_settled(100.0));
        // And no countdown is reported when nothing has been observed.
        assert_eq!(cm.time_until_settle(0.0), None);
        assert_eq!(cm.time_until_settle(100.0), None);
    }

    #[wasm_bindgen_test]
    fn observe_then_not_yet_settled_within_window() {
        let mut cm = CameraMotion::default();
        cm.observe(fp(39.0, -98.0), 10.0);
        // Only 0.1s elapsed (< 0.18 window): not settled.
        assert!(!cm.is_settled(10.1));
    }

    #[wasm_bindgen_test]
    fn observe_then_settled_after_window() {
        let mut cm = CameraMotion::default();
        cm.observe(fp(39.0, -98.0), 10.0);
        // Just past the window boundary settles (avoid the exact f64 boundary,
        // where (10.0 + 0.18) - 10.0 rounds a hair under 0.18).
        assert!(cm.is_settled(10.0 + SETTLE_WINDOW_SECS + 1e-6));
        // Comfortably past the window: settled.
        assert!(cm.is_settled(11.0));
    }

    #[wasm_bindgen_test]
    fn re_observing_same_fingerprint_does_not_reset_timer() {
        let mut cm = CameraMotion::default();
        cm.observe(fp(39.0, -98.0), 10.0);
        // Same fingerprint observed later must NOT push last_change forward.
        cm.observe(fp(39.0, -98.0), 10.5);
        // 10.0 + 0.18 = 10.18, so at 10.2 we are settled relative to original change.
        assert!(cm.is_settled(10.2));
    }

    #[wasm_bindgen_test]
    fn observing_new_fingerprint_resets_timer() {
        let mut cm = CameraMotion::default();
        cm.observe(fp(39.0, -98.0), 10.0);
        assert!(cm.is_settled(10.5));
        // A different projection resets the change time to 10.5.
        cm.observe(fp(41.0, -98.0), 10.5);
        // 10.6 is only 0.1s past the reset: not settled.
        assert!(!cm.is_settled(10.6));
        // 10.5 + 0.18 = 10.68: settled at 10.7.
        assert!(cm.is_settled(10.7));
    }

    #[wasm_bindgen_test]
    fn time_until_settle_counts_down() {
        let mut cm = CameraMotion::default();
        cm.observe(fp(39.0, -98.0), 10.0);
        // target = 10.18; at now=10.0 remaining is the full window.
        let remaining = cm.time_until_settle(10.0).expect("should be pending");
        assert!((remaining - SETTLE_WINDOW_SECS).abs() < 1e-9);
        // At now=10.1, remaining = 10.18 - 10.1 = 0.08.
        let remaining = cm.time_until_settle(10.1).expect("should be pending");
        assert!((remaining - 0.08).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn time_until_settle_none_once_settled() {
        let mut cm = CameraMotion::default();
        cm.observe(fp(39.0, -98.0), 10.0);
        // Past target (10.18): no countdown remaining.
        assert_eq!(cm.time_until_settle(10.5), None);
        // Exactly at target: now < target is false, so None.
        assert_eq!(cm.time_until_settle(10.0 + SETTLE_WINDOW_SECS), None);
    }

    #[wasm_bindgen_test]
    fn prev_sweep_cache_key_default_is_zeroed() {
        let k = PrevSweepCacheKey::default();
        assert_eq!(k.playback_ts_bits, 0);
        assert_eq!(k.displayed_elev, 0);
        assert!(!k.is_auto);
        assert_eq!(k.scan_count, 0);
    }

    #[wasm_bindgen_test]
    fn prev_sweep_cache_key_equality() {
        let a = PrevSweepCacheKey {
            playback_ts_bits: 42,
            displayed_elev: 3,
            is_auto: true,
            scan_count: 7,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[wasm_bindgen_test]
    fn prev_sweep_cache_key_inequality_each_field() {
        let base = PrevSweepCacheKey {
            playback_ts_bits: 42,
            displayed_elev: 3,
            is_auto: true,
            scan_count: 7,
        };
        assert_ne!(
            base,
            PrevSweepCacheKey {
                playback_ts_bits: 43,
                ..base.clone()
            }
        );
        assert_ne!(
            base,
            PrevSweepCacheKey {
                displayed_elev: 4,
                ..base.clone()
            }
        );
        assert_ne!(
            base,
            PrevSweepCacheKey {
                is_auto: false,
                ..base.clone()
            }
        );
        assert_ne!(
            base,
            PrevSweepCacheKey {
                scan_count: 8,
                ..base.clone()
            }
        );
    }

    #[wasm_bindgen_test]
    fn render_cache_default_is_empty() {
        let rc = RenderCache::default();
        assert_eq!(rc.last_dark, None);
        assert_eq!(rc.prev_sweep_cache_key, None);
        assert!(rc.prev_sweep_cache_value.is_none());
        // The embedded camera motion uses the default settle window and is
        // unsettled until something is observed.
        assert!(!rc.camera_motion.is_settled(1000.0));
    }

    #[wasm_bindgen_test]
    fn render_cache_holds_prev_sweep_value() {
        let mut rc = RenderCache::default();
        rc.last_dark = Some(true);
        rc.prev_sweep_cache_value = Some((1.0, 2u8, 3.0f32, 4.0, 5.0));
        assert_eq!(rc.last_dark, Some(true));
        let v = rc.prev_sweep_cache_value.expect("value set");
        assert!((v.0 - 1.0).abs() < 1e-12);
        assert_eq!(v.1, 2u8);
        assert!((v.2 - 3.0f32).abs() < 1e-6);
    }
}
