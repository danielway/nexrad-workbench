//! Sites: the radars on screen, every one the same kind of thing.
//!
//! [`Sites`] holds one [`SiteState`] per active radar and a `focus` index.
//! A `SiteState` owns everything that differs between radars: the site's id
//! and position, its scan inventory, its render track and the crossfade and
//! scrub caches in front of it, the frame on its canvas and how old it is,
//! its storm cells, the GPU renderers that draw it ([`SiteGpu`]), the
//! request ledger, retry set and prefetch settle its pumps fetch through, and
//! its live stream ([`SiteLive`]: the realtime channel, the volume in
//! progress, the projection engine and the per-frame live models).
//! Everything shared (playback, selection, settings, the worker pool, the
//! download queue, the archive index, the activity book, the live session's
//! phase and view-model on [`crate::subsystem::Live`]) stays with its own
//! owner.
//!
//! `focus` only says which site the single-valued surfaces describe: the tilt
//! ladder, the frame readout, the age pill, the timeline's full track, the
//! overview's storm bars, the calendar and the preferences. Per-frame shells
//! loop over the list instead of reaching for the focus. The
//! single-site app is a list of one with `focus == 0`.
//!
//! The list is never empty and `focus` is always one of its indices
//! ([`crate::core::sites::focus_valid`]). Sites join and leave through the
//! decisions in [`crate::core::sites`], carried out by [`Sites::apply`].

use crate::core::acquisition::{IngestRetries, PrefetchSettle, RequestLedger};
use crate::core::playback_manager::PlaybackManager;
use crate::core::render::RenderTrack;
use crate::core::render_loop::ScrubCache;
use crate::core::sites::Change;
use crate::core::storm_cells::{SceneCellKey, StormCells};
use crate::core::storm_tracks::StormTracks;
use crate::core::{DisplayedSweep, RadarTimeline, ScanBoundary};
use crate::data::SiteId;
use crate::nexrad::{GlobeRadarRenderer, RadarGpuRenderer, VolumeRayRenderer};
use crate::subsystem::SiteLive;
use std::sync::{Arc, Mutex};

/// One site's GPU renderers: the flat disc, its projection onto the globe
/// and the volume ray-march. Each holds that site's textures, so two sites
/// never share one. The GL context and the site-independent renderers (the
/// globe sphere, geographic lines) stay on `GpuResources`.
#[derive(Default)]
pub(crate) struct SiteGpu {
    /// 2D radar disc; also owns the colour table the 3D paths read.
    pub radar: Option<Arc<Mutex<RadarGpuRenderer>>>,
    pub globe_radar: Option<Arc<Mutex<GlobeRadarRenderer>>>,
    pub volume_ray: Option<Arc<Mutex<VolumeRayRenderer>>>,
    /// `RadarGpuRenderer::new` failed once; don't retry (and re-log) each
    /// frame.
    radar_failed: bool,
}

impl SiteGpu {
    /// Delete this site's GL objects (textures, buffers, programs). Called
    /// when the site leaves the map, before its `SiteState` is dropped.
    pub(crate) fn release(&mut self, gl: &glow::Context) {
        if let Some(r) = self.radar.take() {
            if let Ok(r) = r.lock() {
                r.release(gl);
            }
        }
        if let Some(r) = self.globe_radar.take() {
            if let Ok(r) = r.lock() {
                r.release(gl);
            }
        }
        if let Some(r) = self.volume_ray.take() {
            if let Ok(r) = r.lock() {
                r.release(gl);
            }
        }
    }

    /// Give back this site's volume texture, when it holds one. Returns
    /// whether it did, so the caller can let the next volume request
    /// through once the site is back inside the budget.
    pub(crate) fn release_volume(&mut self, gl: &glow::Context) -> bool {
        let Some(Ok(mut r)) = self.volume_ray.as_ref().map(|r| r.lock()) else {
            return false;
        };
        let held = r.texture_bytes() > 0 || r.has_data();
        if held {
            r.clear_volume(gl);
        }
        held
    }

    /// Bytes this site's volume texture takes; 0 when it holds none.
    pub(crate) fn volume_bytes(&self) -> u64 {
        self.volume_ray
            .as_ref()
            .and_then(|r| r.lock().ok().map(|r| r.texture_bytes()))
            .unwrap_or(0)
    }

    /// Create whichever renderers this site does not have yet. Cheap once
    /// they exist, so the frame loop calls it for every site.
    pub(crate) fn ensure(&mut self, gl: &Arc<glow::Context>) {
        if self.radar.is_none() && !self.radar_failed {
            match RadarGpuRenderer::new(gl) {
                Ok(r) => self.radar = Some(Arc::new(Mutex::new(r))),
                Err(e) => {
                    log::error!("Failed to create GPU radar renderer: {}", e);
                    self.radar_failed = true;
                }
            }
        }
        if self.globe_radar.is_none() {
            self.globe_radar = Some(Arc::new(Mutex::new(GlobeRadarRenderer::new(gl))));
        }
        if self.volume_ray.is_none() {
            self.volume_ray = Some(Arc::new(Mutex::new(VolumeRayRenderer::new(gl))));
        }
    }
}

/// Everything the app holds for one radar.
pub(crate) struct SiteState {
    /// The radar's ICAO id (upper case, as the store keys it).
    pub id: SiteId,
    /// The radar's latitude. The map's centre is its own value
    /// (`VizState::map_centre`).
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
    /// This site's renderers; empty until [`SiteGpu::ensure`] runs with a GL
    /// context.
    pub gpu: SiteGpu,
    /// Scans recently requested for this site: the dedup input that
    /// survives queue clears and bridges the ingest→timeline-refresh
    /// blackout (see [`RequestLedger`]). Keyed by scan start alone, which is
    /// enough within one site.
    pub ledger: RequestLedger,
    /// This site's scans that have had their one quiet re-download after a
    /// truncated archive (see [`IngestRetries`]).
    pub retries: IngestRetries,
    /// Debounce/idempotency state for this site's reactive prefetch (see
    /// [`PrefetchSettle`]).
    pub prefetch_settle: PrefetchSettle,
    /// This site's live stream: its channel, the volume it is collecting,
    /// its projection engine and this frame's live models. Whether the app
    /// is streaming at all is the session's ([`crate::subsystem::Live`]).
    pub live: SiteLive,
    /// What the render loop knows about this site's frame and the playhead
    /// this frame, the step bringing its volume included: the canvas
    /// caption reads it.
    pub caption: crate::core::canvas::CaptionContext,
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
            gpu: SiteGpu::default(),
            ledger: RequestLedger::default(),
            retries: IngestRetries::default(),
            prefetch_settle: PrefetchSettle::default(),
            live: SiteLive::default(),
            caption: crate::core::canvas::CaptionContext::default(),
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

    /// The timeline view of this site's scans, listings and, while the
    /// session `streaming`, the volume its stream is collecting.
    pub(crate) fn timeline_view(&self, streaming: bool) -> crate::core::TimelineView<'_> {
        crate::core::TimelineView::build(
            &self.scans,
            &self.shadows,
            streaming.then_some(&self.live.volume),
            self.live.radar_model.position.as_ref(),
        )
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

/// The scene's storm cells: while more than one site is shown, cells are
/// detected once on the composite grid of every shown site
/// (`crate::core::storm_cells::decide_scene`) and tracked with one letter
/// pool, so a storm two radars see is one storm. The per-site results are
/// not asked for meanwhile; with one site this stays empty and the focus's
/// own cells and `VizState::storm_tracks` are used.
#[derive(Default)]
pub(crate) struct SceneCells {
    pub cells: StormCells<SceneCellKey>,
    pub tracks: StormTracks<SceneCellKey>,
    /// The last sync took the scene path: switching paths restarts the
    /// tracker the other path leaves.
    pub active: bool,
}

impl SceneCells {
    /// Forget the result, cache, request in flight and tracks.
    pub(crate) fn reset(&mut self) {
        self.cells.reset();
        self.tracks.reset();
    }
}

/// The active sites and which one the single-valued surfaces describe.
pub(crate) struct Sites {
    /// One element per active radar; never empty.
    pub list: Vec<SiteState>,
    /// Index into `list` of the site the single-valued surfaces describe.
    pub focus: usize,
    /// The storm cells of the whole scene, beside the per-site ones.
    pub scene: SceneCells,
}

impl Sites {
    /// A list of one, focused.
    pub(crate) fn new(initial: SiteState) -> Self {
        Self {
            list: vec![initial],
            focus: 0,
            scene: SceneCells::default(),
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

    /// Every site, in list order.
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, SiteState> {
        self.list.iter()
    }

    /// Mutable [`Self::iter`].
    pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, SiteState> {
        self.list.iter_mut()
    }

    /// Carry out a change [`crate::core::sites`] decided. `added` is the
    /// element a [`Change::Add`] pushes. Returns the elements that left the
    /// list, for the shell to stop their streams and release their GPU
    /// objects.
    pub(crate) fn apply(&mut self, change: Change, added: Option<SiteState>) -> Vec<SiteState> {
        match change {
            Change::Add => {
                self.list.extend(added);
                Vec::new()
            }
            Change::Remove { index, focus } => {
                let removed = self.list.remove(index);
                self.focus = focus;
                vec![removed]
            }
            Change::Focus { index } => {
                self.focus = index;
                Vec::new()
            }
            Change::Only { keep } => {
                let kept = self.list.remove(keep);
                let removed = std::mem::replace(&mut self.list, vec![kept]);
                self.focus = 0;
                removed
            }
        }
    }

    /// The sites the canvas draws, in [`crate::core::sites::draw_order`]:
    /// older frame first, so the newer frame is on top.
    pub(crate) fn drawn(&self) -> impl Iterator<Item = &SiteState> {
        let times: Vec<Option<f64>> = self
            .iter()
            .map(|s| {
                s.displayed
                    .as_ref()
                    .map(|d| (d.start_time + d.end_time) / 2.0)
            })
            .collect();
        crate::core::sites::draw_order(&times)
            .into_iter()
            .map(move |i| &self.list[i])
    }

    /// The site set as the picker reads it: every site's id, frame age and
    /// stream health, the focus, and the layout's cap. `phase` and
    /// `last_exit` are the live session's.
    pub(crate) fn picker_set(
        &self,
        phase: crate::core::LivePhase,
        last_exit: Option<crate::core::LiveExitReason>,
        cap: usize,
    ) -> crate::core::site_picker::SiteSet {
        use crate::core::session::Stream;
        crate::core::site_picker::SiteSet {
            sites: self
                .iter()
                .map(|s| crate::core::site_picker::OnMap {
                    id: s.id.0.clone(),
                    age_secs: s.displayed.as_ref().and(s.data_staleness_secs),
                    stream: Stream::classify(s.live.frame_status.activity, phase, last_exit),
                })
                .collect(),
            focus: self.focus,
            cap,
        }
    }

    /// The timeline lanes over `[start, end]`: one per site but the focus,
    /// in list order (the focus has the full track). `session` is the live
    /// session; `downloads` gives a site's failed, queued and downloading
    /// scans, to which each site's ledger adds the scans requested and not
    /// yet on its timeline (as of `now_ms`). Empty with one site.
    pub(crate) fn lanes(
        &self,
        session: &crate::core::LiveModeState,
        downloads: impl Fn(&SiteId) -> crate::core::timeline_view::LaneDownloads,
        now_ms: f64,
        start: f64,
        end: f64,
    ) -> Vec<crate::core::timeline_view::LaneVm> {
        use crate::core::session::Stream;
        self.iter()
            .enumerate()
            .filter(|&(i, _)| i != self.focus)
            .map(|(_, s)| {
                let mut downloads = downloads(&s.id);
                downloads
                    .in_flight
                    .extend(s.ledger.in_progress_scan_starts(now_ms));
                crate::core::timeline_view::lane_vm(
                    &s.id.0,
                    &s.timeline_view(session.is_active()),
                    start,
                    end,
                    crate::core::timeline_view::LaneInputs {
                        downloads: &downloads,
                        newest_chunk_secs: s.live.volume.last_radial_time_secs,
                        stream: Stream::classify(
                            s.live.frame_status.activity,
                            session.phase,
                            session.last_exit_reason,
                        ),
                    },
                )
            })
            .collect()
    }

    /// Every site's stream as the activity chip and sheet read it, in list
    /// order. `phase` and `last_exit` are the live session's.
    pub(crate) fn activity_streams(
        &self,
        phase: crate::core::LivePhase,
        last_exit: Option<crate::core::LiveExitReason>,
    ) -> Vec<crate::core::activity::SiteStream> {
        use crate::core::session::Stream;
        self.iter()
            .map(|s| {
                let status = &s.live.frame_status;
                crate::core::activity::SiteStream {
                    id: s.id.0.clone(),
                    stream: Stream::classify(status.activity, phase, last_exit),
                    chunks_received: status.chunks_received,
                    last_chunk_age_secs: status.data_age_secs,
                    retry_in_secs: status.retry_in_secs,
                }
            })
            .collect()
    }

    /// Every site's request ledger counts, summed, for the activity surface.
    pub(crate) fn ledger_summary(&self, now_ms: f64) -> crate::core::acquisition::LedgerSummary {
        self.iter().map(|s| s.ledger.summarize(now_ms)).fold(
            Default::default(),
            crate::core::acquisition::LedgerSummary::merge,
        )
    }

    /// Every site's scans in the ingest→timeline blackout, for the
    /// activity surface's Finishing count.
    pub(crate) fn awaiting_scans(&self, now_ms: f64) -> Vec<crate::core::activity::AwaitingScan> {
        self.iter()
            .flat_map(|s| {
                s.ledger
                    .awaiting_scan_starts(now_ms)
                    .into_iter()
                    .map(|scan_start| crate::core::activity::AwaitingScan {
                        site: s.id.0.clone(),
                        scan_start,
                    })
            })
            .collect()
    }

    /// Which sites may hold a volume on `layout`, in list order
    /// ([`crate::core::volume_plan::resident_volumes`]).
    pub(crate) fn volume_residency(&self, layout: crate::core::phone::DeviceLayout) -> Vec<bool> {
        use crate::core::volume_plan::{resident_volumes, volume_budget};
        resident_volumes(self.list.len(), self.focus, volume_budget(layout))
    }

    /// The storm cells the canvas draws: the scene's, detected on the
    /// composite grid, while more than one site is shown, else the focus's.
    pub(crate) fn shown_cells(&self) -> &crate::core::StormCellsResult {
        if crate::core::storm_cells::uses_scene(self.list.len()) {
            &self.scene.cells.result
        } else {
            &self.focus().cells.result
        }
    }

    /// The lettered tracks on [`Self::shown_cells`], which the canvas draws
    /// and the distance tool snaps to: the scene's tracker while more than
    /// one site is shown, else `focus_tracks` (the focus's, on `VizState`).
    pub(crate) fn shown_tracks_view(
        &self,
        focus_tracks: &StormTracks,
        reference: Option<(f64, f64)>,
    ) -> Option<crate::core::storm_tracks::TracksView> {
        if crate::core::storm_cells::uses_scene(self.list.len()) {
            self.scene.tracks.view(reference)
        } else {
            focus_tracks.view(reference)
        }
    }

    /// The developer readout of [`Self::shown_cells`].
    pub(crate) fn shown_cell_stats(&self) -> Option<crate::core::storm_cells::StormCellStats> {
        if crate::core::storm_cells::uses_scene(self.list.len()) {
            self.scene.cells.stats()
        } else {
            self.focus().cells.stats()
        }
    }

    /// Every site's volume texture size, in list order, for the developer
    /// readout.
    pub(crate) fn volume_textures(&self) -> Vec<crate::core::volume_plan::VolumeTexture> {
        self.iter()
            .map(|s| crate::core::volume_plan::VolumeTexture {
                site: s.id.0.clone(),
                bytes: s.gpu.volume_bytes(),
            })
            .collect()
    }

    /// The ids in list order, for [`crate::core::sites::route`].
    pub(crate) fn ids(&self) -> Vec<SiteId> {
        self.iter().map(|s| s.id.clone()).collect()
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
    fn iter_visits_every_site_and_iter_mut_writes_each() {
        let mut sites = two_sites();
        for site in sites.iter_mut() {
            site.shadows.push(ScanBoundary { start: 0, end: 1 });
        }
        let ids: Vec<_> = sites.iter().map(|s| s.id.0.as_str()).collect();
        assert_eq!(ids, ["KDMX", "KTLX"]);
        assert!(sites.iter().all(|s| s.shadows.len() == 1));
    }

    fn ids_of(sites: &Sites) -> Vec<&str> {
        sites.iter().map(|s| s.id.0.as_str()).collect()
    }

    #[wasm_bindgen_test]
    fn the_canvas_draws_every_site_older_frame_first() {
        let mut sites = two_sites();
        sites.list[0].displayed = Some(sweep(500.0, 600.0));
        sites.list[1].displayed = Some(sweep(100.0, 200.0));
        let drawn: Vec<_> = sites.drawn().map(|s| s.id.0.as_str()).collect();
        assert_eq!(drawn, ["KTLX", "KDMX"]);
        // A site with no frame draws first.
        sites.list[1].displayed = None;
        let drawn: Vec<_> = sites.drawn().map(|s| s.id.0.as_str()).collect();
        assert_eq!(drawn, ["KTLX", "KDMX"]);
    }

    #[wasm_bindgen_test]
    fn apply_add_pushes_and_keeps_the_focus() {
        let mut sites = two_sites();
        let gone = sites.apply(Change::Add, Some(SiteState::new("KFWS", 32.57, -97.30)));
        assert!(gone.is_empty());
        assert_eq!(ids_of(&sites), ["KDMX", "KTLX", "KFWS"]);
        assert_eq!(sites.focus, 0);
    }

    #[wasm_bindgen_test]
    fn apply_remove_hands_back_the_element_and_moves_the_focus() {
        let mut sites = two_sites();
        sites.focus = 1;
        let gone = sites.apply(Change::Remove { index: 1, focus: 0 }, None);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].id, SiteId::new("KTLX"));
        assert_eq!(ids_of(&sites), ["KDMX"]);
        assert_eq!(sites.focus, 0);
    }

    #[wasm_bindgen_test]
    fn apply_focus_moves_only_the_index() {
        let mut sites = two_sites();
        assert!(sites.apply(Change::Focus { index: 1 }, None).is_empty());
        assert_eq!(sites.focus().id, SiteId::new("KTLX"));
        assert_eq!(ids_of(&sites), ["KDMX", "KTLX"]);
    }

    #[wasm_bindgen_test]
    fn apply_only_keeps_one_element_focused() {
        let mut sites = two_sites();
        sites.list.push(SiteState::new("KFWS", 32.57, -97.30));
        sites.list[2]
            .shadows
            .push(ScanBoundary { start: 0, end: 1 });
        let gone = sites.apply(Change::Only { keep: 2 }, None);
        let gone: Vec<_> = gone.iter().map(|s| s.id.0.as_str()).collect();
        assert_eq!(gone, ["KDMX", "KTLX"]);
        assert_eq!(ids_of(&sites), ["KFWS"]);
        assert_eq!(sites.focus, 0);
        // The kept element is the same one, scans and all.
        assert_eq!(sites.focus().shadows.len(), 1);
    }

    #[wasm_bindgen_test]
    fn a_new_site_has_no_renderers_until_ensured() {
        let site = SiteState::new("KTLX", 35.33, -97.28);
        assert!(site.gpu.radar.is_none());
        assert!(site.gpu.globe_radar.is_none());
        assert!(site.gpu.volume_ray.is_none());
    }

    #[wasm_bindgen_test]
    fn lanes_are_every_site_but_the_focus_in_list_order() {
        let session = crate::core::LiveModeState::default();
        let mut sites = two_sites();
        sites.list.push(SiteState::new("KFWS", 32.57, -97.30));
        sites.list[2].shadows.push(ScanBoundary {
            start: 1_000,
            end: 1_300,
        });
        sites.focus = 1;
        let lanes = sites.lanes(&session, |_| Default::default(), 0.0, 0.0, 5_000.0);
        let ids: Vec<_> = lanes.iter().map(|l| l.site.as_str()).collect();
        assert_eq!(ids, ["KDMX", "KFWS"]);
        assert!(lanes[0].blocks.is_empty());
        assert_eq!(lanes[1].blocks.len(), 1);
    }

    #[wasm_bindgen_test]
    fn lanes_read_each_sites_own_failures() {
        let session = crate::core::LiveModeState::default();
        let mut sites = two_sites();
        sites.list[1].shadows.push(ScanBoundary {
            start: 1_000,
            end: 1_300,
        });
        let lanes = sites.lanes(
            &session,
            |id| crate::core::timeline_view::LaneDownloads {
                failed: if id.0 == "KTLX" {
                    vec![1_000]
                } else {
                    Vec::new()
                },
                ..Default::default()
            },
            0.0,
            0.0,
            5_000.0,
        );
        assert_eq!(
            lanes[0].blocks[0].state,
            crate::core::timeline_view::LaneBlockState::Failed
        );
    }

    #[wasm_bindgen_test]
    fn a_lane_shows_its_sites_requests_in_flight_from_the_ledger() {
        let session = crate::core::LiveModeState::default();
        let mut sites = two_sites();
        sites.list[1].shadows.push(ScanBoundary {
            start: 1_000,
            end: 1_300,
        });
        // Requested, no longer in the queue (downloaded, ingesting).
        sites.list[1].ledger.note_enqueued(1_000, None, 0.0);
        let lanes = sites.lanes(&session, |_| Default::default(), 10.0, 0.0, 5_000.0);
        assert_eq!(
            lanes[0].blocks[0].state,
            crate::core::timeline_view::LaneBlockState::InFlight
        );
    }

    #[wasm_bindgen_test]
    fn one_site_has_no_lanes() {
        let session = crate::core::LiveModeState::default();
        let sites = Sites::default();
        assert!(sites
            .lanes(&session, |_| Default::default(), 0.0, 0.0, 5_000.0)
            .is_empty());
    }

    #[wasm_bindgen_test]
    fn the_focus_holds_its_volume_first() {
        use crate::core::phone::DeviceLayout;
        let mut sites = two_sites();
        sites.list.push(SiteState::new("KFWS", 32.57, -97.30));
        sites.focus = 2;
        assert_eq!(
            sites.volume_residency(DeviceLayout::Desktop),
            vec![true, true, true]
        );
        assert_eq!(
            sites.volume_residency(DeviceLayout::Phone { landscape: false }),
            vec![false, false, true]
        );
    }

    #[wasm_bindgen_test]
    fn a_site_without_renderers_holds_no_volume_bytes() {
        let sites = two_sites();
        let textures = sites.volume_textures();
        assert_eq!(textures.len(), 2);
        assert_eq!(textures[1].site, "KTLX");
        assert!(textures.iter().all(|t| t.bytes == 0));
    }

    #[wasm_bindgen_test]
    fn the_activity_surface_reads_every_sites_ledger() {
        let mut sites = two_sites();
        sites.list[0].ledger.note_ingested(1_000, 60, 0.0);
        sites.list[1].ledger.note_ingested(1_000, 60, 0.0);
        sites.list[1].ledger.note_ingested(2_000, 60, 0.0);
        assert_eq!(sites.ledger_summary(10.0).awaiting_timeline, 3);
        let awaiting = sites.awaiting_scans(10.0);
        assert_eq!(awaiting.len(), 3);
        assert_eq!(awaiting.iter().filter(|a| a.site == "KTLX").count(), 2);
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
