//! `TimelineView` — the single source-agnostic view the timeline UI renders
//! from, produced by one adapter that merges every timeline data source.
//!
//! Before this module the timeline had a *split-brain*: cached scans
//! (`RadarTimeline`) and the in-progress live volume (`VcpPositionModel`)
//! were drawn by different renderers that *filtered each other out* instead
//! of *merging*. A scan that was partially downloaded and then resumed live
//! was handed wholesale to the live overlay, which had no idea its cached
//! sweeps existed — so they vanished from the timeline until streaming
//! stopped. The old `TimelineModel` only did dedup-by-filtering
//! ("renderer, skip this one"); it never merged per sweep.
//!
//! `TimelineView` replaces that. It is built once per frame from the sources
//! passed by reference — the IDB cache, the archive shadow boundaries, the
//! live streaming state, and its projection — none of which reference each
//! other. The UI asks the view *availability* questions ("what's cached?
//! what's collecting? is this point already covered?") and never reaches
//! past it to a raw source. The cache↔live merge lives in exactly one place
//! ([`merge_cached_into_live`]), which makes the disappearing-sweep bug
//! structurally impossible: a streaming volume keeps its cached sweeps
//! ([`SweepAvailability::Cached`]) while its active cut reads as
//! [`SweepAvailability::Collecting`].
//!
//! Like its predecessor the view *borrows* the cache scans (the common,
//! large source) to avoid cloning them every frame, and *owns* only the
//! small, merged in-progress pieces (the live volume's position model and
//! overlay context). The borrowing keeps per-frame cost negligible while
//! still routing every read through one handle.

use crate::data::ScanKey;
use crate::nexrad::ScanBoundary;
use crate::state::radar_data::{RadarTimeline, Scan};
use crate::state::{LiveModeState, SweepStatus, SweepTiming, VcpPositionModel};
use std::collections::BTreeSet;

/// Tolerance for matching a download/shadow boundary against a cached scan,
/// in seconds. Wider than zero because the boundary comes from the archive
/// listing's filename time while the cached scan's `key_timestamp` comes
/// from the volume-header time parsed during ingest; the two can differ by a
/// few seconds without representing different volumes. 60s comfortably
/// covers normal NEXRAD volume cadence (~5–10 min) without absorbing
/// adjacent volumes. (Moved verbatim from the former `TimelineModel`.)
pub const BOUNDARY_MATCH_TOLERANCE_SECS: i64 = 60;

/// Tolerance for matching the download-completion flash against a cached
/// scan. Tighter than [`BOUNDARY_MATCH_TOLERANCE_SECS`] because the flash is
/// short-lived (~1s) and a wider band would let an unrelated scan trigger
/// the wrong flash position.
pub const COMPLETION_MATCH_TOLERANCE_SECS: i64 = 30;

/// Non-geometry inputs the realtime overlay needs that don't belong on the
/// position model (UI animation state, countdown). Built by the adapter so
/// the overlay never reads [`LiveModeState`] directly.
pub struct LiveOverlayContext {
    pub countdown_secs: Option<f64>,
    pub in_progress_radials: u32,
    /// Elevation numbers known to be in this volume — the live session's
    /// received cuts **unioned with any already-cached cuts**, so the
    /// "next sweep" placeholder logic stays consistent when resuming a
    /// partially-downloaded volume.
    pub elevations_received: Vec<u8>,
    pub in_progress_elevation: Option<u8>,
}

/// Frame-scoped, source-agnostic view of the timeline.
///
/// Borrows the cache scans; owns the merged in-progress volume + context.
pub struct TimelineView<'a> {
    cache: &'a RadarTimeline,
    shadows: &'a [ScanBoundary],
    /// Key-millis of the in-progress volume the overlay will draw. Excluded
    /// from the "settled" (cached-track) iterators so it isn't drawn twice.
    /// `None` when there is no live overlay (then the cached scan, if any,
    /// renders normally on the settled track).
    live_volume_ms: Option<i64>,
    /// The in-progress volume, with cached sweeps merged in. Owned.
    live_position: Option<VcpPositionModel>,
    live_ctx: Option<LiveOverlayContext>,
    /// Cached scan key-millis (plus the live volume) for O(log n) coverage
    /// queries by the shadow/ghost overlays.
    coverage_keys: BTreeSet<i64>,
    elevation_filter: Option<u8>,
    now_secs: f64,
}

impl<'a> TimelineView<'a> {
    /// Build the view from every timeline source. Sources are passed by
    /// reference and never reference each other; all merging happens here.
    ///
    /// `live_position` is the live subsystem's per-frame `from_live` model
    /// (`live.radar_model.position`); the adapter clones it and overlays the
    /// cached sweeps for the same volume.
    pub fn build(
        cache: &'a RadarTimeline,
        shadows: &'a [ScanBoundary],
        live_state: Option<&LiveModeState>,
        live_position: Option<&VcpPositionModel>,
        elevation_filter: Option<u8>,
        now_secs: f64,
    ) -> Self {
        let (live_position, live_ctx, live_volume_ms) = match (live_state, live_position) {
            (Some(ls), Some(pos)) if ls.is_active() => {
                let anchor_ms = ls.current_volume.as_ref().map(|a| a.scan_key.scan_start.0);
                let mut merged = pos.clone();
                let mut received = ls.elevations_received.clone();

                if let Some(ms) = anchor_ms {
                    if let Some(scan) = scan_with_key_ms(cache, ms) {
                        merge_cached_into_live(&mut merged, scan);
                        for sw in &scan.sweeps {
                            if !received.contains(&sw.elevation_number) {
                                received.push(sw.elevation_number);
                            }
                        }
                    }
                }
                received.sort_unstable();

                let ctx = LiveOverlayContext {
                    countdown_secs: ls.countdown_remaining_secs(now_secs),
                    in_progress_radials: ls.current_in_progress_radials.unwrap_or(0),
                    elevations_received: received,
                    in_progress_elevation: ls.current_in_progress_elevation,
                };
                (Some(merged), Some(ctx), anchor_ms)
            }
            _ => (None, None, None),
        };

        // Coverage set: every cached scan key, plus the live volume (which is
        // also "data we have / are getting" for ghost/shadow suppression).
        let mut coverage_keys: BTreeSet<i64> = cache
            .scans
            .iter()
            .map(|s| (s.key_timestamp * 1000.0).round() as i64)
            .collect();
        if let Some(ms) = live_volume_ms {
            coverage_keys.insert(ms);
        }

        Self {
            cache,
            shadows,
            live_volume_ms,
            live_position,
            live_ctx,
            coverage_keys,
            elevation_filter,
            now_secs,
        }
    }

    /// Cached ("settled") scans overlapping `[start, end]`, excluding the
    /// in-progress volume (the realtime overlay owns that). These carry
    /// [`crate::state::SweepAvailability::Cached`] availability. Borrows tie
    /// to the underlying cache (`'a`), not to the view, so callers can return
    /// them past the view's own scope.
    pub fn settled_scans_in_range(&self, start: f64, end: f64) -> impl Iterator<Item = &'a Scan> {
        let live_ms = self.live_volume_ms;
        self.cache
            .scans_in_visual_range(start, end)
            .filter(move |s| !is_live_scan(s, live_ms))
    }

    /// The cached scan at `ts`, excluding the in-progress volume.
    pub fn settled_scan_at(&self, ts: f64) -> Option<&'a Scan> {
        let scan = self.cache.find_scan_at_timestamp(ts)?;
        (!is_live_scan(scan, self.live_volume_ms)).then_some(scan)
    }

    /// The full cache handle (for callers that need range bounds or sweep
    /// lookups across all cached scans, e.g. the mobile scrubber). Prefer
    /// the availability-oriented accessors above where possible.
    pub fn cache(&self) -> &'a RadarTimeline {
        self.cache
    }

    /// Archive shadow boundaries (scans known to exist but not downloaded).
    pub fn shadow_boundaries(&self) -> &'a [ScanBoundary] {
        self.shadows
    }

    /// The in-progress volume, with cached sweeps merged in. `None` when not
    /// streaming (or when the VCP isn't known yet and no overlay is drawn).
    pub fn live_volume(&self) -> Option<&VcpPositionModel> {
        self.live_position.as_ref()
    }

    /// UI/animation context for the realtime overlay.
    pub fn live_context(&self) -> Option<&LiveOverlayContext> {
        self.live_ctx.as_ref()
    }

    /// Whether a download-ghost or shadow-boundary at `start_secs` is already
    /// covered by cached data (or the in-progress volume), within
    /// [`BOUNDARY_MATCH_TOLERANCE_SECS`]. Overlays use this to suppress
    /// markers for ranges already represented by a filled block.
    pub fn is_covered_by_cached(&self, start_secs: i64) -> bool {
        let lo = (start_secs - BOUNDARY_MATCH_TOLERANCE_SECS).saturating_mul(1000);
        let hi = (start_secs + BOUNDARY_MATCH_TOLERANCE_SECS).saturating_mul(1000);
        self.coverage_keys.range(lo..=hi).next().is_some()
    }

    /// Resolve the download-completion flash to its target cached scan's
    /// `(start, end)` bounds, within [`COMPLETION_MATCH_TOLERANCE_SECS`].
    pub fn completion_target(&self, scan_start_secs: i64) -> Option<(f64, f64)> {
        self.cache
            .scans
            .iter()
            .find(|s| {
                let key_secs = (s.key_timestamp).round() as i64;
                (key_secs - scan_start_secs).abs() <= COMPLETION_MATCH_TOLERANCE_SECS
            })
            .map(|s| (s.start_time, s.end_time))
    }

    pub fn elevation_filter(&self) -> Option<u8> {
        self.elevation_filter
    }

    #[allow(dead_code)] // Carried for consumers that need the frame's canonical `now`.
    pub fn now_secs(&self) -> f64 {
        self.now_secs
    }
}

/// Whether `scan` is the in-progress live volume identified by `live_ms`.
fn is_live_scan(scan: &Scan, live_ms: Option<i64>) -> bool {
    match live_ms {
        Some(ms) => (scan.key_timestamp * 1000.0).round() as i64 == ms,
        None => false,
    }
}

/// Find the cached scan whose key matches `key_ms` (exact, rounded millis).
fn scan_with_key_ms(cache: &RadarTimeline, key_ms: i64) -> Option<&Scan> {
    cache
        .scans
        .iter()
        .find(|s| (s.key_timestamp * 1000.0).round() as i64 == key_ms)
}

/// Overlay the already-cached sweeps of a volume onto its in-progress
/// position model — **the fix for the disappearing-sweep bug**.
///
/// For every cached sweep whose elevation is not already `Complete` in the
/// live model, mark it `Complete` with the cached observed start/end times.
/// The live model's own freshly-decoded sweeps win (they already carry
/// sub-second radial timing for this session), so cached data only fills the
/// elevations the live session hasn't produced yet. Idempotent and pure.
pub fn merge_cached_into_live(position: &mut VcpPositionModel, cached: &Scan) {
    for sw in &cached.sweeps {
        if let Some(p) = position
            .sweeps
            .iter_mut()
            .find(|p| p.elevation_number == sw.elevation_number)
        {
            if p.status != SweepStatus::Complete {
                p.status = SweepStatus::Complete;
                p.timing = SweepTiming::Observed;
                p.start = sw.start_time;
                p.end = sw.end_time;
                p.chunks.clear();
            }
        }
    }
}

/// Convenience: the key-millis a [`ScanKey`] resolves to (mirrors how the
/// adapter and renderers round `key_timestamp`).
#[allow(dead_code)]
pub fn scan_key_ms(key: &ScanKey) -> i64 {
    key.scan_start.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::radar_data::Sweep;
    use crate::state::vcp_position::{SweepPosition, SweepStatus, SweepTiming};
    use crate::state::SweepAvailability;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep_pos(elev: u8, status: SweepStatus) -> SweepPosition {
        SweepPosition {
            elevation_number: elev,
            elevation_angle: 0.5 * elev as f32,
            start: 0.0,
            end: 0.0,
            timing: SweepTiming::Estimated,
            status,
            chunks: Vec::new(),
        }
    }

    fn live_model(sweeps: Vec<SweepPosition>) -> VcpPositionModel {
        VcpPositionModel {
            vcp_number: 212,
            volume_start: 1_700_000_000.0,
            volume_end: 1_700_000_300.0,
            complete: false,
            scan_key: None,
            sweeps,
            extrapolation: None,
            next_volume_ghost: None,
        }
    }

    fn cached_sweep(elev: u8, start: f64, end: f64) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: 0.5 * elev as f32,
            elevation_number: elev,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: Vec::new(),
        }
    }

    fn cached_scan(key_secs: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: key_secs,
            end_time: key_secs + 300.0,
            key_timestamp: key_secs,
            vcp: 212,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    /// The bug: a resumed volume's cached cuts must survive while streaming.
    #[wasm_bindgen_test]
    fn merge_keeps_cached_sweeps_while_streaming() {
        // Live session has only seen elev 2 (in progress); elevs 1 & 3 are
        // future. The volume already has elevs 1 & 3 cached from a prefetch.
        let mut pos = live_model(vec![
            sweep_pos(1, SweepStatus::Future),
            sweep_pos(
                2,
                SweepStatus::InProgress {
                    radials_received: 100,
                    chunks_received: 1,
                    chunks_expected: Some(3),
                },
            ),
            sweep_pos(3, SweepStatus::Future),
        ]);
        let scan = cached_scan(
            1_700_000_000.0,
            vec![
                cached_sweep(1, 1_700_000_001.0, 1_700_000_030.0),
                cached_sweep(3, 1_700_000_060.0, 1_700_000_090.0),
            ],
        );

        merge_cached_into_live(&mut pos, &scan);

        // Cached cuts now render as Cached with their observed times.
        let s1 = &pos.sweeps[0];
        assert_eq!(s1.availability(), SweepAvailability::Cached);
        assert_eq!(s1.timing, SweepTiming::Observed);
        assert_eq!(s1.start, 1_700_000_001.0);
        assert_eq!(s1.end, 1_700_000_030.0);

        // The in-progress cut is untouched (session data wins).
        assert_eq!(pos.sweeps[1].availability(), SweepAvailability::Collecting);

        let s3 = &pos.sweeps[2];
        assert_eq!(s3.availability(), SweepAvailability::Cached);
        assert_eq!(s3.start, 1_700_000_060.0);

        assert_eq!(pos.completed_count(), 2);
    }

    /// Session-decoded sweeps are authoritative; the merge never clobbers a
    /// cut the live session already completed.
    #[wasm_bindgen_test]
    fn merge_does_not_clobber_session_complete() {
        let mut pos = live_model(vec![{
            let mut p = sweep_pos(1, SweepStatus::Complete);
            p.timing = SweepTiming::Observed;
            p.start = 999.0;
            p.end = 1099.0;
            p
        }]);
        let scan = cached_scan(0.0, vec![cached_sweep(1, 1.0, 2.0)]);
        merge_cached_into_live(&mut pos, &scan);
        // Session times preserved, not overwritten by the cached (1.0, 2.0).
        assert_eq!(pos.sweeps[0].start, 999.0);
        assert_eq!(pos.sweeps[0].end, 1099.0);
    }

    /// `is_covered_by_cached` matches the old 60s tolerance band.
    #[wasm_bindgen_test]
    fn covered_by_cached_uses_60s_band() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(1_700_000_000.0, Vec::new())],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, 1_700_000_000.0);

        assert!(view.is_covered_by_cached(1_700_000_000));
        assert!(view.is_covered_by_cached(1_700_000_059));
        assert!(view.is_covered_by_cached(1_700_000_000 - 60));
        assert!(!view.is_covered_by_cached(1_700_000_061));
        assert!(!view.is_covered_by_cached(1_700_000_000 - 61));
    }

    /// Without a live anchor the cached scan stays on the settled track.
    #[wasm_bindgen_test]
    fn settled_includes_scan_when_not_live() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(1_700_000_000.0, Vec::new())],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, 1_700_000_000.0);
        let n = view
            .settled_scans_in_range(1_699_999_000.0, 1_700_001_000.0)
            .count();
        assert_eq!(n, 1);
        assert!(view.live_volume().is_none());
    }

    /// `completion_target` resolves a scan within the 30s band and returns
    /// its bounds.
    #[wasm_bindgen_test]
    fn completion_target_uses_30s_band() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(1_700_000_000.0, Vec::new())],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, 1_700_000_000.0);

        assert_eq!(
            view.completion_target(1_700_000_029),
            Some((1_700_000_000.0, 1_700_000_300.0))
        );
        assert_eq!(view.completion_target(1_700_000_031), None);
    }
}
