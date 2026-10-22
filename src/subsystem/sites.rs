//! Sites: the radars on screen, every one the same kind of thing.
//!
//! [`Sites`] holds one [`SiteState`] per active radar and a `focus` index.
//! A `SiteState` owns everything that differs between radars: the site's id
//! and position, its scan inventory, its render track and the crossfade and
//! scrub caches in front of it. Everything shared (playback,
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
use crate::core::{RadarTimeline, ScanBoundary};
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
        }
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

    #[wasm_bindgen_test]
    fn default_is_the_first_visit_site() {
        let sites = Sites::default();
        assert_eq!(sites.focus().id, SiteId::new("KDMX"));
        assert!((sites.focus().lat - 41.7312).abs() < 1e-9);
        assert!((sites.focus().lon + 93.7229).abs() < 1e-9);
    }
}
