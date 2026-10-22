//! Per-frame bookkeeping: stats, theme, staleness, storm cells, site change.
//!
//! Runs first in the `update()` loop so the rest of the frame sees up-to-date
//! values for theme, data age, and active site.

use crate::WorkbenchApp;
use eframe::egui;

impl WorkbenchApp {
    /// Per-frame bookkeeping: record stats, apply theme, recompute staleness,
    /// update storm cells, and detect site changes.
    pub(crate) fn apply_frame_setup(&mut self, ctx: &egui::Context) {
        // THE wall-clock capture for this frame. Every frame-path consumer
        // (staleness, live tick, timeline, countdowns) reads this value so
        // they can't drift against each other within a frame.
        self.state.frame_now = crate::core::FrameNow::capture();

        // Every site gets its renderers before anything uploads to them.
        if let Some(gl) = self.gpu.gl.as_ref() {
            for site in self.sites.iter_mut() {
                site.gpu.ensure(gl);
            }
            // A site past the volume budget gives its volume texture back.
            let resident = self.sites.volume_residency(self.chrome.device_layout);
            for (site, keep) in self.sites.iter_mut().zip(resident) {
                if !keep && site.gpu.release_volume(gl) {
                    site.render.forget_volume_render();
                }
            }
        }

        // Safety release for the scrub level. The timeline strip owns this
        // flag, but if the strip stops rendering mid-drag (mobile switch,
        // panel collapse) its clear would never run and the reactive prefetch
        // would stay suppressed for the rest of the session. No button down
        // means no drag, so this can only ever release a stale flag — during a
        // real scrub the primary button is held.
        if !ctx.input(|i| i.pointer.any_down()) {
            self.state.pointer_scrub_active = false;
        }

        // Frame time for the transport row's fps readout.
        let dt = ctx.input(|i| i.stable_dt);
        self.state.session_stats.record_frame_time(dt);

        // Resolve theme and apply egui visuals. The `Visuals` struct is
        // cloned into the egui context on each `set_visuals` call, so guard
        // against the per-frame allocation+copy when the resolved theme
        // hasn't changed.
        self.state.is_dark = self.state.theme_mode.is_dark();
        if self.state.render_cache.last_dark != Some(self.state.is_dark) {
            if self.state.is_dark {
                let mut visuals = egui::Visuals::dark();
                visuals.panel_fill = egui::Color32::BLACK;
                visuals.window_fill = egui::Color32::BLACK;
                visuals.extreme_bg_color = egui::Color32::BLACK;
                ctx.set_visuals(visuals);
            } else {
                ctx.set_visuals(egui::Visuals::light());
            }
            self.state.render_cache.last_dark = Some(self.state.is_dark);
        }

        // Recompute data staleness every frame against wall-clock time.
        // This ensures archive data correctly shows its true age (days/years)
        // rather than a misleading "few minutes" relative to playback position.
        let now = self.state.frame_now.secs();
        for site in self.sites.iter_mut() {
            site.refresh_staleness(now);
        }

        // Ensure continuous repainting for time-dependent UI elements (the "now"
        // marker on the timeline and the data age desaturation) even when the user
        // is idle and playback is stopped.  Repaint once per second which is
        // sufficient for these indicators while being easy on the CPU.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));

        // Storm cells: run once per displayed sweep and threshold, never per
        // frame (an empty result is a result too).
        self.sync_storm_cells();

        // Detect site changes and clear volume ring
        self.sync_to_active_site();
    }

    /// Clear everything tied to the previous radar when the active site has
    /// changed since the last call. Latching lives in
    /// `PersistenceManager::detect_site_change`, so this is idempotent and safe
    /// to call more than once per frame.
    ///
    /// Called from [`Self::apply_frame_setup`] (catching site changes that
    /// arrive from deep links / restore) and directly from the `SelectSite`
    /// handler, so a user-driven site switch tears down the old site's data in
    /// the same step that retargets `viz_state` — never a frame later, which
    /// would paint the old radar under the new site's projection.
    pub(crate) fn sync_to_active_site(&mut self) {
        if !self
            .persistence
            .detect_site_change(&self.sites.focus().id.0)
        {
            return;
        }
        // A running stream targets the old site — stop it. If the user
        // was actively live (playhead attached), restart on the new site
        // so a site switch doesn't silently drop them out of live.
        if self.live.mode_state.is_active() {
            let was_attached =
                self.playback.state.is_pinned() || self.playback.state.is_live_range();
            self.stop_live_mode(crate::core::LiveExitReason::UserStopped);
            if was_attached {
                self.state.push_command(crate::core::Intent::StartLive);
            }
        }
        if let Some(ref renderer) = self.sites.focus().gpu.radar {
            if let Ok(mut r) = renderer.lock() {
                r.clear_data();
            }
        }
        let focus = self.sites.focus_mut();
        focus.crossfade.clear_cache();
        // `clear_for_site_change` is broader than `clear_active_scan`:
        // it also wipes `available_elevations` and the per-render
        // dedup cache. Pair it with the on-GPU `displayed` wipe so
        // the timeline/canvas don't keep highlighting the old site.
        focus.render.clear_for_site_change();
        focus.shadows.clear();
        focus.displayed = None;
        focus.previous_displayed = None;
        focus.cells.reset();
        self.state.viz_state.storm_tracks.reset();
        self.sites.scene.reset();
        // Letters restart with the tracks: let snapped endpoints go.
        self.state.viz_state.measurement.release_tracks();
    }

    /// Bring the storm cells in line with what the sites display. With one
    /// site, its own cells ([`Self::sync_storm_cells_for`]); with more, the
    /// scene's, detected once on the composite grid of every shown site
    /// ([`Self::sync_scene_cells`]), and the per-site results are not asked
    /// for. Switching between the two restarts the tracker of the path
    /// left.
    pub(crate) fn sync_storm_cells(&mut self) {
        let scene = crate::core::storm_cells::uses_scene(self.sites.list.len());
        if scene != self.sites.scene.active {
            self.sites.scene.active = scene;
            if scene {
                self.state.viz_state.storm_tracks.reset();
            } else {
                self.sites.scene.reset();
            }
            // The letters start over: let snapped endpoints go.
            self.state.viz_state.measurement.release_tracks();
        }
        if scene {
            self.sync_scene_cells();
        } else {
            for site in 0..self.sites.list.len() {
                self.sync_storm_cells_for(site);
            }
        }
    }

    /// A measurement endpoint snapped to a track (or "My location") follows
    /// it to the shown volume ([`crate::core::measure::follow`]), on the
    /// tracks the canvas shows ([`crate::subsystem::Sites::shown_tracks_view`]),
    /// each time the tracker that feeds them has run.
    fn follow_measurement(&mut self) {
        if !self.state.viz_state.measurement.follows_anything() {
            return;
        }
        let my_location = self
            .state
            .layer_state
            .geo
            .gps_location
            .then_some(self.diagnostics.gps.coords)
            .flatten();
        let viz = &mut self.state.viz_state;
        let tracks_on = viz.storm_cells_visible && viz.storm_tracks_visible;
        let view = tracks_on
            .then(|| self.sites.shown_tracks_view(&viz.storm_tracks, None))
            .flatten();
        crate::core::measure::follow(&mut viz.measurement, tracks_on, view.as_ref(), my_location);
    }

    /// Bring site `site`'s storm cell result in line with what it displays. The
    /// decision (keep, clear, show a cached result, or ask a worker) is
    /// [`crate::core::storm_cells::decide`]; this assembles its inputs and
    /// executes the action, posting the described [`DetectRequest`] to the
    /// worker pool. Called every frame, right after each GPU upload, and
    /// after each detect reply, so a cached or fresh result shows at once.
    ///
    /// [`DetectRequest`]: crate::core::storm_cells::DetectRequest
    ///
    /// Storm tracks stay single-valued on `VizState` and follow the focus.
    pub(crate) fn sync_storm_cells_for(&mut self, site: usize) {
        use crate::core::storm_cells::decide;
        use crate::core::storm_tracks::history_keys;

        let is_focus = site == self.sites.focus;
        let streaming = self.live.mode_state.is_active();
        let now_ms = self.state.frame_now.millis();
        let viz = &self.state.viz_state;
        let target = &mut self.sites.list[site];
        let inputs = SiteCellInputs::of(target, streaming);
        // Tracks need the cells of the volumes before the displayed one.
        let tracks_on = is_focus && viz.storm_cells_visible && viz.storm_tracks_visible;
        let history = match inputs.displayed() {
            Some(d) if tracks_on => history_keys(
                &target.scans.scans,
                &d.identity.scan_key,
                viz.storm_cell_threshold_dbz,
            ),
            _ => Vec::new(),
        };
        // Disjoint borrows of the one site: the env reads its frame and
        // scans while the reducer writes its cells.
        let (scans, live, cells) = (&target.scans, &target.live, &mut target.cells);
        let env = inputs.env(viz, scans, live, (target.lat, target.lon), now_ms, &history);
        let action = decide(&env, cells);
        let request = cells.apply(action, &env);
        if is_focus {
            let st = &mut self.state.viz_state.storm_tracks;
            let observations = st.observations();
            let tracks = st.sync(tracks_on, cells, &history);
            log_tracks(tracks, st.observations() != observations, st.newest_label());
            self.follow_measurement();
        }
        if let Some(request) = request {
            if request.volume_cuts.is_empty() {
                log::debug!("storm cells: requesting {:?}", request.key);
            } else {
                log::debug!(
                    "storm cells: requesting columns over cuts {:?} for {:?}",
                    request.volume_cuts,
                    request.key
                );
            }
            let id = request.request_id;
            if !self.render.coordinator.detect(request) {
                self.sites.list[site].cells.fail(id);
            }
        }
    }

    /// Bring the scene's storm cells in line with what every site displays:
    /// [`crate::core::storm_cells::decide_scene`] over each site's inputs,
    /// the described [`SceneDetectRequest`] posted to the worker pool, and
    /// the scene's tracker run on the result. Called every frame and after
    /// each scene detect reply.
    ///
    /// [`SceneDetectRequest`]: crate::core::storm_cells::SceneDetectRequest
    pub(crate) fn sync_scene_cells(&mut self) {
        use crate::core::storm_cells::{decide_scene, SceneCellEnv, SceneSiteEnv};
        use crate::core::storm_tracks::scene_history_keys;

        let streaming = self.live.mode_state.is_active();
        let now_ms = self.state.frame_now.millis();
        let viz = &self.state.viz_state;
        let crate::subsystem::Sites { list, scene, .. } = &mut self.sites;
        let inputs: Vec<SiteCellInputs> = list
            .iter()
            .map(|s| SiteCellInputs::of(s, streaming))
            .collect();
        let sites: Vec<SceneSiteEnv<'_>> = list
            .iter()
            .zip(&inputs)
            .map(|(s, i)| SceneSiteEnv {
                id: &s.id.0,
                env: i.env(viz, &s.scans, &s.live, (s.lat, s.lon), now_ms, &[]),
            })
            .collect();
        let tracks_on = viz.storm_cells_visible && viz.storm_tracks_visible;
        // History follows the scene that is shown: its sites' volumes before
        // it.
        let history = match scene.cells.computed_for.as_ref() {
            Some(shown) if tracks_on => {
                let scans: Vec<&[crate::core::Scan]> = shown
                    .sites
                    .iter()
                    .filter_map(|k| {
                        list.iter()
                            .find(|s| s.id == k.scan_key.site)
                            .map(|s| s.scans.scans.as_slice())
                    })
                    .collect();
                scene_history_keys(&scans, shown)
            }
            _ => Vec::new(),
        };
        let env = SceneCellEnv {
            visible: viz.storm_cells_visible,
            view_mode: viz.view_mode(),
            threshold_dbz: viz.storm_cell_threshold_dbz,
            now_ms,
            sites: &sites,
            history: &history,
        };
        let action = decide_scene(&env, &scene.cells);
        let request = scene.cells.apply_scene(action, &env);
        let observations = scene.tracks.observations();
        let tracks = scene.tracks.sync(tracks_on, &scene.cells, &history);
        log_tracks(
            tracks,
            scene.tracks.observations() != observations,
            scene.tracks.newest_label(),
        );
        self.follow_measurement();
        if let Some(request) = request {
            log::debug!("storm cells: requesting the scene {:?}", request.key);
            let id = request.request_id;
            if !self.render.coordinator.detect_composite(request) {
                self.sites.scene.cells.fail(id);
            }
        }
    }
}

fn log_tracks(
    tracks: crate::core::storm_tracks::TrackSync,
    observed: bool,
    newest: Option<String>,
) {
    if observed {
        log::debug!("storm tracks: {tracks:?}, newest letter {newest:?}");
    } else if tracks != crate::core::storm_tracks::TrackSync::Off {
        log::trace!("storm tracks: {tracks:?}");
    }
}

/// One site's owned inputs to a storm cell decision, gathered before the
/// borrowed [`crate::core::storm_cells::StormCellEnv`] is built over them.
struct SiteCellInputs {
    /// What the site displays. A live partial is displayed under a
    /// placeholder scan key (live renders are not bound to a stored scan);
    /// here it carries the key of the volume the site's stream is
    /// collecting.
    displayed: Option<crate::core::DisplayedSweep>,
    /// The cut the live stream is filling, keyed to the live volume.
    live_cut: Option<crate::core::storm_cells::LiveCutProgress>,
    /// The displayed volume is the one streaming.
    live_volume_displayed: bool,
}

impl SiteCellInputs {
    fn of(site: &crate::subsystem::SiteState, streaming: bool) -> Self {
        use crate::core::storm_cells::LiveCutProgress;
        let live = &site.live;
        let live_anchor = streaming
            .then_some(live.volume.current_volume.as_ref())
            .flatten();
        let shown = site.displayed.as_ref();
        let displayed = shown.map(|d| {
            let mut d = d.clone();
            if let Some(anchor) = live_anchor.filter(|_| d.identity.scan_key.site.0.is_empty()) {
                d.identity.scan_key = anchor.scan_key.clone();
            }
            d
        });
        let displayed_ms = displayed.as_ref().map(|d| d.identity.scan_key.scan_start.0);
        let live_cut = live_anchor.and_then(|anchor| {
            let engine = live.engine.borrow();
            let obs = engine.observations();
            Some(LiveCutProgress {
                scan_key: anchor.scan_key.clone(),
                elevation_number: obs.current_in_progress_elevation?,
                radials: obs.current_in_progress_radials?,
            })
        });
        let live_volume_displayed =
            live_anchor.is_some_and(|a| Some(a.scan_key.scan_start.0) == displayed_ms);
        Self {
            displayed,
            live_cut,
            live_volume_displayed,
        }
    }

    fn displayed(&self) -> Option<&crate::core::DisplayedSweep> {
        self.displayed.as_ref()
    }

    fn env<'a>(
        &'a self,
        viz: &crate::state::VizState,
        scans: &'a crate::core::RadarTimeline,
        live: &'a crate::subsystem::SiteLive,
        (radar_lat, radar_lon): (f64, f64),
        now_ms: f64,
        history: &'a [crate::core::storm_cells::StormCellKey],
    ) -> crate::core::storm_cells::StormCellEnv<'a> {
        let displayed = self.displayed.as_ref();
        let displayed_ms = displayed.map(|d| d.identity.scan_key.scan_start.0);
        let volume =
            displayed_ms.and_then(|ms| scans.scans.iter().rev().find(|s| s.key_ms() == ms));
        let live_vcp = self
            .live_volume_displayed
            .then_some(live.radar_model.volume.as_ref())
            .flatten()
            .and_then(|v| v.vcp_pattern.as_ref());
        crate::core::storm_cells::StormCellEnv {
            visible: viz.storm_cells_visible,
            view_mode: viz.view_mode(),
            displayed,
            threshold_dbz: viz.storm_cell_threshold_dbz,
            volume,
            live_vcp,
            live_cut: self.live_cut.as_ref(),
            radar_lat,
            radar_lon,
            now_ms,
            history,
        }
    }
}
