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
use crate::nexrad::projection::{ScanProjection, SweepProjectionStatus, SweepTimingProvenance};
use crate::nexrad::ScanBoundary;
use crate::state::radar_data::{RadarTimeline, Scan};
use crate::state::LiveModeState;
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
    /// Whether the plan's immediate next download target lives in the *next*
    /// volume (the active filter has no remaining current-volume match). When
    /// true, the "next chunk" countdown belongs to the next-volume ghost, not
    /// a current-volume future sweep.
    pub next_target_in_next_volume: bool,
    /// Elevation number of that next target, used to highlight the matching
    /// sweep in the next-volume ghost.
    pub next_target_elevation: Option<u8>,
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
    live_position: Option<ScanProjection>,
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
        live_position: Option<&ScanProjection>,
        live_plan: Option<&crate::nexrad::projection::Projection>,
        elevation_filter: Option<u8>,
        now_secs: f64,
    ) -> Self {
        let (live_position, live_ctx, live_volume_ms) = match (live_state, live_position) {
            (Some(ls), Some(pos)) if ls.is_active() => {
                let anchor_ms = ls.current_volume.as_ref().map(|a| a.scan_key.scan_start.0);
                let mut merged = pos.clone();
                let mut received = pos.roster.received.clone();

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
                    countdown_secs: if ls.phase == crate::state::LivePhase::WaitingForChunk {
                        live_plan.and_then(|p| p.next_available_in_secs(now_secs))
                    } else {
                        None
                    },
                    in_progress_radials: pos.in_progress_radials.unwrap_or(0),
                    elevations_received: received,
                    in_progress_elevation: pos.in_progress_elevation,
                    next_target_in_next_volume: live_plan
                        .is_some_and(|p| p.next_target_in_next_volume()),
                    next_target_elevation: live_plan.and_then(|p| p.next_target_elevation()),
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

    /// Every cached scan whose *displayed* block intersects `[start, end]`,
    /// paired with that block's clamped right edge (see [`clamped_display_end`]).
    /// Includes the in-progress volume's cached scan — connector lines want it;
    /// [`Self::settled_scans_in_range`] filters it back out for the settled
    /// track. This is the single source of the displayed right edge, so block
    /// fills, connector lines, and visual-range culling all agree on one value.
    pub fn visual_scans_in_range(
        &self,
        start: f64,
        end: f64,
    ) -> impl Iterator<Item = (&'a Scan, f64)> {
        let scans: &'a [Scan] = &self.cache.scans;
        let shadows: &'a [ScanBoundary] = self.shadows;
        scans.iter().enumerate().filter_map(move |(i, scan)| {
            let clamped_end = clamped_display_end(scans, shadows, i);
            (clamped_end >= start && scan.start_time <= end).then_some((scan, clamped_end))
        })
    }

    /// Cached ("settled") scans overlapping `[start, end]`, excluding the
    /// in-progress volume (the realtime overlay owns that). These carry
    /// [`crate::state::SweepAvailability::Cached`] availability. Borrows tie
    /// to the underlying cache (`'a`), not to the view, so callers can return
    /// them past the view's own scope. Each item carries the clamped display
    /// end from [`Self::visual_scans_in_range`].
    pub fn settled_scans_in_range(
        &self,
        start: f64,
        end: f64,
    ) -> impl Iterator<Item = (&'a Scan, f64)> {
        let live_ms = self.live_volume_ms;
        self.visual_scans_in_range(start, end)
            .filter(move |(s, _)| !is_live_scan(s, live_ms))
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
    pub fn live_volume(&self) -> Option<&ScanProjection> {
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

/// The displayed right edge of the scan at `scans[i]`.
///
/// Prefers the **authoritative** archive extent: when the scan matches a known
/// archive boundary (by `key_timestamp`, within [`BOUNDARY_MATCH_TOLERANCE_SECS`])
/// that boundary's `end` is the real start time of the *next* volume, so the
/// block ends exactly there. This caps sparse/incomplete scans against the
/// following volume even when that volume is only ghosted (not downloaded) —
/// the VCP-projected extent alone can overrun several ghosted scans.
///
/// With no matching boundary (e.g. no archive listing fetched, as during pure
/// live streaming) it falls back to the VCP projection
/// ([`Scan::display_end_time`]), capped at the next *downloaded* scan's start so
/// sparse blocks still can't overrun a neighbor we do have. Never truncates
/// real collected data: the result is floored at the scan's own `end_time`.
fn clamped_display_end(scans: &[Scan], shadows: &[ScanBoundary], i: usize) -> f64 {
    let scan = &scans[i];
    if let Some(b) = shadows.iter().find(|b| {
        (b.start as f64 - scan.key_timestamp).abs() <= BOUNDARY_MATCH_TOLERANCE_SECS as f64
    }) {
        return (b.end as f64).max(scan.end_time);
    }
    let de = scan.display_end_time();
    scans.get(i + 1).map_or(de, |next| de.min(next.start_time))
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
pub fn merge_cached_into_live(position: &mut ScanProjection, cached: &Scan) {
    for sw in &cached.sweeps {
        if let Some(p) = position
            .sweeps
            .iter_mut()
            .find(|p| p.elevation_number == sw.elevation_number)
        {
            if p.status != SweepProjectionStatus::CollectedByUs {
                p.status = SweepProjectionStatus::CollectedByUs;
                p.timing = SweepTimingProvenance::Observed;
                p.collection_start_secs = sw.start_time;
                p.collection_end_secs = sw.end_time;
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
    use crate::nexrad::projection::{
        ProjectionScanRole, SweepAvailability, SweepProjection, SweepProjectionStatus,
        SweepTimingProvenance,
    };
    use crate::state::radar_data::Sweep;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep_pos(elev: u8, status: SweepProjectionStatus) -> SweepProjection {
        SweepProjection {
            elevation_number: elev,
            elevation_angle: 0.5 * elev as f32,
            scan_role: ProjectionScanRole::CurrentInProgress,
            status,
            timing: SweepTimingProvenance::Estimated,
            collection_start_secs: 0.0,
            collection_end_secs: 0.0,
            chunks_in_sweep: 0,
            chunks_received: 0,
            radials_received: 0,
            azimuth_rate_dps: 0.0,
            chunks: Vec::new(),
        }
    }

    fn live_model(sweeps: Vec<SweepProjection>) -> ScanProjection {
        ScanProjection {
            vcp_number: 212,
            vcp_pattern: None,
            roster: crate::state::VolumeElevationRoster::default(),
            in_progress_elevation: None,
            in_progress_radials: None,
            volume_start: 1_700_000_000.0,
            volume_end: 1_700_000_300.0,
            complete: false,
            scan_key: None,
            sweeps,
            extrapolation: None,
            next_scan_ghost: None,
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
            sweep_pos(1, SweepProjectionStatus::FutureExpected),
            sweep_pos(2, SweepProjectionStatus::InProgress),
            sweep_pos(3, SweepProjectionStatus::FutureExpected),
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
        assert_eq!(s1.timing, SweepTimingProvenance::Observed);
        assert_eq!(s1.collection_start_secs, 1_700_000_001.0);
        assert_eq!(s1.collection_end_secs, 1_700_000_030.0);

        // The in-progress cut is untouched (session data wins).
        assert_eq!(pos.sweeps[1].availability(), SweepAvailability::Collecting);

        let s3 = &pos.sweeps[2];
        assert_eq!(s3.availability(), SweepAvailability::Cached);
        assert_eq!(s3.collection_start_secs, 1_700_000_060.0);

        assert_eq!(pos.completed_count(), 2);
    }

    /// Session-decoded sweeps are authoritative; the merge never clobbers a
    /// cut the live session already completed.
    #[wasm_bindgen_test]
    fn merge_does_not_clobber_session_complete() {
        let mut pos = live_model(vec![{
            let mut p = sweep_pos(1, SweepProjectionStatus::CollectedByUs);
            p.timing = SweepTimingProvenance::Observed;
            p.collection_start_secs = 999.0;
            p.collection_end_secs = 1099.0;
            p
        }]);
        let scan = cached_scan(0.0, vec![cached_sweep(1, 1.0, 2.0)]);
        merge_cached_into_live(&mut pos, &scan);
        // Session times preserved, not overwritten by the cached (1.0, 2.0).
        assert_eq!(pos.sweeps[0].collection_start_secs, 999.0);
        assert_eq!(pos.sweeps[0].collection_end_secs, 1099.0);
    }

    /// `is_covered_by_cached` matches the old 60s tolerance band.
    #[wasm_bindgen_test]
    fn covered_by_cached_uses_60s_band() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(1_700_000_000.0, Vec::new())],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, None, 1_700_000_000.0);

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
        let view = TimelineView::build(&cache, &shadows, None, None, None, None, 1_700_000_000.0);
        let n = view
            .settled_scans_in_range(1_699_999_000.0, 1_700_001_000.0)
            .count();
        assert_eq!(n, 1);
        assert!(view.live_volume().is_none());
    }

    /// A scan matching a known archive boundary ends exactly at that
    /// boundary's `end` (the next volume's real start) — even when the scan is
    /// sparse and its VCP projection would overrun the following ghosted scan.
    #[wasm_bindgen_test]
    fn visual_extent_uses_authoritative_archive_boundary() {
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};

        // Sparse scan at key 1000: real data [1000, 1010], VCP projects 120s
        // (→ 1120), but the archive says the next volume starts at 1060.
        let mut sparse = cached_scan(1000.0, Vec::new());
        sparse.start_time = 1000.0;
        sparse.end_time = 1010.0;
        sparse.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle: 0.5,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(3.0),
            }],
        });
        let cache = RadarTimeline {
            scans: vec![sparse],
        };
        // Archive listing knows scans at 1000 and 1060 (only 1000 downloaded).
        let shadows = vec![
            ScanBoundary {
                start: 1000,
                end: 1060,
            },
            ScanBoundary {
                start: 1060,
                end: 1120,
            },
        ];
        let view = TimelineView::build(&cache, &shadows, None, None, None, None, 1000.0);

        let pairs: Vec<_> = view.visual_scans_in_range(0.0, 5000.0).collect();
        assert_eq!(pairs.len(), 1);
        // Clamped to the archive boundary end (1060), not the VCP projection (1120).
        assert_eq!(pairs[0].1, 1060.0);
    }

    /// With no archive listing, the projection is capped at the next
    /// downloaded scan's start, and culling uses that clamped extent.
    #[wasm_bindgen_test]
    fn visual_extent_falls_back_to_next_downloaded_scan() {
        use crate::data::keys::{ExtractedVcp, ExtractedVcpElevation};

        let mut sparse = cached_scan(1000.0, Vec::new());
        sparse.start_time = 1000.0;
        sparse.end_time = 1010.0;
        sparse.vcp_pattern = Some(ExtractedVcp {
            number: 215,
            elevations: vec![ExtractedVcpElevation {
                angle: 0.5,
                waveform: "CS".to_string(),
                prf_number: 1,
                is_sails: false,
                is_mrle: false,
                is_base_tilt: false,
                azimuth_rate: Some(3.0),
            }],
        });
        let mut next = cached_scan(1060.0, Vec::new());
        next.start_time = 1060.0;
        next.end_time = 1100.0;
        next.vcp_pattern = None;
        let cache = RadarTimeline {
            scans: vec![sparse, next],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, None, 1000.0);

        let pairs: Vec<_> = view.visual_scans_in_range(0.0, 5000.0).collect();
        assert_eq!(pairs.len(), 2);
        // Sparse scan's 1120 projection capped at next downloaded start (1060).
        assert_eq!(pairs[0].1, 1060.0);
        // Last scan, no VCP: display end == end_time.
        assert_eq!(pairs[1].1, 1100.0);

        // Culling uses the clamped extent: a window past the clamp drops it.
        assert_eq!(view.visual_scans_in_range(1065.0, 5000.0).count(), 1);
    }

    /// `completion_target` resolves a scan within the 30s band and returns
    /// its bounds.
    #[wasm_bindgen_test]
    fn completion_target_uses_30s_band() {
        let cache = RadarTimeline {
            scans: vec![cached_scan(1_700_000_000.0, Vec::new())],
        };
        let shadows: Vec<ScanBoundary> = Vec::new();
        let view = TimelineView::build(&cache, &shadows, None, None, None, None, 1_700_000_000.0);

        assert_eq!(
            view.completion_target(1_700_000_029),
            Some((1_700_000_000.0, 1_700_000_300.0))
        );
        assert_eq!(view.completion_target(1_700_000_031), None);
    }
}
