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
            for site in &mut self.sites.list {
                site.gpu.ensure(gl);
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
        for site in &mut self.sites.list {
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
    }

    /// [`Self::sync_storm_cells_for`] for every site.
    pub(crate) fn sync_storm_cells(&mut self) {
        for site in 0..self.sites.list.len() {
            self.sync_storm_cells_for(site);
        }
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
        use crate::core::storm_cells::{decide, LiveCutProgress, StormCellEnv};
        use crate::core::storm_tracks::history_keys;

        let is_focus = site == self.sites.focus;
        let viz = &self.state.viz_state;
        // Disjoint borrows of the one site: the env reads its frame and
        // scans while the reducer writes its cells.
        let target = &mut self.sites.list[site];
        let (shown, scans, cells) = (&target.displayed, &target.scans, &mut target.cells);
        let (visible, view_mode, threshold_dbz, radar_lat, radar_lon) = (
            viz.storm_cells_visible,
            viz.view_mode(),
            viz.storm_cell_threshold_dbz,
            target.lat,
            target.lon,
        );
        // A live partial is displayed under a placeholder scan key (live
        // renders are not bound to a stored scan); its volume is the one the
        // stream is collecting. The stream belongs to the focus.
        let live_anchor = (is_focus && self.live.mode_state.is_active())
            .then_some(self.live.mode_state.current_volume.as_ref())
            .flatten();
        let live_partial = shown.as_ref().and_then(|d| {
            let anchor = live_anchor.filter(|_| d.identity.scan_key.site.0.is_empty())?;
            let mut d = d.clone();
            d.identity.scan_key = anchor.scan_key.clone();
            Some(d)
        });
        let displayed = live_partial.as_ref().or(shown.as_ref());
        let displayed_ms = displayed.map(|d| d.identity.scan_key.scan_start.0);
        let volume =
            displayed_ms.and_then(|ms| scans.scans.iter().rev().find(|s| s.key_ms() == ms));
        // The cut the live stream is filling, keyed to the live volume.
        let live_cut = live_anchor.and_then(|anchor| {
            let engine = self.live.engine.borrow();
            let obs = engine.observations();
            Some(LiveCutProgress {
                scan_key: anchor.scan_key.clone(),
                elevation_number: obs.current_in_progress_elevation?,
                radials: obs.current_in_progress_radials?,
            })
        });
        // Tracks need the cells of the volumes before the displayed one.
        let tracks_on = is_focus && visible && viz.storm_tracks_visible;
        let history = match displayed {
            Some(d) if tracks_on => history_keys(&scans.scans, &d.identity.scan_key, threshold_dbz),
            _ => Vec::new(),
        };
        let live_vcp = live_anchor
            .filter(|a| Some(a.scan_key.scan_start.0) == displayed_ms)
            .and(self.live.radar_model.volume.as_ref())
            .and_then(|v| v.vcp_pattern.as_ref());
        let env = StormCellEnv {
            visible,
            view_mode,
            displayed,
            threshold_dbz,
            volume,
            live_vcp,
            live_cut: live_cut.as_ref(),
            radar_lat,
            radar_lon,
            now_ms: self.state.frame_now.millis(),
            history: &history,
        };
        let action = decide(&env, cells);
        let request = cells.apply(action, &env);
        if is_focus {
            let st = &mut self.state.viz_state.storm_tracks;
            let observations = st.observations();
            let tracks = st.sync(tracks_on, cells, &history);
            if st.observations() != observations {
                log::debug!(
                    "storm tracks: {tracks:?}, newest letter {:?}",
                    st.newest_label()
                );
            } else if tracks != crate::core::storm_tracks::TrackSync::Off {
                log::trace!("storm tracks: {tracks:?}");
            }
        }
        if let Some(request) = request {
            log::debug!("storm cells: requesting {:?}", request.key);
            let id = request.request_id;
            if !self.render.coordinator.detect(request) {
                self.sites.list[site].cells.fail(id);
            }
        }
    }
}
