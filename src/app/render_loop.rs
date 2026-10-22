//! Render-loop machinery: playback advancement, GPU sweep texture sync,
//! display clearing, and the per-frame "do I need to render?" decision.
//!
//! These methods run after worker results have been drained (so the
//! latest decoded sweep is visible) and before UI panels render (so the
//! canvas reflects the new frame).

use crate::core::playback_manager::{PlaybackManager, PrevSweepAction};
use crate::core::render_loop::{
    decide_prefetch_and_caption, reduce_advance_playback, ActiveScanSync, AdvancePlaybackActions,
    AdvancePlaybackEnv, AdvancePlaybackSlices, FrameFollowupEnv,
};
use crate::core::SweepIdentity;
use crate::{data, state, WorkbenchApp, MAX_SCAN_AGE_SECS, PREFETCH_LOOKAHEAD_SECS};

impl WorkbenchApp {
    /// Auto-load scans when scrubbing the timeline and prefetch upcoming
    /// sweeps. Thin shell over two pure decision points (see
    /// [`crate::core::render_loop`]): assemble → [`reduce_advance_playback`]
    /// → execute, then re-snapshot → [`decide_prefetch_and_caption`] →
    /// execute. The re-snapshot matters: the executed render request can
    /// blank the site's `displayed` and the active-scan sync moves its
    /// track's key, and the follow-up decision must see both.
    ///
    /// Runs once per site; the caption describes the focus site.
    pub(crate) fn advance_playback(&mut self) {
        // Rays stands down while the volume draws every tilt (and comes
        // back when it stops).
        self.playback
            .state
            .apply_tilt_role(self.state.viz_state.tilt_role().draws_all());
        for site in 0..self.sites.list.len() {
            self.advance_playback_for(site);
        }
    }

    /// [`Self::advance_playback`] for one site.
    fn advance_playback_for(&mut self, site: usize) {
        let target = &mut self.sites.list[site];
        let actions = reduce_advance_playback(
            AdvancePlaybackEnv {
                live_active: self.live.mode_state.is_active(),
                has_worker: self.render.coordinator.has_worker(),
                site_id: &target.id.0,
                product: self.state.viz_state.product,
                volume_3d_active: self.state.viz_state.volume_3d_active(),
                track_scan_key: target.render.scan_key(),
                max_scan_age_secs: MAX_SCAN_AGE_SECS,
            },
            AdvancePlaybackSlices {
                playback: &mut self.playback.state,
                elevation_selection: &mut self.state.viz_state.elevation_selection,
                scrub_cache: &mut target.scrub,
            },
            &target.scans,
        );
        self.execute_advance_playback_actions(site, actions);

        let target = &self.sites.list[site];
        let followup = decide_prefetch_and_caption(
            FrameFollowupEnv {
                live_active: self.live.mode_state.is_active(),
                has_worker: self.render.coordinator.has_worker(),
                product: self.state.viz_state.product,
                site_id: &target.id.0,
                elevation_selection: &self.state.viz_state.elevation_selection,
                volume_3d_active: self.volume_3d_active(),
                archive_boundaries: &target.shadows,
                displayed: target.displayed.as_ref(),
                track_scan_key: target.render.scan_key(),
                active_download_ranges: &self.state.download_progress.active_scans,
                in_flight_download_ranges: &self.state.download_progress.in_flight_scans,
                pending_download_ranges: &self.state.download_progress.pending_scans,
                max_scan_age_secs: MAX_SCAN_AGE_SECS,
                prefetch_lookahead_secs: PREFETCH_LOOKAHEAD_SECS,
            },
            &self.playback.state,
            &target.scans,
        );
        if let Some(prefetch) = followup.prefetch {
            log::debug!(
                "Prefetching next sweep: elev_num={} ({:.1}s ahead)",
                prefetch.identity.elevation_number,
                prefetch.lead_secs,
            );
            let identity = prefetch.identity;
            self.sites.list[site]
                .render
                .set_last_render(identity.clone());
            self.render.coordinator.render_direct(
                &identity.scan_key,
                identity.elevation_number,
                identity.product.clone(),
            );
        }
        if site == self.sites.focus {
            self.state.viz_state.caption_context = followup.caption_context;
        }
    }

    /// Execute the effects described by [`reduce_advance_playback`], in
    /// field order.
    fn execute_advance_playback_actions(&mut self, site: usize, actions: AdvancePlaybackActions) {
        match actions.active_scan {
            Some(ActiveScanSync::Set {
                scan_key,
                elevations,
                ..
            }) => self.set_active_scan(site, scan_key, elevations),
            Some(ActiveScanSync::AdvanceChunkEmpty { scan_key, .. }) => {
                self.advance_active_scan_chunk(site, scan_key, &[])
            }
            Some(ActiveScanSync::Clear) => self.clear_active_scan(site),
            None => {}
        }
        if actions.force_fresh_render {
            self.sites.list[site].render.force_fresh_render();
        }
        if actions.request_render {
            self.request_worker_render(site);
        }
        if actions.request_volume_render {
            self.request_worker_render_volume(site);
        }
    }

    /// Stateless sweep animation: ensure the previous-sweep GPU texture matches
    /// the sweep that *should* be the under-layer based on the current playback
    /// position, not based on what happened to be rendered before.
    ///
    /// The "previous sweep" is the one displayed just before the current one:
    /// within the same scan that's the preceding sweep in time order. Only look
    /// at the previous scan if the current sweep is the very first in its scan.
    ///
    /// Runs once per site.
    pub(crate) fn sync_prev_sweep_texture(&mut self) {
        for site in 0..self.sites.list.len() {
            self.sync_prev_sweep_texture_for(site);
        }
    }

    /// [`Self::sync_prev_sweep_texture`] for one site.
    fn sync_prev_sweep_texture_for(&mut self, site: usize) {
        // When a live partial is on the canvas, its previous-sweep texture is
        // owned by promote_current_to_previous in the LiveDecoded handler —
        // don't let the timeline-based sync overwrite or clear it. But when a
        // *cached* cut is shown during live (e.g. just after a mid-stream
        // reload), there's no live promote for it, so the archive prev-sweep
        // animation is correct and should run.
        if self.live.mode_state.is_active() && self.gpu_holds_live_sweep(site) {
            return;
        }

        if !self.state.effective_sweep_animation(
            &self.playback.state,
            self.live.canvas_follows_live(&self.playback.state),
            &self.live.radar_model,
        ) {
            self.sites.list[site].previous_displayed = None;
            self.state.viz_state.last_sweep_line_cache = None;
            return;
        }

        let playback_ts = self.playback.state.playback_position();
        // Anchor "previous" to the on-GPU main slot (i.e., what's
        // actually on screen), not the resolver's intent — otherwise
        // the prev-sweep upload races ahead of the main upload.
        let displayed_elev = match self.sites.list[site]
            .displayed
            .as_ref()
            .map(|d| d.identity.elevation_number)
        {
            Some(e) => e,
            None => return,
        };

        let is_auto = self.state.viz_state.elevation_selection.is_auto();

        // Cache the previous-sweep search by its inputs. When the user is
        // paused on the same sweep frame after frame, the timeline walk in
        // `find_prev_sweep` becomes a no-op cache hit.
        let cache_key = state::PrevSweepCacheKey {
            playback_ts_bits: playback_ts.to_bits(),
            displayed_elev,
            is_auto,
            scan_count: self.sites.list[site].scans.scans.len(),
        };
        let prev_info = if self.state.render_cache.prev_sweep_cache_key.as_ref() == Some(&cache_key)
        {
            self.state.render_cache.prev_sweep_cache_value
        } else {
            let computed = PlaybackManager::find_prev_sweep(
                &self.sites.list[site].scans,
                playback_ts,
                displayed_elev,
                crate::core::tilt::TiltFilter::from(&self.state.viz_state.elevation_selection),
                MAX_SCAN_AGE_SECS,
            );
            self.state.render_cache.prev_sweep_cache_key = Some(cache_key);
            self.state.render_cache.prev_sweep_cache_value = computed;
            computed
        };

        let (prev_scan_key_ts, prev_elev_num, prev_elev_deg, prev_start, prev_end) = match prev_info
        {
            Some(info) => info,
            None => {
                self.sites.list[site].previous_displayed = None;
                // Clear GPU previous sweep so shader composites against black
                if let Some(ref renderer) = self.sites.list[site].gpu.radar {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                return;
            }
        };

        let prev_scan_key =
            data::ScanKey::from_secs_f64(&self.sites.list[site].id.0, prev_scan_key_ts);

        // Mirror the prev-sweep slot onto the site so the timeline's
        // secondary border and the prev-sweep overlay panel reflect what
        // we're driving into the prev-sweep GPU texture. Product matches
        // the current main-slot product (prev animation only makes sense
        // within a single product channel).
        let prev_product = self.state.viz_state.product.to_worker_string().to_string();
        self.sites.list[site].previous_displayed = Some(crate::core::DisplayedSweep {
            identity: SweepIdentity::new(prev_scan_key.clone(), prev_elev_num, prev_product),
            start_time: prev_start,
            end_time: prev_end,
            elevation_deg: prev_elev_deg,
        });

        // Get current GPU prev sweep ID for comparison
        let current_gpu_prev_id = self.sites.list[site]
            .gpu
            .radar
            .as_ref()
            .and_then(|renderer| {
                renderer
                    .lock()
                    .ok()
                    .and_then(|r| r.prev_sweep_id().map(String::from))
            });

        let product = self.state.viz_state.product.to_worker_string().to_string();
        let action = self.sites.list[site].crossfade.resolve_prev_sweep(
            &prev_scan_key,
            prev_elev_num,
            current_gpu_prev_id.as_deref(),
            &product,
        );

        match action {
            PrevSweepAction::AlreadyLoaded => {}
            PrevSweepAction::UploadFromCache(cache_key) => {
                // Clear stale previous sweep immediately
                if let Some(ref renderer) = self.sites.list[site].gpu.radar {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                if let Some(cached) = self.sites.list[site].crossfade.get_cached_sweep(&cache_key) {
                    if let (Some(ref renderer), Some(ref gl)) =
                        (&self.sites.list[site].gpu.radar, &self.gpu.gl)
                    {
                        if let Ok(mut r) = renderer.lock() {
                            r.update_previous_data(
                                gl,
                                &cached.azimuths,
                                &cached.gate_values,
                                cached.azimuth_count,
                                cached.gate_count,
                                cached.first_gate_range_km,
                                cached.gate_interval_km,
                                cached.max_range_km,
                                cached.offset,
                                cached.scale,
                                cached.azimuth_spacing_deg,
                                Some(cache_key),
                                &cached.radial_times,
                            );
                        }
                    }
                }
            }
            PrevSweepAction::FetchFromWorker {
                scan_key,
                elevation_number,
                product,
            } => {
                // Clear stale previous sweep immediately
                if let Some(ref renderer) = self.sites.list[site].gpu.radar {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                self.render
                    .coordinator
                    .render_direct(&scan_key, elevation_number, product);
            }
            PrevSweepAction::Clear => {
                if let Some(ref renderer) = self.sites.list[site].gpu.radar {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
            }
        }
    }

    /// Clear the on-canvas sweep and the overlay fields when the entire scan
    /// is gone (e.g. scrubbed off the timeline). Resets site `site`'s scan
    /// key too.
    pub(crate) fn clear_display_no_scan(&mut self, site: usize) {
        if let Some(ref renderer) = self.sites.list[site].gpu.radar {
            if let Ok(mut r) = renderer.lock() {
                r.clear_data();
            }
        }
        self.clear_active_scan(site);
        if site == self.sites.focus {
            self.state.viz_state.elevation = "-- deg".to_string();
        }
        // clear_data() drops both GPU textures; match the prev-sweep state
        // so the timeline highlight and canvas overlay don't point at state
        // that no longer has backing pixels. `displayed = None` is the canonical
        // "no frame" signal the overlay/readout read to drop the timestamp.
        let target = &mut self.sites.list[site];
        target.clear_display();
        target.scrub.last_active_scan_ts = None;
    }

    /// Clear the on-canvas sweep when the selected (elevation, product) isn't
    /// available for the current scan, but the scan itself is still valid.
    /// Leaves the scan key intact so other elevations/products can still render.
    pub(crate) fn clear_display_no_sweep(&mut self, site: usize) {
        if let Some(ref renderer) = self.sites.list[site].gpu.radar {
            if let Ok(mut r) = renderer.lock() {
                r.clear_data();
            }
        }
        if site == self.sites.focus {
            self.state.viz_state.elevation = "-- deg".to_string();
        }
        // clear_data() drops both GPU textures. sync_prev_sweep_texture
        // early-returns while `displayed` is None, so clearing it here
        // also keeps the prev slot from drifting until a new sweep lands.
        self.sites.list[site].clear_display();
        // Keep `last_render` set to the failed identity. A "no pre-computed
        // sweep" answer is permanent for that exact (scan, elev, product), so
        // the dedup must suppress the next frame's identical request — clearing
        // it would re-fire the failing render every frame.
    }

    /// Re-render when the user changes elevation, product, or view mode.
    /// Runs once per site.
    pub(crate) fn request_render_if_needed(&mut self) {
        if !self.render.coordinator.has_worker() {
            return;
        }
        for site in 0..self.sites.list.len() {
            self.request_render_if_needed_for(site);
        }
    }

    /// [`Self::request_render_if_needed`] for one site.
    fn request_render_if_needed_for(&mut self, site: usize) {
        let live_active = self.live.mode_state.is_active();
        // Archive renders against an active scan; without one there's nothing
        // to re-render. Live drives the unified resolver off the timeline, so
        // it doesn't need a coordinator scan key — switching to a completed
        // cut repaints it immediately from cache instead of waiting ~12s for
        // the next chunk. Volume (3D) renders stay archive-only.
        if !live_active && self.sites.list[site].render.scan_key().is_none() {
            return;
        }
        if !live_active && self.volume_3d_active() {
            self.request_worker_render_volume(site);
        }
        self.request_worker_render(site);
    }

    /// The single 3D predicate every volume-render gate shares: the
    /// volumetric layer is enabled AND the camera is in Globe3D. Keeping one
    /// definition means the frame-list granularity, the render triggers, and
    /// the ingest-driven volume refresh can never disagree about what "3D
    /// mode" is.
    pub(crate) fn volume_3d_active(&self) -> bool {
        self.state.viz_state.volume_3d_active()
    }
}
