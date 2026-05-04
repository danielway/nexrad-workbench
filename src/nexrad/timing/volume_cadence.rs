//! EWMA tracker for the average wall-clock duration between volume rollovers.
//!
//! Used by the realtime streaming loop to (a) feed the volume-discovery
//! prediction step on the *next* session start and (b) act as a prior for
//! cold-start triangulation when the cached hint is stale or missing.
//!
//! A volume "rollover" event is observed each time a Start chunk
//! (`ChunkType::Start`) is processed by the streaming loop — that's the
//! definitive signal that the active volume index has advanced. Wall-clock
//! delta between consecutive rollover observations is one volume duration
//! sample.

/// EWMA estimate of volume duration in seconds.
///
/// Persisted into the `VolumeHint` cache so the next session warms with
/// the prior site's observed cadence rather than restarting at the default.
#[derive(Debug, Clone)]
pub struct VolumeCadenceTracker {
    ewma_secs: f64,
    last_rollover_ms: Option<i64>,
}

impl VolumeCadenceTracker {
    /// Smoothing factor. 0.3 lets the EWMA track a real cadence change
    /// (e.g. VCP swap from clear-air ~10 min to precipitation ~4.5 min)
    /// within ~5 volumes while still rejecting one-off outliers.
    const ALPHA: f64 = 0.3;

    /// Sanity-clamp range. Any observed delta outside this range is
    /// rejected (likely a clock skew or a multi-volume gap from a paused
    /// session resumed mid-stream, neither of which is a useful cadence
    /// sample).
    const MIN_SECS: f64 = 60.0;
    const MAX_SECS: f64 = 900.0;

    /// Default cadence used when no prior is available. ~5 minutes is the
    /// midpoint of typical NEXRAD VCP volume durations.
    pub const DEFAULT_SECS: f64 = 300.0;

    pub fn new(seed_secs: f64) -> Self {
        Self {
            ewma_secs: seed_secs.clamp(Self::MIN_SECS, Self::MAX_SECS),
            last_rollover_ms: None,
        }
    }

    /// Record a Start-chunk observation at wall-clock `now_ms`. The first
    /// call only stores the timestamp; subsequent calls compute the delta
    /// against the previous rollover and feed it into the EWMA.
    pub fn record_rollover(&mut self, now_ms: i64) {
        if let Some(prev_ms) = self.last_rollover_ms {
            let delta_secs = ((now_ms - prev_ms) as f64) / 1000.0;
            if (Self::MIN_SECS..=Self::MAX_SECS).contains(&delta_secs) {
                self.ewma_secs = Self::ALPHA * delta_secs + (1.0 - Self::ALPHA) * self.ewma_secs;
            }
        }
        self.last_rollover_ms = Some(now_ms);
    }

    pub fn current_secs(&self) -> f64 {
        self.ewma_secs
    }

    /// Replace the EWMA with a seed value (e.g. recovered from cache).
    /// Does not affect `last_rollover_ms` — seeding mid-session continues
    /// to fold new samples in.
    pub fn seed(&mut self, secs: f64) {
        self.ewma_secs = secs.clamp(Self::MIN_SECS, Self::MAX_SECS);
    }
}

impl Default for VolumeCadenceTracker {
    fn default() -> Self {
        Self::new(Self::DEFAULT_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_rollover_only_stores_timestamp() {
        let mut t = VolumeCadenceTracker::new(300.0);
        t.record_rollover(1_000_000);
        assert_eq!(t.current_secs(), 300.0);
    }

    #[test]
    fn second_rollover_blends_into_ewma() {
        let mut t = VolumeCadenceTracker::new(300.0);
        t.record_rollover(0);
        t.record_rollover(270_000); // 270s delta
                                    // 0.3 * 270 + 0.7 * 300 = 81 + 210 = 291
        assert!((t.current_secs() - 291.0).abs() < 0.01);
    }

    #[test]
    fn out_of_range_deltas_are_rejected() {
        let mut t = VolumeCadenceTracker::new(300.0);
        t.record_rollover(0);
        t.record_rollover(30_000); // 30s — below MIN
        assert_eq!(t.current_secs(), 300.0);
        t.record_rollover(30_000 + 1_200_000); // 1200s — above MAX
        assert_eq!(t.current_secs(), 300.0);
    }

    #[test]
    fn seed_clamps_to_range() {
        let mut t = VolumeCadenceTracker::default();
        t.seed(10.0);
        assert_eq!(t.current_secs(), VolumeCadenceTracker::MIN_SECS);
        t.seed(2_000.0);
        assert_eq!(t.current_secs(), VolumeCadenceTracker::MAX_SECS);
    }
}
