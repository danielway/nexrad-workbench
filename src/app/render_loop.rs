//! Render-loop machinery: playback advancement, GPU sweep texture sync,
//! display clearing, and the per-frame "do I need to render?" decision.
//!
//! These methods run after worker results have been drained (so the
//! latest decoded sweep is visible) and before UI panels render (so the
//! canvas reflects the new frame).

use crate::state::playback_manager::{PlaybackManager, PrevSweepAction};
use crate::state::SweepIdentity;
use crate::{data, state, WorkbenchApp, MAX_SCAN_AGE_SECS, PREFETCH_LOOKAHEAD_SECS};

impl WorkbenchApp {
    /// Auto-load scans when scrubbing the timeline and prefetch upcoming sweeps.
    pub(crate) fn advance_playback(&mut self) {
        // Live mode no longer skips playback-driven renders: the unified
        // resolver (`request_worker_render`) runs in both modes so a cached
        // cut paints while streaming. Live still owns *acquisition* (the
        // archive "acquiring" hint never applies) and the active-scan tracking
        // + prefetch-next-sweep path below stay archive-only.
        let live_active = self.live.mode_state.is_active();
        // Rebuild macro frame list when dirty (elevation selection, bounds, or
        // scan count changed). Uses the *effective* mode so the list is also
        // built during a lookback replay (which frame-steps regardless of zoom).
        if self.playback.state.effective_playback_mode() == crate::state::PlaybackMode::Macro {
            let product = self.state.viz_state.product.to_worker_string();
            let inputs = crate::state::MacroFrameInputs {
                elevation: self.state.viz_state.elevation_selection.clone(),
                product,
                bounds: self.playback.state.time_model.playback_bounds,
                scan_count: self.timeline.scans.scans.len(),
            };

            if let Some(cause) = self.playback.state.macro_playback.rebuild_cause(&inputs) {
                let frames = match &inputs.elevation {
                    crate::state::ElevationSelection::Fixed {
                        elevation_number, ..
                    } => self.timeline.scans.matching_sweep_end_times_by_number(
                        *elevation_number,
                        product,
                        inputs.bounds,
                    ),
                    crate::state::ElevationSelection::Latest => self
                        .timeline
                        .scans
                        .all_sweep_end_times(product, inputs.bounds),
                };
                self.playback
                    .state
                    .macro_playback
                    .store_rebuilt(inputs, frames);
                self.playback.state.sync_macro_frame_index();
                // When the elevation filter changes, snap playback_position
                // to the resolved frame so the canvas resolver picks a sweep
                // at the new elevation. Frames are sweep end-times and a
                // higher elevation's sweep starts after the previous one
                // ends — without snapping, the resolver's
                // `start_time <= playback_position` filter rejects every
                // sweep at the new elevation in the current scan, blanking
                // the canvas. Skip on bounds/scan_count changes so
                // streaming and selection edits don't teleport the cursor.
                if cause == crate::state::RebuildCause::ElevationChanged {
                    self.playback.state.snap_playback_to_macro_frame();
                }
            }

            // Detect manual seek: if playback position changed externally
            // (user clicked timeline, jog, etc.) re-sync frame index.
            let pos = self.playback.state.playback_position();
            let last_pos = self.playback.state.macro_playback.last_seen_position;
            if (pos - last_pos).abs() > 0.5 {
                self.playback.state.sync_macro_frame_index();
                self.playback.state.macro_playback.frame_accumulator = 0.0;
            }
            self.playback.state.macro_playback.last_seen_position = pos;
        }

        // Auto-load scan when scrubbing: find the most recent scan within 15 minutes.
        // In the worker architecture, this sends a render request directly —
        // the worker reads records from IDB, decodes the target elevation, and renders.
        //
        // In FixedTilt mode, we also detect intra-scan sweep changes: a scan may
        // contain multiple sweeps at the target elevation (e.g. VCP 215 has 0.5°
        // at both elevation_number 1 and 3). As playback advances past a new
        // sweep's start_time, we re-render with that sweep's elevation_number.
        // Uses module-level MAX_SCAN_AGE_SECS constant.
        {
            let playback_ts = self.playback.state.playback_position();

            // Skip the timeline walk when nothing that feeds the scrub
            // decision has moved since last frame. The O(scans) search
            // below used to run every frame even while paused; this lets
            // the idle case cost only a few comparisons.
            let scan_count = self.timeline.scans.scans.len();
            let elev_sel = &self.state.viz_state.elevation_selection;
            let active_ts = self
                .render
                .coordinator
                .scan_key()
                .map(|k| k.scan_start.as_secs_f64());
            let scrub_cache_hit = self.render.scrub_cache.last_playback_ts == Some(playback_ts)
                && self.render.scrub_cache.last_scan_count == scan_count
                && self.render.scrub_cache.last_active_scan_ts == active_ts
                && self
                    .render
                    .scrub_cache
                    .last_elevation_selection
                    .as_ref()
                    .is_some_and(|cached| cached == elev_sel);

            if !scrub_cache_hit {
                self.render.scrub_cache.last_playback_ts = Some(playback_ts);
                self.render.scrub_cache.last_scan_count = scan_count;
                self.render.scrub_cache.last_active_scan_ts = active_ts;
                self.render.scrub_cache.last_elevation_selection = Some(elev_sel.clone());
            }

            if !scrub_cache_hit {
                // Identify the scan covering the playback position. The
                // resolver in `request_worker_render` then decides which
                // sweep within it to actually fetch — advance_playback's
                // job is just to keep `RenderCoordinator.current_scan_key`
                // (and the elevation list / VCP-resolution) in sync.
                let scrub_action = self
                    .timeline
                    .scans
                    .find_recent_scan(playback_ts, MAX_SCAN_AGE_SECS)
                    .map(|scan| {
                        let scan_ts: f64 = scan.key_timestamp;
                        let mut elev_nums: Vec<u8> =
                            scan.sweeps.iter().map(|s| s.elevation_number).collect();
                        elev_nums.sort_unstable();
                        elev_nums.dedup();
                        let elev_list = Self::build_elevation_list(scan);
                        (scan_ts, elev_nums, elev_list)
                    });

                match scrub_action {
                    Some((scan_ts, elev_nums, elev_list)) => {
                        if self.render.coordinator.has_worker() {
                            let scan_key = data::ScanKey::from_secs_f64(
                                &self.state.viz_state.site_id,
                                scan_ts,
                            );
                            let scan_changed = active_ts != Some(scan_ts);
                            // The live ingest path owns active-scan tracking
                            // while streaming; only archive playback mutates it
                            // here (and `force_fresh_render` would fight live
                            // dedup). The unified `request_worker_render` below
                            // still runs in both modes.
                            if scan_changed && !live_active {
                                if !elev_nums.is_empty() {
                                    self.set_active_scan(scan_key, elev_nums, scan_ts);
                                } else {
                                    self.advance_active_scan_chunk(scan_key, &[], scan_ts);
                                }
                                self.state
                                    .viz_state
                                    .elevation_selection
                                    .resolve_for_vcp(&elev_list);
                                self.render.coordinator.force_fresh_render();
                                // Active scan moved — refresh the cache snapshot.
                                self.render.scrub_cache.last_active_scan_ts = self
                                    .render
                                    .coordinator
                                    .scan_key()
                                    .map(|k| k.scan_start.as_secs_f64());
                            }
                            self.request_worker_render();
                            if self.state.viz_state.volume_3d_enabled && !live_active {
                                self.request_worker_render_volume();
                            }
                        }
                    }
                    None => {
                        // The playhead drifted into an undownloaded region or
                        // gap. Per spec §11.2 (alignment §3) we DON'T blank on
                        // age — keep showing the most recent frame and surface
                        // the discrepancy via the canvas caption (computed at
                        // the end of this function). Blanking stays correct only
                        // for site/product/elevation changes and cache wipes,
                        // which clear `displayed` on their own paths.
                        //
                        // Still drop the stale active-scan key so the resolver
                        // and prefetch don't keep targeting a scan the playhead
                        // has left — without re-clearing the GPU frame.
                        if active_ts.is_some() && !live_active {
                            self.clear_active_scan();
                        }
                    }
                }
            }
        }

        // Pre-render next sweep: when playing and near the end of the current sweep,
        // preemptively send a render request for the upcoming sweep so the result
        // is ready when the boundary is crossed, reducing perceived stutter.
        // Skip in macro mode — frame jumps are instant and the frame list handles sequencing.
        if self.playback.state.playing
            && !live_active
            && self.render.coordinator.has_worker()
            && self.playback.state.playback_mode() == crate::state::PlaybackMode::Micro
        {
            let playback_ts = self.playback.state.playback_position();
            let speed = self.playback.state.speed.timeline_seconds_per_real_second();
            let prefetch_lookahead = PREFETCH_LOOKAHEAD_SECS * speed;

            if let Some(scan) = self.timeline.scans.find_scan_at_timestamp(playback_ts) {
                if let Some((sweep_idx, sweep)) = scan.find_sweep_at_timestamp(playback_ts) {
                    let time_to_end = sweep.end_time - playback_ts;
                    if time_to_end > 0.0 && time_to_end < prefetch_lookahead {
                        let next_elev_num = if sweep_idx + 1 < scan.sweeps.len() {
                            Some(scan.sweeps[sweep_idx + 1].elevation_number)
                        } else {
                            let future_ts = playback_ts + prefetch_lookahead;
                            self.timeline
                                .scans
                                .find_scan_at_timestamp(future_ts)
                                .and_then(|next_scan| {
                                    next_scan.sweeps.first().map(|s| s.elevation_number)
                                })
                        };

                        if let Some(next_en) = next_elev_num {
                            let cur_elev = self
                                .state
                                .viz_state
                                .displayed
                                .as_ref()
                                .map(|d| d.identity.elevation_number);
                            if cur_elev != Some(next_en) {
                                if let Some(scan_key) = self.render.coordinator.scan_key().cloned()
                                {
                                    let product =
                                        self.state.viz_state.product.to_worker_string().to_string();
                                    let prefetch_identity = SweepIdentity::new(
                                        scan_key.clone(),
                                        next_en,
                                        product.clone(),
                                    );
                                    log::debug!(
                                        "Prefetching next sweep: elev_num={} ({:.1}s ahead)",
                                        next_en,
                                        time_to_end,
                                    );
                                    self.render.coordinator.set_last_render(prefetch_identity);
                                    self.render
                                        .coordinator
                                        .render_direct(&scan_key, next_en, product);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Canvas honesty caption (spec §11.2). Derived here where the displayed
        // identity, playhead, and download-progress ranges are all in scope. The
        // live partial path owns the canvas while the playhead is attached
        // (pinned/lookback), so the caption is suppressed there — but a detached
        // background stream resolves the cached frame at the cursor, so the
        // caption applies just like ordinary archive browsing.
        let pos = self.playback.state.playback_position();
        let attached = self.playback.state.time_model.is_pinned()
            || self.playback.state.time_model.is_lookback();
        let displayed = self
            .state
            .viz_state
            .displayed
            .as_ref()
            .map(|d| (d.start_time, d.end_time, (d.start_time + d.end_time) / 2.0));
        let scan_covers_playhead = self
            .timeline
            .scans
            .find_recent_scan(pos, MAX_SCAN_AGE_SECS)
            .is_some();
        let fetch_covers_playhead = self.position_is_being_acquired(pos);
        self.state.viz_state.canvas_caption = state::derive_canvas_caption(
            attached,
            displayed,
            pos,
            scan_covers_playhead,
            fetch_covers_playhead,
        );
    }

    /// Whether an archive fetch covering `playback_secs` is in flight or being
    /// ingested — drives the canvas "Acquiring…" hint when the position has no
    /// cached scan yet. Checks the download-progress ghost ranges, which mirror
    /// the active and just-completed-but-still-ingesting downloads.
    fn position_is_being_acquired(&self, playback_secs: f64) -> bool {
        let pos = playback_secs as i64;
        let progress = &self.state.download_progress;
        progress
            .active_scans
            .iter()
            .chain(progress.in_flight_scans.iter())
            .chain(progress.pending_scans.iter())
            .any(|&(start, end)| pos >= start && pos <= end)
    }

    /// Stateless sweep animation: ensure the previous-sweep GPU texture matches
    /// the sweep that *should* be the under-layer based on the current playback
    /// position, not based on what happened to be rendered before.
    ///
    /// The "previous sweep" is the one displayed just before the current one:
    /// within the same scan that's the preceding sweep in time order. Only look
    /// at the previous scan if the current sweep is the very first in its scan.
    pub(crate) fn sync_prev_sweep_texture(&mut self) {
        // When a live partial is on the canvas, its previous-sweep texture is
        // owned by promote_current_to_previous in the LiveDecoded handler —
        // don't let the timeline-based sync overwrite or clear it. But when a
        // *cached* cut is shown during live (e.g. just after a mid-stream
        // reload), there's no live promote for it, so the archive prev-sweep
        // animation is correct and should run.
        if self.live.mode_state.is_active() && self.gpu_holds_live_sweep() {
            return;
        }

        if !self.state.effective_sweep_animation(&self.playback.state) {
            self.state.viz_state.previous_displayed = None;
            self.state.viz_state.last_sweep_line_cache = None;
            return;
        }

        let playback_ts = self.playback.state.playback_position();
        // Anchor "previous" to the on-GPU main slot (i.e., what's
        // actually on screen), not the resolver's intent — otherwise
        // the prev-sweep upload races ahead of the main upload.
        let displayed_elev = match self
            .state
            .viz_state
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
            scan_count: self.timeline.scans.scans.len(),
        };
        let prev_info = if self.state.render_cache.prev_sweep_cache_key.as_ref() == Some(&cache_key)
        {
            self.state.render_cache.prev_sweep_cache_value
        } else {
            let computed = PlaybackManager::find_prev_sweep(
                &self.timeline.scans,
                playback_ts,
                displayed_elev,
                is_auto,
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
                self.state.viz_state.previous_displayed = None;
                // Clear GPU previous sweep so shader composites against black
                if let Some(ref renderer) = self.gpu.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                return;
            }
        };

        let prev_scan_key =
            data::ScanKey::from_secs_f64(&self.state.viz_state.site_id, prev_scan_key_ts);

        // Mirror the prev-sweep slot into viz_state so the timeline's
        // secondary border and the prev-sweep overlay panel reflect what
        // we're driving into the prev-sweep GPU texture. Product matches
        // the current main-slot product (prev animation only makes sense
        // within a single product channel).
        let prev_product = self.state.viz_state.product.to_worker_string().to_string();
        self.state.viz_state.previous_displayed = Some(state::DisplayedSweep {
            identity: state::SweepIdentity::new(prev_scan_key.clone(), prev_elev_num, prev_product),
            start_time: prev_start,
            end_time: prev_end,
            elevation_deg: prev_elev_deg,
        });

        // Get current GPU prev sweep ID for comparison
        let current_gpu_prev_id = self.gpu.gpu.as_ref().and_then(|renderer| {
            renderer
                .lock()
                .ok()
                .and_then(|r| r.prev_sweep_id().map(String::from))
        });

        let product = self.state.viz_state.product.to_worker_string().to_string();
        let action = self.render.playback_manager.resolve_prev_sweep(
            &prev_scan_key,
            prev_elev_num,
            current_gpu_prev_id.as_deref(),
            &product,
        );

        match action {
            PrevSweepAction::AlreadyLoaded => {}
            PrevSweepAction::UploadFromCache(cache_key) => {
                // Clear stale previous sweep immediately
                if let Some(ref renderer) = self.gpu.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                if let Some(cached) = self.render.playback_manager.get_cached_sweep(&cache_key) {
                    if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
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
                if let Some(ref renderer) = self.gpu.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
                self.render
                    .coordinator
                    .render_direct(&scan_key, elevation_number, product);
            }
            PrevSweepAction::Clear => {
                if let Some(ref renderer) = self.gpu.gpu {
                    if let Ok(mut r) = renderer.lock() {
                        r.clear_previous_data();
                    }
                }
            }
        }
    }

    /// Clear the on-canvas sweep and the overlay fields when the entire scan
    /// is gone (e.g. scrubbed off the timeline). Resets the scan key too.
    pub(crate) fn clear_display_no_scan(&mut self) {
        if let Some(ref renderer) = self.gpu.gpu {
            if let Ok(mut r) = renderer.lock() {
                r.clear_data();
            }
        }
        self.clear_active_scan();
        self.state.viz_state.data_staleness_secs = None;
        self.state.viz_state.data_staleness_start_secs = None;
        self.state.viz_state.elevation = "-- deg".to_string();
        // clear_data() drops both GPU textures; match the prev-sweep state
        // so the timeline highlight and canvas overlay don't point at state
        // that no longer has backing pixels. `displayed = None` is the canonical
        // "no frame" signal the overlay/readout read to drop the timestamp.
        self.state.viz_state.displayed = None;
        self.state.viz_state.previous_displayed = None;
        self.render.scrub_cache.last_active_scan_ts = None;
    }

    /// Clear the on-canvas sweep when the selected (elevation, product) isn't
    /// available for the current scan, but the scan itself is still valid.
    /// Leaves the scan key intact so other elevations/products can still render.
    pub(crate) fn clear_display_no_sweep(&mut self) {
        if let Some(ref renderer) = self.gpu.gpu {
            if let Ok(mut r) = renderer.lock() {
                r.clear_data();
            }
        }
        self.state.viz_state.data_staleness_secs = None;
        self.state.viz_state.data_staleness_start_secs = None;
        self.state.viz_state.elevation = "-- deg".to_string();
        // clear_data() drops both GPU textures. sync_prev_sweep_texture
        // early-returns while `displayed` is None, so clearing it here
        // also keeps the prev slot from drifting until a new sweep lands.
        self.state.viz_state.displayed = None;
        self.state.viz_state.previous_displayed = None;
        // Keep `last_render` set to the failed identity. A "no pre-computed
        // sweep" answer is permanent for that exact (scan, elev, product), so
        // the dedup must suppress the next frame's identical request — clearing
        // it would re-fire the failing render every frame.
    }

    /// Re-render when the user changes elevation, product, or view mode.
    pub(crate) fn request_render_if_needed(&mut self) {
        if !self.render.coordinator.has_worker() {
            return;
        }
        let live_active = self.live.mode_state.is_active();
        // Archive renders against an active scan; without one there's nothing
        // to re-render. Live drives the unified resolver off the timeline, so
        // it doesn't need a coordinator scan key — switching to a completed
        // cut repaints it immediately from cache instead of waiting ~12s for
        // the next chunk. Volume (3D) renders stay archive-only.
        if !live_active && self.render.coordinator.scan_key().is_none() {
            return;
        }
        if !live_active
            && self.state.viz_state.volume_3d_enabled
            && self.state.viz_state.view_mode() == state::ViewMode::Globe3D
        {
            self.request_worker_render_volume();
        }
        self.request_worker_render();
    }
}
