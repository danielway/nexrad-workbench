//! Sites: the radars on screen, every one the same kind of thing.
//!
//! [`Sites`] holds one [`SiteState`] per active radar and a `focus` index.
//! A `SiteState` owns everything that differs between radars: the site's id
//! and position, its scan inventory, its render track and the crossfade and
//! scrub caches in front of it, the frame on its canvas and how old it is,
//! and its storm cells. Everything shared (playback,
//! selection, settings, the worker pool, the download queue, the archive
//! index, the activity book) stays with its own owner.
//!
//! `focus` only says which site the single-valued surfaces describe: the tilt
//! ladder, the frame readout, the age pill, the timeline's full track, the
//! overview's storm bars, the calendar and the preferences. Per-frame shells
//! loop over the list instead of reaching for the focus. The
//! single-site app is a list of one with `focus == 0`.
//!
//! The list is never empty and `focus` is always one of its indices
//! ([`crate::core::sites::focus_valid`]); nothing adds or removes sites yet.

use crate::core::playback_manager::PlaybackManager;
use crate::core::render::RenderTrack;
use crate::core::render_loop::ScrubCache;
use crate::core::storm_cells::StormCells;
use crate::core::{DisplayedSweep, RadarTimeline, ScanBoundary};
use crate::data::SiteId;

/// Everything the app holds for one radar.
pub(crate) struct SiteState {
    /// The radar's ICAO id (upper case, as the store keys it).
    pub id: SiteId,
    /// The radar's latitude. Also the map's centre until the map has one of
    /// its own (see `ARCHITECTURE.md`, State Management).
    pub lat: f64,
    /// The radar's longitude (see [`Self::lat`]).
    pub lon: f64,
    /// Scans held on the device plus their sweep listings. Rebuilt by the
    /// cache load and grown by worker ingest.
    pub scans: RadarTimeline,
    /// Per-scan time boundaries from the archive listing, drawn on the
    /// timeline before the scans are downloaded. Cleared on a site change.
    pub shadows: Vec<ScanBoundary>,
    /// The scan this site is rendering, its renderable elevations and the
    /// dedup inputs for its worker requests. The worker pool itself is
    /// shared ([`crate::nexrad::RenderCoordinator`]).
    pub render: RenderTrack,
    /// Decoded sweeps kept for the crossfade under-layer, and the pending
    /// previous-sweep request.
    pub crossfade: PlaybackManager,
    /// Scrub detection for this site's active scan.
    pub scrub: ScrubCache,
    /// What is actually on this site's GPU main-slot texture. Set only after
    /// a successful `update_data()` in `handle_decoded_outcome` /
    /// `handle_live_decoded_outcome`; cleared when the canvas blanks. The
    /// single source of truth for the timeline active border, the canvas
    /// overlay text and the staleness counters.
    pub displayed: Option<DisplayedSweep>,
    /// What is on this site's GPU prev-sweep texture (the under-layer for
    /// sweep animation). Written by `sync_prev_sweep_texture` in archive
    /// mode and by the promote branch of `handle_live_decoded_outcome` in
    /// live mode, so it reflects what the prev-sweep slot actually holds,
    /// not the prior main upload.
    pub previous_displayed: Option<DisplayedSweep>,
    /// Age of the displayed sweep's newest radial (its end), in seconds
    /// against the wall clock; `None` for a frame in the future.
    pub data_staleness_secs: Option<f64>,
    /// Age of the displayed sweep's oldest radial (its start).
    pub data_staleness_start_secs: Option<f64>,
    /// The last storm cell detection for this site and the sweep and
    /// threshold it was computed for (see `crate::core::storm_cells`). The
    /// visibility flag and threshold are global settings on `VizState`.
    pub cells: StormCells,
}

impl SiteState {
    /// A site at `(lat, lon)` with nothing loaded yet.
    pub(crate) fn new(id: impl Into<SiteId>, lat: f64, lon: f64) -> Self {
        Self {
            id: id.into(),
            lat,
            lon,
            scans: RadarTimeline::default(),
            shadows: Vec::new(),
            render: RenderTrack::default(),
            crossfade: PlaybackManager::new(),
            scrub: ScrubCache::default(),
            displayed: None,
            previous_displayed: None,
            data_staleness_secs: None,
            data_staleness_start_secs: None,
            cells: StormCells::default(),
        }
    }

    /// Set the staleness counters from a sweep spanning `start..end` (Unix
    /// seconds) at wall-clock `now_secs`. A sweep in the future reads `None`.
    pub(crate) fn set_staleness(&mut self, start: f64, end: f64, now_secs: f64) {
        let end_age = now_secs - end;
        let start_age = now_secs - start;
        self.data_staleness_secs = (end_age >= 0.0).then_some(end_age);
        self.data_staleness_start_secs = (start_age >= 0.0).then_some(start_age);
    }

    /// Recompute staleness from the displayed sweep against `now_secs` (per
    /// frame, so archive data shows its true age). Leaves the counters alone
    /// when nothing is displayed.
    pub(crate) fn refresh_staleness(&mut self, now_secs: f64) {
        if let Some((start, end)) = self.displayed.as_ref().map(|d| (d.start_time, d.end_time)) {
            self.set_staleness(start, end, now_secs);
        }
    }

    /// Forget the frame on this site's canvas: what the GPU slots held, the
    /// staleness counters and the storm cells drawn over it.
    pub(crate) fn clear_display(&mut self) {
        self.data_staleness_secs = None;
        self.data_staleness_start_secs = None;
        self.displayed = None;
        self.previous_displayed = None;
        self.cells.clear();
    }

    /// Representative (midpoint) collection time of the on-screen frame, in
    /// Unix seconds, or `None` when the canvas holds no frame. This is the raw
    /// time the primary readout and overlay format live each frame — never
    /// the playhead (the canvas-honesty invariant).
    #[cfg(test)]
    pub(crate) fn displayed_midpoint_secs(&self) -> Option<f64> {
        self.displayed
            .as_ref()
            .map(|d| (d.start_time + d.end_time) / 2.0)
    }

    /// Point this element at another radar. The caller tears down what was
    /// tied to the old one (see `app::frame_setup::sync_to_active_site`).
    pub(crate) fn retarget(&mut self, id: impl Into<SiteId>, lat: f64, lon: f64) {
        self.id = id.into();
        self.lat = lat;
        self.lon = lon;
    }
}

impl Default for SiteState {
    /// The site a first visit starts on before one is picked (Des Moines).
    fn default() -> Self {
        Self::new("KDMX", 41.7312, -93.7229)
    }
}

/// The active sites and which one the single-valued surfaces describe.
pub(crate) struct Sites {
    /// One element per active radar; never empty.
    pub list: Vec<SiteState>,
    /// Index into `list` of the site the single-valued surfaces describe.
    pub focus: usize,
}

impl Sites {
    /// A list of one, focused.
    pub(crate) fn new(initial: SiteState) -> Self {
        Self {
            list: vec![initial],
            focus: 0,
        }
    }

    /// The site the single-valued surfaces describe.
    pub(crate) fn focus(&self) -> &SiteState {
        debug_assert!(crate::core::sites::focus_valid(self.focus, self.list.len()));
        &self.list[self.focus]
    }

    /// Mutable [`Self::focus`].
    pub(crate) fn focus_mut(&mut self) -> &mut SiteState {
        debug_assert!(crate::core::sites::focus_valid(self.focus, self.list.len()));
        &mut self.list[self.focus]
    }

    /// The ids in list order, for [`crate::core::sites::route`].
    pub(crate) fn ids(&self) -> Vec<SiteId> {
        self.list.iter().map(|s| s.id.clone()).collect()
    }
}

impl Default for Sites {
    fn default() -> Self {
        Self::new(SiteState::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn two_sites() -> Sites {
        let mut sites = Sites::new(SiteState::new("KDMX", 41.73, -93.72));
        sites.list.push(SiteState::new("KTLX", 35.33, -97.28));
        sites
    }

    #[wasm_bindgen_test]
    fn new_is_a_focused_list_of_one() {
        let sites = Sites::new(SiteState::new("KTLX", 35.33, -97.28));
        assert_eq!(sites.list.len(), 1);
        assert_eq!(sites.focus, 0);
        assert_eq!(sites.focus().id, SiteId::new("KTLX"));
        assert!((sites.focus().lat - 35.33).abs() < 1e-9);
        assert!(sites.focus().scans.scans.is_empty());
        assert!(sites.focus().shadows.is_empty());
    }

    #[wasm_bindgen_test]
    fn focus_reads_the_focused_element() {
        let mut sites = two_sites();
        assert_eq!(sites.focus().id, SiteId::new("KDMX"));
        sites.focus = 1;
        assert_eq!(sites.focus().id, SiteId::new("KTLX"));
    }

    #[wasm_bindgen_test]
    fn focus_mut_writes_only_the_focused_element() {
        let mut sites = two_sites();
        sites.focus = 1;
        sites.focus_mut().retarget("KFWS", 32.57, -97.30);
        assert_eq!(sites.list[0].id, SiteId::new("KDMX"));
        assert_eq!(sites.list[1].id, SiteId::new("KFWS"));
        assert!((sites.list[1].lon + 97.30).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn ids_follow_list_order() {
        let sites = two_sites();
        assert_eq!(sites.ids(), vec![SiteId::new("KDMX"), SiteId::new("KTLX")]);
    }

    fn sweep(start: f64, end: f64) -> DisplayedSweep {
        DisplayedSweep {
            identity: crate::core::SweepIdentity::new(
                crate::data::ScanKey::new("KDMX", crate::data::keys::UnixMillis(1000)),
                1,
                "reflectivity",
            ),
            start_time: start,
            end_time: end,
            elevation_deg: 0.5,
        }
    }

    #[wasm_bindgen_test]
    fn set_staleness_ages_both_ends_of_the_sweep() {
        let mut site = SiteState::default();
        // now=300, end=200 -> 100s stale; start=100 -> 200s stale.
        site.set_staleness(100.0, 200.0, 300.0);
        assert!((site.data_staleness_secs.unwrap() - 100.0).abs() < 1e-6);
        assert!((site.data_staleness_start_secs.unwrap() - 200.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn set_staleness_clamps_future_frames_to_none() {
        let mut site = SiteState::default();
        // now precedes both start and end -> negative staleness is None.
        site.set_staleness(500.0, 600.0, 400.0);
        assert_eq!(site.data_staleness_secs, None);
        assert_eq!(site.data_staleness_start_secs, None);
    }

    #[wasm_bindgen_test]
    fn refresh_staleness_reads_the_displayed_sweep_or_leaves_it() {
        let mut site = SiteState::default();
        site.set_staleness(0.0, 10.0, 20.0);
        // Nothing displayed: the counters are left as they were.
        site.refresh_staleness(1_000.0);
        assert!((site.data_staleness_secs.unwrap() - 10.0).abs() < 1e-6);
        site.displayed = Some(sweep(100.0, 250.0));
        site.refresh_staleness(300.0);
        assert!((site.data_staleness_secs.unwrap() - 50.0).abs() < 1e-6);
        assert!((site.data_staleness_start_secs.unwrap() - 200.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn clear_display_forgets_the_frame() {
        let mut site = SiteState::default();
        site.displayed = Some(sweep(100.0, 250.0));
        site.previous_displayed = Some(sweep(0.0, 100.0));
        site.set_staleness(100.0, 250.0, 300.0);
        site.clear_display();
        assert!(site.displayed.is_none());
        assert!(site.previous_displayed.is_none());
        assert_eq!(site.data_staleness_secs, None);
        assert_eq!(site.data_staleness_start_secs, None);
    }

    #[wasm_bindgen_test]
    fn displayed_midpoint_secs_none_then_some() {
        let mut site = SiteState::default();
        assert_eq!(site.displayed_midpoint_secs(), None);
        site.displayed = Some(sweep(100.0, 250.0));
        assert!((site.displayed_midpoint_secs().unwrap() - 175.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn default_is_the_first_visit_site() {
        let sites = Sites::default();
        assert_eq!(sites.focus().id, SiteId::new("KDMX"));
        assert!((sites.focus().lat - 41.7312).abs() < 1e-9);
        assert!((sites.focus().lon + 93.7229).abs() < 1e-9);
    }
}
