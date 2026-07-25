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

        // Record frame time for FPS meter (dev mode only)
        if self.state.dev_mode {
            let dt = ctx.input(|i| i.stable_dt);
            self.state.session_stats.record_frame_time(dt);
        }

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

        // Run storm cell detection on demand when toggled on with existing data
        if self.state.viz_state.storm_cells_visible
            && self.state.viz_state.detected_storm_cells.is_empty()
        {
            if let Some(ref renderer) = self.gpu.gpu {
                if let Ok(r) = renderer.lock() {
                    if r.has_data() {
                        self.state.viz_state.detected_storm_cells = r.detect_storm_cells(
                            self.state.viz_state.center_lat,
                            self.state.viz_state.center_lon,
                            self.state.viz_state.storm_cell_threshold_dbz,
                        );
                    }
                }
            }
        }
        // Clear cached cells when toggle is off
        if !self.state.viz_state.storm_cells_visible
            && !self.state.viz_state.detected_storm_cells.is_empty()
        {
            self.state.viz_state.detected_storm_cells.clear();
        }

        // Detect site changes and clear volume ring
        if self
            .persistence
            .detect_site_change(&self.state.viz_state.site_id)
        {
            // A running stream targets the old site — stop it. If the user
            // was actively live (playhead attached), restart on the new site
            // so a site switch doesn't silently drop them out of live; the
            // command dispatches later this same frame.
            if self.live.mode_state.is_active() {
                let was_attached = self.playback.state.time_model.is_pinned()
                    || self.playback.state.time_model.is_lookback();
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
        }
    }
}
