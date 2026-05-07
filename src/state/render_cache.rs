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
