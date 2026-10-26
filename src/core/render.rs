//! Pure render-dispatch decisions: prefetch targeting and request dedup.
//!
//! The GPU upload and worker `postMessage` are irreducibly shell (the P4
//! carve-out: worker dispatch is async + stateful). What's *pure* is the
//! deciding — which sweep to prefetch next, and whether a request duplicates the
//! last one — so those live here and are unit-tested. The prev-sweep texture
//! decision already uses this pattern
//! ([`crate::core::playback_manager::PrevSweepAction`]); this module covers the
//! main-sweep prefetch + dedup gates.

use crate::core::{Scan, SweepIdentity};
use crate::data::ScanKey;

/// Parameters for a volume (all-elevations) render request. Single-elevation
/// requests are identified by [`SweepIdentity`] so the dedup cache shares one
/// type with the on-GPU `displayed` slot; volume requests span elevations.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VolumeRenderRequest {
    pub scan_key: ScanKey,
    pub product: String,
}

/// One site's render tracking: the scan it is rendering, the elevations that
/// scan can render, and the last requests sent for it (the dedup inputs).
///
/// The worker pool is shared between sites
/// ([`crate::nexrad::RenderCoordinator`]); this is the part that is per site,
/// held on [`crate::subsystem::sites::SiteState::render`].
#[derive(Clone, Debug, Default)]
pub(crate) struct RenderTrack {
    /// The scan being rendered. Typed rather than its storage-key string so
    /// callers compare by value; the string is built at the worker boundary.
    scan_key: Option<ScanKey>,
    /// Elevations the GPU can render right now for that scan, from worker
    /// ingest results in either the live or archive flow. Distinct from
    /// [`crate::core::VolumeElevationRoster::received`], which is the
    /// in-progress live volume's observation roster.
    available_elevations: Vec<u8>,
    /// The last single-elevation request, for dedup. Equal identities mean
    /// the same on-disk sweep and the request is suppressed.
    last_render: Option<SweepIdentity>,
    /// The last volume request, for dedup.
    last_volume_render: Option<VolumeRenderRequest>,
}

impl RenderTrack {
    /// The scan being rendered, if any.
    pub(crate) fn scan_key(&self) -> Option<&ScanKey> {
        self.scan_key.as_ref()
    }

    /// Elevation numbers the current scan can render.
    pub(crate) fn available_elevations(&self) -> &[u8] {
        &self.available_elevations
    }

    /// Set the scan and its elevations (after ingest).
    pub(crate) fn set_scan(&mut self, key: ScanKey, elevations: Vec<u8>) {
        self.scan_key = Some(key);
        self.available_elevations = elevations;
    }

    /// Set just the scan (during a scrub or a chunk ingest).
    pub(crate) fn set_scan_key(&mut self, key: ScanKey) {
        self.scan_key = Some(key);
    }

    /// Add newly completed elevations (chunk ingest), kept sorted and unique.
    pub(crate) fn add_elevations(&mut self, new: &[u8]) {
        for &elev in new {
            if !self.available_elevations.contains(&elev) {
                self.available_elevations.push(elev);
                self.available_elevations.sort_unstable();
            }
        }
    }

    /// Forget everything: the site changed.
    pub(crate) fn clear_for_site_change(&mut self) {
        *self = Self::default();
    }

    /// Let the next requests through (clears the dedup cache).
    pub(crate) fn force_fresh_render(&mut self) {
        self.last_render = None;
        self.last_volume_render = None;
    }

    /// Forget the last volume request, so the same volume can be asked for
    /// again (its texture was given back).
    pub(crate) fn forget_volume_render(&mut self) {
        self.last_volume_render = None;
    }

    /// Clear the scan (no scan in range); the elevations stay.
    pub(crate) fn clear_scan_key(&mut self) {
        self.scan_key = None;
        self.last_render = None;
    }

    /// The last single-elevation request.
    pub(crate) fn last_render(&self) -> Option<&SweepIdentity> {
        self.last_render.as_ref()
    }

    /// Record a single-elevation request as sent (also used by the prefetch,
    /// so the same request is not sent again).
    pub(crate) fn set_last_render(&mut self, identity: SweepIdentity) {
        self.last_render = Some(identity);
    }

    /// The last volume request.
    pub(crate) fn last_volume_render(&self) -> Option<&VolumeRenderRequest> {
        self.last_volume_render.as_ref()
    }

    /// Record a volume request as sent.
    pub(crate) fn set_last_volume_render(&mut self, request: VolumeRenderRequest) {
        self.last_volume_render = Some(request);
    }

    /// The dedup gate for a single-elevation request: whether `identity`
    /// differs from the last one sent for this site.
    pub(crate) fn admits_sweep(&self, identity: &SweepIdentity) -> bool {
        should_dispatch(identity, self.last_render())
    }

    /// Whether a volume reply for `scan_key` / `product` is still wanted:
    /// it answers the last volume request sent for this site. With none on
    /// record (the texture was given back) it is taken, as before.
    pub(crate) fn wants_volume(&self, scan_key: &ScanKey, product: &str) -> bool {
        self.last_volume_render
            .as_ref()
            .is_none_or(|r| r.scan_key == *scan_key && r.product == product)
    }

    /// The dedup gate for a volume request.
    pub(crate) fn admits_volume(&self, request: &VolumeRenderRequest) -> bool {
        should_dispatch(request, self.last_volume_render())
    }
}

/// Decide which elevation number (if any) to prefetch as the playhead nears the
/// end of the current sweep, or `None` to prefetch nothing.
///
/// Pure extraction of the prefetch block in `advance_playback`:
/// 1. Only prefetch when the playhead is within `prefetch_lookahead` of the
///    current sweep's end (and not past it): `0 < end - ts < lookahead`.
/// 2. The next elevation is the next sweep in the same scan, else the first
///    sweep of the scan covering `ts + lookahead` (`future_scan`).
/// 3. Skip if that elevation is already displayed (`cur_elev`) — no churn.
///
/// `future_scan` need only be supplied when the current sweep is the last in its
/// scan; the caller may pass `None` otherwise (it's ignored when an in-scan next
/// sweep exists).
pub(crate) fn decide_prefetch_next_elevation(
    scan: &Scan,
    sweep_idx: usize,
    sweep_end_time: f64,
    playback_ts: f64,
    prefetch_lookahead: f64,
    future_scan: Option<&Scan>,
    cur_elev: Option<u8>,
) -> Option<u8> {
    let time_to_end = sweep_end_time - playback_ts;
    if !(time_to_end > 0.0 && time_to_end < prefetch_lookahead) {
        return None;
    }

    let next_elev_num = if sweep_idx + 1 < scan.sweeps.len() {
        Some(scan.sweeps[sweep_idx + 1].elevation_number)
    } else {
        future_scan.and_then(|s| s.sweeps.first().map(|sw| sw.elevation_number))
    };

    match next_elev_num {
        Some(next_en) if cur_elev != Some(next_en) => Some(next_en),
        _ => None,
    }
}

/// Whether a render request should be dispatched, i.e. it is *not* a duplicate of
/// the last dispatched request. Structural equality — equal identities mean the
/// same on-disk sweep (or volume), so the request is suppressed.
///
/// This is the dedup gate `RenderCoordinator` applies before `worker.render(...)`
/// / `worker.render_volume(...)`. Keeping it a tiny pure function makes the
/// "identical request is suppressed; a changed one goes through" rule assertable
/// without a worker.
pub(crate) fn should_dispatch<T: PartialEq>(new: &T, last: Option<&T>) -> bool {
    last != Some(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Scan, Sweep};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep(elev_num: u8, start: f64, end: f64) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: elev_num as f32 * 0.5,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: Vec::new(),
        }
    }

    fn scan(key_ts: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: key_ts,
            end_time: key_ts + 300.0,
            key_timestamp: key_ts,
            vcp: 215,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    #[wasm_bindgen_test]
    fn no_prefetch_when_not_near_boundary() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1010.0, 1020.0)],
        );
        // ts=1002, end=1010 → time_to_end=8, lookahead=2 → not near.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1002.0, 2.0, None, Some(1)),
            None
        );
    }

    #[wasm_bindgen_test]
    fn no_prefetch_past_boundary() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1010.0, 1020.0)],
        );
        // ts past end → time_to_end <= 0.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1011.0, 5.0, None, Some(1)),
            None
        );
    }

    #[wasm_bindgen_test]
    fn prefetches_next_sweep_in_scan() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1010.0, 1020.0)],
        );
        // ts=1009, end=1010 → time_to_end=1 < lookahead 2 → next elev = 2.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1009.0, 2.0, None, Some(1)),
            Some(2)
        );
    }

    #[wasm_bindgen_test]
    fn skips_prefetch_when_next_is_current() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1010.0, 1020.0)],
        );
        // next would be elev 2, but it's already displayed → None.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1009.0, 2.0, None, Some(2)),
            None
        );
    }

    #[wasm_bindgen_test]
    fn falls_through_to_future_scan_for_last_sweep() {
        let s = scan(1000.0, vec![sweep(1, 1000.0, 1010.0)]);
        let next = scan(1010.0, vec![sweep(5, 1010.0, 1020.0)]);
        // Last sweep in scan → use the future scan's first sweep (elev 5).
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1009.0, 2.0, Some(&next), Some(1)),
            Some(5)
        );
        // No future scan → nothing to prefetch.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1009.0, 2.0, None, Some(1)),
            None
        );
    }

    #[wasm_bindgen_test]
    fn dedup_suppresses_identical_and_passes_changed() {
        assert!(!should_dispatch(&"a", Some(&"a"))); // identical → suppress
        assert!(should_dispatch(&"a", Some(&"b"))); // changed → dispatch
        assert!(should_dispatch(&"a", None)); // nothing prior → dispatch
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::{Scan, Sweep};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sweep(elev_num: u8, start: f64, end: f64) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: elev_num as f32 * 0.5,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: Vec::new(),
        }
    }

    fn scan(key_ts: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: key_ts,
            end_time: key_ts + 300.0,
            key_timestamp: key_ts,
            vcp: 215,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    // --- decide_prefetch_next_elevation boundary conditions ---

    // The window guard is strict: `time_to_end > 0 && time_to_end < lookahead`.
    // Exactly at the lookahead boundary (time_to_end == lookahead) is NOT inside.
    #[wasm_bindgen_test]
    fn no_prefetch_exactly_at_lookahead_boundary() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1010.0, 1020.0)],
        );
        // end=1010, ts=1008 → time_to_end=2.0 == lookahead 2.0 → excluded.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1008.0, 2.0, None, Some(1)),
            None
        );
    }

    // Just inside the upper boundary (time_to_end slightly below lookahead) → prefetch.
    #[wasm_bindgen_test]
    fn prefetch_just_inside_lookahead_boundary() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1010.0, 1020.0)],
        );
        // end=1010, ts=1008.001 → time_to_end ≈ 1.999 < 2.0 → in window → next elev 2.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1008.001, 2.0, None, Some(1)),
            Some(2)
        );
    }

    // Exactly at the sweep end (time_to_end == 0) is excluded by the strict `> 0`.
    #[wasm_bindgen_test]
    fn no_prefetch_exactly_at_sweep_end() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1010.0, 1020.0)],
        );
        // ts == end → time_to_end == 0.0 → excluded.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1010.0, 5.0, None, Some(1)),
            None
        );
    }

    // --- cur_elev = None: nothing currently displayed, so always take next ---

    #[wasm_bindgen_test]
    fn prefetches_next_when_cur_elev_is_none() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(2, 1010.0, 1020.0)],
        );
        // cur_elev None != Some(2) → dispatch next elev 2.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1009.0, 2.0, None, None),
            Some(2)
        );
    }

    // --- in-scan next sweep takes precedence; future_scan is ignored ---

    #[wasm_bindgen_test]
    fn future_scan_ignored_when_in_scan_next_exists() {
        let s = scan(
            1000.0,
            vec![sweep(1, 1000.0, 1010.0), sweep(7, 1010.0, 1020.0)],
        );
        let future = scan(1010.0, vec![sweep(9, 1010.0, 1020.0)]);
        // sweep_idx 0 is not the last sweep, so the in-scan next (elev 7) wins,
        // never the future scan's elev 9.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1009.0, 2.0, Some(&future), Some(1)),
            Some(7)
        );
    }

    // --- future scan with no sweeps → first() is None → no prefetch ---

    #[wasm_bindgen_test]
    fn no_prefetch_when_future_scan_empty() {
        let s = scan(1000.0, vec![sweep(1, 1000.0, 1010.0)]);
        let empty_future = scan(1010.0, Vec::new());
        // last sweep in scan, future scan has no sweeps → next_elev_num None → None.
        assert_eq!(
            decide_prefetch_next_elevation(
                &s,
                0,
                1010.0,
                1009.0,
                2.0,
                Some(&empty_future),
                Some(1)
            ),
            None
        );
    }

    // --- future scan's first sweep equals cur_elev → suppressed (no churn) ---

    #[wasm_bindgen_test]
    fn future_scan_first_equals_cur_elev_suppressed() {
        let s = scan(1000.0, vec![sweep(3, 1000.0, 1010.0)]);
        let future = scan(1010.0, vec![sweep(3, 1010.0, 1020.0)]);
        // future first elev 3 == cur_elev Some(3) → no churn → None.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1009.0, 2.0, Some(&future), Some(3)),
            None
        );
    }

    // Confirms the in-scan path also de-churns when next equals cur (distinct from
    // the existing test by using a non-adjacent elevation numbering).
    #[wasm_bindgen_test]
    fn in_scan_next_equals_cur_elev_suppressed() {
        let s = scan(
            1000.0,
            vec![sweep(4, 1000.0, 1010.0), sweep(6, 1010.0, 1020.0)],
        );
        // next would be elev 6, but cur_elev is already 6 → None.
        assert_eq!(
            decide_prefetch_next_elevation(&s, 0, 1010.0, 1009.0, 2.0, None, Some(6)),
            None
        );
    }

    // --- should_dispatch with non-string PartialEq types ---

    #[wasm_bindgen_test]
    fn dispatch_gate_on_tuple_identity() {
        // Same-valued (but distinct) tuples are structurally equal → suppress.
        let new = (3u8, "ref".to_string());
        let last = (3u8, "ref".to_string());
        assert!(!should_dispatch(&new, Some(&last)));

        // Differ in the numeric field only → dispatch.
        let changed = (4u8, "ref".to_string());
        assert!(should_dispatch(&new, Some(&changed)));

        // Differ in the string field only → dispatch.
        let changed_str = (3u8, "vel".to_string());
        assert!(should_dispatch(&new, Some(&changed_str)));
    }

    #[wasm_bindgen_test]
    fn dispatch_gate_on_integers() {
        assert!(should_dispatch(&42i32, None)); // no prior → dispatch
        assert!(!should_dispatch(&42i32, Some(&42i32))); // equal → suppress
        assert!(should_dispatch(&42i32, Some(&7i32))); // changed → dispatch
    }
}

#[cfg(test)]
mod track_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn key(site: &str, ms: i64) -> ScanKey {
        ScanKey::new(site, crate::data::keys::UnixMillis(ms))
    }

    fn identity(site: &str, ms: i64, elev: u8, product: &str) -> SweepIdentity {
        SweepIdentity::new(key(site, ms), elev, product)
    }

    fn volume(site: &str, ms: i64, product: &str) -> VolumeRenderRequest {
        VolumeRenderRequest {
            scan_key: key(site, ms),
            product: product.to_string(),
        }
    }

    #[wasm_bindgen_test]
    fn default_is_empty() {
        let t = RenderTrack::default();
        assert!(t.scan_key().is_none());
        assert!(t.available_elevations().is_empty());
        assert!(t.last_render().is_none());
        assert!(t.last_volume_render().is_none());
    }

    #[wasm_bindgen_test]
    fn set_scan_sets_key_and_elevations() {
        let mut t = RenderTrack::default();
        t.set_scan(key("KDMX", 1_700_000_000_000), vec![1, 2, 3]);
        assert_eq!(t.scan_key(), Some(&key("KDMX", 1_700_000_000_000)));
        assert_eq!(t.available_elevations(), &[1, 2, 3]);
    }

    #[wasm_bindgen_test]
    fn set_scan_key_only_leaves_elevations_untouched() {
        let mut t = RenderTrack::default();
        t.set_scan(key("KDMX", 100), vec![4, 5]);
        t.set_scan_key(key("KFTG", 200));
        assert_eq!(t.scan_key(), Some(&key("KFTG", 200)));
        assert_eq!(t.available_elevations(), &[4, 5]);
    }

    #[wasm_bindgen_test]
    fn set_scan_overwrites_prior_elevations() {
        let mut t = RenderTrack::default();
        t.set_scan(key("KDMX", 1), vec![1, 2, 3]);
        t.set_scan(key("KDMX", 2), vec![9]);
        assert_eq!(t.scan_key(), Some(&key("KDMX", 2)));
        assert_eq!(t.available_elevations(), &[9]);
    }

    #[wasm_bindgen_test]
    fn add_elevations_keeps_sorted_unique() {
        let mut t = RenderTrack::default();
        t.add_elevations(&[5, 3, 9]);
        assert_eq!(t.available_elevations(), &[3, 5, 9]);
        t.add_elevations(&[9, 1, 5]);
        assert_eq!(t.available_elevations(), &[1, 3, 5, 9]);
        t.add_elevations(&[1]);
        assert_eq!(t.available_elevations(), &[1, 3, 5, 9]);
    }

    #[wasm_bindgen_test]
    fn add_elevations_merges_into_existing_sorted() {
        let mut t = RenderTrack::default();
        t.set_scan(key("KABC", 1), vec![2, 6]);
        t.add_elevations(&[4]);
        assert_eq!(t.available_elevations(), &[2, 4, 6]);
    }

    #[wasm_bindgen_test]
    fn clear_for_site_change_resets_everything() {
        let mut t = RenderTrack::default();
        t.set_scan(key("KDMX", 5), vec![1, 2]);
        t.set_last_render(identity("KDMX", 5, 1, "ref"));
        t.set_last_volume_render(volume("KDMX", 5, "ref"));
        t.clear_for_site_change();
        assert!(t.scan_key().is_none());
        assert!(t.available_elevations().is_empty());
        assert!(t.last_render().is_none());
        assert!(t.last_volume_render().is_none());
    }

    #[wasm_bindgen_test]
    fn clear_scan_key_clears_key_and_last_render_but_keeps_elevations() {
        let mut t = RenderTrack::default();
        t.set_scan(key("KDMX", 5), vec![1, 2]);
        t.set_last_render(identity("KDMX", 5, 1, "ref"));
        t.set_last_volume_render(volume("KDMX", 5, "ref"));
        t.clear_scan_key();
        assert!(t.scan_key().is_none());
        assert_eq!(t.available_elevations(), &[1, 2]);
        assert!(t.last_render().is_none());
        // The volume dedup survives: only a forced render clears it.
        assert!(t.last_volume_render().is_some());
    }

    #[wasm_bindgen_test]
    fn force_fresh_render_clears_dedup_but_keeps_the_scan() {
        let mut t = RenderTrack::default();
        t.set_scan(key("KDMX", 1), vec![3]);
        t.set_last_render(identity("KDMX", 1, 3, "ref"));
        t.set_last_volume_render(volume("KDMX", 1, "ref"));
        t.force_fresh_render();
        assert_eq!(t.scan_key(), Some(&key("KDMX", 1)));
        assert_eq!(t.available_elevations(), &[3]);
        assert!(t.admits_sweep(&identity("KDMX", 1, 3, "ref")));
        assert!(t.admits_volume(&volume("KDMX", 1, "ref")));
    }

    #[wasm_bindgen_test]
    fn admits_sweep_suppresses_the_last_identity_only() {
        let mut t = RenderTrack::default();
        assert!(t.admits_sweep(&identity("KDMX", 1, 1, "ref")));
        t.set_last_render(identity("KDMX", 1, 1, "ref"));
        assert!(!t.admits_sweep(&identity("KDMX", 1, 1, "ref")));
        assert!(t.admits_sweep(&identity("KDMX", 1, 2, "ref")));
        assert!(t.admits_sweep(&identity("KDMX", 1, 1, "vel")));
        assert!(t.admits_sweep(&identity("KTLX", 1, 1, "ref")));
    }

    #[wasm_bindgen_test]
    fn admits_volume_suppresses_the_last_request_only() {
        let mut t = RenderTrack::default();
        assert!(t.admits_volume(&volume("KDMX", 1, "ref")));
        t.set_last_volume_render(volume("KDMX", 1, "ref"));
        assert!(!t.admits_volume(&volume("KDMX", 1, "ref")));
        assert!(t.admits_volume(&volume("KDMX", 2, "ref")));
        assert!(t.admits_volume(&volume("KDMX", 1, "vel")));
    }

    #[wasm_bindgen_test]
    fn a_volume_reply_is_wanted_only_for_the_last_request() {
        let mut t = RenderTrack::default();
        // Nothing on record: taken.
        assert!(t.wants_volume(&key("KDMX", 1), "ref"));
        t.set_last_volume_render(volume("KDMX", 1, "ref"));
        t.set_last_volume_render(volume("KDMX", 2, "ref"));
        // The reply for the volume asked for first arrives late.
        assert!(!t.wants_volume(&key("KDMX", 1), "ref"));
        assert!(!t.wants_volume(&key("KDMX", 2), "vel"));
        assert!(t.wants_volume(&key("KDMX", 2), "ref"));
    }

    #[wasm_bindgen_test]
    fn two_tracks_dedup_independently() {
        // One per site: a request recorded on one never suppresses the other.
        let mut a = RenderTrack::default();
        let b = RenderTrack::default();
        a.set_last_render(identity("KDMX", 1, 1, "ref"));
        assert!(!a.admits_sweep(&identity("KDMX", 1, 1, "ref")));
        assert!(b.admits_sweep(&identity("KDMX", 1, 1, "ref")));
    }
}
