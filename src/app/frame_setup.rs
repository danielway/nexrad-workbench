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
        if let Some(displayed) = self.state.viz_state.displayed.as_ref() {
            let now = self.state.frame_now.secs();
            let end_age = now - displayed.end_time;
            let start_age = now - displayed.start_time;
            self.state.viz_state.data_staleness_secs = (end_age >= 0.0).then_some(end_age);
            self.state.viz_state.data_staleness_start_secs =
                (start_age >= 0.0).then_some(start_age);
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
            .detect_site_change(&self.state.viz_state.site_id)
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
        if let Some(ref renderer) = self.gpu.gpu {
            if let Ok(mut r) = renderer.lock() {
                r.clear_data();
            }
        }
        self.render.playback_manager.clear_cache();
        // `clear_for_site_change` is broader than `clear_active_scan`:
        // it also wipes `available_elevations` and the per-render
        // dedup cache. Pair it with the on-GPU `displayed` wipe so
        // the timeline/canvas don't keep highlighting the old site.
        self.render.coordinator.clear_for_site_change();
        self.state.viz_state.displayed = None;
        self.state.viz_state.previous_displayed = None;
        self.timeline.shadow_scan_boundaries.clear();
        self.state.viz_state.storm_cells.reset();
    }

    /// Bring the storm cell result in line with what is displayed. The
    /// decision (keep, clear, show a cached result, or ask a worker) is
    /// [`crate::core::storm_cells::decide`]; this assembles its inputs and
    /// executes the action, posting the described [`DetectRequest`] to the
    /// worker pool. Called every frame, right after each GPU upload, and
    /// after each detect reply, so a cached or fresh result shows at once.
    ///
    /// [`DetectRequest`]: crate::core::storm_cells::DetectRequest
    pub(crate) fn sync_storm_cells(&mut self) {
        use crate::core::storm_cells::{decide, LiveCutProgress, StormCellEnv};

        let viz = &self.state.viz_state;
        let (visible, view_mode, product, threshold_dbz, radar_lat, radar_lon) = (
            viz.storm_cells_visible,
            viz.view_mode(),
            viz.product,
            viz.storm_cell_threshold_dbz,
            viz.center_lat,
            viz.center_lon,
        );
        // A live partial is displayed under a placeholder scan key (live
        // renders are not bound to a stored scan); its volume is the one the
        // stream is collecting.
        let live_anchor = self
            .live
            .mode_state
            .is_active()
            .then_some(self.live.mode_state.current_volume.as_ref())
            .flatten();
        let live_partial = self.state.viz_state.displayed.as_ref().and_then(|d| {
            let anchor = live_anchor.filter(|_| d.identity.scan_key.site.0.is_empty())?;
            let mut d = d.clone();
            d.identity.scan_key = anchor.scan_key.clone();
            Some(d)
        });
        let displayed = live_partial
            .as_ref()
            .or(self.state.viz_state.displayed.as_ref());
        let displayed_ms = displayed.map(|d| d.identity.scan_key.scan_start.0);
        let volume = displayed_ms.and_then(|ms| {
            self.timeline
                .scans
                .scans
                .iter()
                .rev()
                .find(|s| s.key_ms() == ms)
        });
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
        let live_vcp = live_anchor
            .filter(|a| Some(a.scan_key.scan_start.0) == displayed_ms)
            .and(self.live.radar_model.volume.as_ref())
            .and_then(|v| v.vcp_pattern.as_ref());
        let env = StormCellEnv {
            visible,
            view_mode,
            product,
            displayed,
            threshold_dbz,
            volume,
            live_vcp,
            live_cut: live_cut.as_ref(),
            radar_lat,
            radar_lon,
            now_ms: self.state.frame_now.millis(),
        };
        let cells = &mut self.state.viz_state.storm_cells;
        let action = decide(&env, cells);
        let request = cells.apply(action, &env);
        if let Some(request) = request {
            log::debug!("storm cells: requesting {:?}", request.key);
            let id = request.request_id;
            if !self.render.coordinator.detect(request) {
                self.state.viz_state.storm_cells.fail(id);
            }
        }
    }
}
