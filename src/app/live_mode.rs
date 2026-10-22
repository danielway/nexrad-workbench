//! Live-mode lifecycle, render-request helpers, and realtime result handling.
//!
//! Groups the methods that drive a live streaming session (`start_live_mode`,
//! `stop_live_mode`, `handle_realtime_result`) alongside the render-request
//! helpers (`request_worker_render`, `request_worker_render_volume`,
//! `update_overlay_from_sweep`, `build_elevation_list`) that both the live
//! and archive paths use.

use crate::{nexrad, state, WorkbenchApp, MAX_SCAN_AGE_SECS};
use eframe::egui;

impl WorkbenchApp {
    pub(crate) fn start_live_mode(&mut self, ctx: &egui::Context) {
        let now = self.state.frame_now.secs();

        // Initialize live mode state. Going live pins the playhead to "now"
        // but does NOT start playback — play/pause is decoupled and, while
        // live, drives the live-range replay instead. The playhead is kept on
        // "now" by `tick_live`, independent of `playing`.
        self.live.mode_state.start(now);
        self.playback.state.enter_pinned_live(now);

        // Ensure the timeline is zoomed in far enough to show individual sweeps
        // and chunks. Live mode enforces micro-mode as the widest allowed zoom.
        // Route through the tier machine so the stored tier lands in Micro
        // (LIVE_DEFAULT_ZOOM is comfortably above the Micro-enter threshold).
        if self.playback.state.timeline_zoom < crate::LIVE_DEFAULT_ZOOM {
            let width = self.playback.state.timeline_width_px;
            self.playback
                .state
                .set_timeline_zoom(crate::LIVE_DEFAULT_ZOOM, width);
            self.playback.state.center_view_on(now);
        }

        // Every listed site streams from its own channel, started with that
        // site's id and projection engine.
        for site in self.sites.iter_mut() {
            log::info!("Starting live mode for site: {}", site.id.0);
            site.live.volume.begin();
            // Push the current elevation filter into the channel before
            // starting so the streaming loop's init-time backfill targets the
            // user's selected elevation rather than a default.
            site.live.channel.sync_filter(
                &self.state.viz_state.elevation_selection,
                site.live.radar_model.vcp_pattern(),
            );
            site.live.channel.start(
                ctx.clone(),
                site.id.0.clone(),
                self.acquisition.coordinator.facade().clone(),
                site.live.engine.clone(),
            );
        }
    }

    /// Re-pin the playhead to the live edge (the `ReturnToLive` command).
    ///
    /// Instant when the stream is already running — detached browsing snaps
    /// straight back to LIVE-NOW with no re-acquisition. Starts a fresh
    /// stream otherwise. Unlike `start_live_mode` this preserves the user's
    /// zoom (only raising it to the micro floor if they zoomed out past it
    /// while detached).
    pub(crate) fn return_to_live(&mut self, ctx: &egui::Context) {
        if !self.live.mode_state.is_active() {
            self.start_live_mode(ctx);
            return;
        }
        let now = self.state.frame_now.secs();
        self.live.mode_state.detached_since = None;
        self.playback.state.clear_selection();
        // `enter_pinned_live` owns the live speed (1× in Rays); the Micro floor
        // zoom below only changes the view.
        self.playback.state.enter_pinned_live(now);
        // Return-to-live re-tethers, so the zoom floor must land the tier back
        // in Micro. The Micro-enter threshold carries hysteresis (≥ 1.15),
        // so floor to LIVE_DEFAULT_ZOOM (2.0) rather than the nominal 1.0.
        if self.playback.state.timeline_tier != crate::core::TimelineTier::Micro {
            let width = self.playback.state.timeline_width_px;
            self.playback
                .state
                .set_timeline_zoom(crate::LIVE_DEFAULT_ZOOM, width);
            self.playback.state.center_view_on(now);
        }
    }

    /// Apply a loop preset (spec §8 "presets first"): the last N frames or a
    /// duration, attached to now. Re-tethers first (or starts a stream) so the
    /// range starts from the live point, then `enter_live_range` slides it
    /// forward with the live edge. Routes every mode change through the named
    /// playhead transitions so the `Selection` invariants hold.
    pub(crate) fn apply_loop_preset(
        &mut self,
        preset: crate::core::LoopPreset,
        ctx: &egui::Context,
    ) {
        let basis = preset.basis();
        let now = self.state.frame_now.secs();
        if !self.playback.state.is_pinned() {
            // Re-tether: instant if a stream is already running, otherwise
            // start one. `return_to_live` lands the playhead on the live point
            // synchronously (it calls `enter_pinned_live` in both the
            // active-stream and cold-start paths), even when the cold path
            // also queued a StartLive command for the channel.
            self.return_to_live(ctx);
        }
        // The live point is guaranteed here, so enter_live_range's
        // `is_pinned` assert holds.
        let seed = self.resolve_pinned_window(basis, now).0;
        self.playback.state.enter_live_range(Some(seed), basis);
    }

    /// Per-frame live tick — drives the playhead while streaming, independent
    /// of the `playing` flag (which now belongs to playback/lookback).
    ///
    /// - Live point (LIVE-NOW): pin the playhead to this frame's now.
    /// - Live range: slide the loop window so its end follows the latest
    ///   frame as new volumes complete. `advance` (driven by `playing` in the
    ///   bottom panel) does the actual looping, in Frames or Rays.
    /// - Detached (a fixed point or range while streaming): the user is
    ///   browsing; leave the playhead and view alone, but stop the stream once
    ///   the detour exceeds [`crate::LIVE_DETACHED_STOP_SECS`].
    ///
    /// In the two live states, keep the live edge on-screen. No-op when not
    /// streaming.
    pub(crate) fn tick_live(&mut self) {
        if !self.live.mode_state.is_active() {
            return;
        }
        let now = self.state.frame_now.secs();

        if self.playback.state.is_pinned() {
            // LIVE-NOW: pin to this frame's now.
            self.playback.state.pin_tick(now);
        } else if let Some(basis) = self
            .playback
            .state
            .selection
            .range()
            .filter(|r| r.live)
            .map(|r| r.basis)
        {
            // Live range: own the frame window, sized from the range's basis
            // (frame-count or duration — see `RangeSelection`). For a
            // frame-count basis, prefer the exact last-N-frame span; before any
            // matching frame is cached, fall back to a time window of ~N volumes
            // so `render_loop` builds `sweep_frames` from recent data — not all
            // history. A duration basis is simply the last `secs` ending at now.
            // `render_loop` turns these bounds into the macro frame list; the
            // backfill pump fills the window in, and this widens to the precise
            // span as frames land.
            //
            // While *playing*, defer incorporating newly arrived frames until the
            // playhead crosses the loop's wrap point (spec §8) — the committed
            // window stays fixed between wraps so the band doesn't stretch
            // mid-cycle. While paused/idle there's no cycle to disrupt, so the
            // window tracks now continuously.
            let (start, end) = self.resolve_pinned_window(basis, now);
            self.playback.state.slide_live_range(start, end);
        } else {
            // Detached: free browsing while the stream ingests in the
            // background. No pin, no auto-scroll, and the background S3
            // polling is bounded by an idle stop. `handle_streaming_results`
            // reconciles the channel off on the next frame; `playing` is
            // deliberately untouched so an archive replay keeps running.
            if crate::core::should_stop_for_detached_idle(
                self.live.mode_state.detached_since,
                now,
                crate::LIVE_DETACHED_STOP_SECS,
            ) {
                self.live.stop(
                    &mut self.sites,
                    crate::core::LiveExitReason::DetachedTimeout,
                );
                self.state
                    .toasts
                    .notice(crate::core::LiveExitReason::DetachedTimeout.message());
            }
            return;
        }

        self.keep_now_on_screen(now);
    }

    /// Resolve the *target* pinned-loop window for `now` from its basis — the
    /// window the loop would have if it tracked now continuously. The
    /// wrap-point deferral ([`PlaybackState::commit_pinned_window`]) decides
    /// whether this target is actually applied this frame.
    ///
    /// Frame-count: the span of the last `n` frames the timeline knows about,
    /// listed or cached (with a time fallback when too few are known).
    /// Duration: the last `secs` ending at now.
    pub(crate) fn resolve_pinned_window(
        &self,
        basis: crate::core::LoopBasis,
        now: f64,
    ) -> (f64, f64) {
        match basis {
            crate::core::LoopBasis::FrameCount(n) => self
                .sites
                .focus()
                .scans
                .lookback_window(
                    &self.sites.focus().shadows,
                    self.live_volume_frames(),
                    &self.state.viz_state.elevation_selection,
                    self.state.viz_state.product.to_worker_string(),
                    now,
                    n as usize,
                )
                .unwrap_or((now - basis.fallback_span_secs(), now)),
            crate::core::LoopBasis::Duration(secs) => (now - secs, now),
        }
    }

    /// The volume the stream is collecting, as the timeline draws it, for
    /// counting its frames (including cuts collected before the stream
    /// joined). `None` when not streaming or before its plan is known.
    fn live_volume_frames(&self) -> Option<crate::core::LiveVolume<'_>> {
        if !self.live.mode_state.is_active() {
            return None;
        }
        let live = &self.sites.focus().live;
        let anchor = live.volume.current_volume.as_ref()?;
        Some(crate::core::LiveVolume {
            key_secs: anchor.scan_key.scan_start.0 as f64 / 1000.0,
            projection: live.radar_model.position.as_ref()?,
        })
    }

    /// Nudge the timeline view minimally so "now" stays visible. Pan/zoom is
    /// otherwise free; this only fires when "now" would fall outside the
    /// visible range. (Relocated from the old `playing`-gated block in the
    /// bottom panel so it runs in both LIVE-NOW and LIVE-LOOKBACK.)
    fn keep_now_on_screen(&mut self, now: f64) {
        // A deliberate user pan turns view-following off; without this the
        // nudge below re-snaps every frame and panning away from the live edge
        // while streaming is impossible.
        if !self.playback.state.view_follows_now {
            return;
        }
        let view_width = self.playback.state.view_width_secs();
        if view_width <= 0.0 {
            return;
        }
        let view_start = self.playback.state.timeline_view_start;
        let view_end = view_start + view_width;
        if now > view_end {
            self.playback.state.timeline_view_start = now - view_width;
        } else if now < view_start {
            self.playback.state.timeline_view_start = now;
        }
    }

    /// Set site `site`'s staleness from a sweep's timing and, for the focus,
    /// the elevation readout.
    pub(crate) fn update_overlay_from_sweep(
        &mut self,
        site: usize,
        start: f64,
        end: f64,
        elevation_deg: f32,
    ) {
        if site == self.sites.focus {
            self.state.viz_state.set_elevation_label(elevation_deg);
        }
        let now = self.state.frame_now.secs();
        self.sites.list[site].set_staleness(start, end, now);
    }

    /// Send a render request to the worker for site `site`'s
    /// scan/elevation/product at the playhead.
    ///
    /// Mode-agnostic: the unified resolver merges the live in-progress
    /// accumulator with the cached timeline and returns one of three intents.
    /// Live and archive are *sources* feeding this decision, not exclusive
    /// owners of the canvas — so a cached completed cut paints even while
    /// streaming (the live partial only wins for the cut it's collecting).
    ///
    /// Honours the user's intent exactly: no fuzzy elevation fallback — if the
    /// user picks 5° and the resolved scan only has 1°/3°/7°, the canvas blanks
    /// rather than snapping to a neighbor.
    pub(crate) fn request_worker_render(&mut self, site: usize) {
        use crate::core::playback_manager::DesiredDisplay;

        let live_cut = self.live_render_sources(site);
        let target = &self.sites.list[site];
        let desired = crate::core::playback_manager::resolve_desired_display(
            &target.id.0,
            self.playback.state.playback_position(),
            &self.state.viz_state.elevation_selection,
            self.state.viz_state.product,
            &target.scans,
            MAX_SCAN_AGE_SECS,
            live_cut,
            self.live_shows_whole_sweeps(),
        );

        match desired {
            DesiredDisplay::LivePartial { elevation_number } => {
                // The chunk-ingest → `render_live` path already owns the GPU
                // for the actively-collecting cut. Don't request a cached blob
                // for it and don't blank — leave the live partial in place.
                // If a stored sweep is on the GPU instead (a live range's
                // playhead just reached the live cut, or a scrubbed playhead
                // was pinned again), draw the partial now rather than at the
                // next chunk.
                if !self.gpu_holds_live_sweep(site) {
                    self.render.coordinator.render_live(
                        self.sites.list[site].id.clone(),
                        elevation_number,
                        self.state.viz_state.product.to_worker_string().to_string(),
                    );
                }
            }
            DesiredDisplay::Cached(identity) => {
                let track = &mut self.sites.list[site].render;
                if self.render.coordinator.request_render_for(track, identity) {
                    // Was guarded by `&& !pipeline.processing`, which only ever
                    // skipped a redundant `true = true`.
                    self.state.session_stats.pipeline.processing = true;
                }
            }
            // Frames mode before any complete sweep is cached: keep the canvas.
            DesiredDisplay::Hold => {}
            DesiredDisplay::Blank => {
                // Don't clear a valid live partial: in a between-chunks frame
                // the in-progress elevation can momentarily read `None` and
                // resolve to `Blank` even though the GPU holds a good `live|*`
                // sweep. Only blank when archive owns the canvas — which it
                // does whenever the playhead is detached from the live edge.
                let protecting_live_partial = self
                    .live
                    .canvas_follows_live(&self.sites.list[site].live, &self.playback.state)
                    && self.gpu_holds_live_sweep(site);
                // Don't blank merely because the playhead drifted into a gap
                // (spec §11.2): a Blank with NO scan covering the playhead is the
                // "drifted away in time" case — keep the held frame and let the
                // canvas caption surface the discrepancy. Blank only when a scan
                // IS present but the selected elevation/product isn't in it (the
                // honest "nothing here for your filter" case).
                let scan_covers_playhead = self.sites.list[site]
                    .scans
                    .find_recent_scan(self.playback.state.playback_position(), MAX_SCAN_AGE_SECS)
                    .is_some();
                if !protecting_live_partial && scan_covers_playhead {
                    self.clear_display_no_sweep(site);
                }
            }
        }
    }

    /// Site `site`'s live cut feeding [`crate::core::playback_manager::resolve_desired_display`]:
    /// `Some((collecting_elevation, anchor_key_ms))` while streaming with a
    /// fully-known volume, the canvas following the site's live stream (see
    /// [`crate::subsystem::SiteLive::canvas_follows_live`]) AND radials of that
    /// elevation still in the accumulator (see
    /// [`crate::core::worker_ingest::live_cut`]), else
    /// `None` (which collapses the resolver to the cache path — a detached
    /// playhead resolves whatever's cached under the cursor, untouched by
    /// the background stream). Shared by `request_worker_render` and the
    /// `Decoded` upload gate in `handle_decoded_outcome`.
    pub(crate) fn live_render_sources(&self, site: usize) -> Option<(u8, i64)> {
        let live = &self.sites.list[site].live;
        if !self.live.canvas_follows_live(live, &self.playback.state) {
            return None;
        }
        let anchor_ms = live
            .volume
            .current_volume
            .as_ref()
            .map(|a| a.scan_key.scan_start.0);
        let engine = live.engine.borrow();
        let obs = engine.observations();
        crate::core::worker_ingest::live_cut(
            obs.current_in_progress_elevation,
            obs.current_in_progress_radials,
            anchor_ms,
        )
    }

    /// Frames mode: live shows only complete sweeps, never the partial the
    /// stream is filling in.
    pub(crate) fn live_shows_whole_sweeps(&self) -> bool {
        self.playback.state.playback_mode() == crate::core::PlaybackMode::Macro
    }

    /// Whether site `site`'s main GPU texture holds a live partial sweep
    /// (`current_sweep_id` like `live|{elev}`, set by
    /// `handle_live_decoded_outcome`). Lets `request_worker_render` avoid
    /// blanking a valid partial during a between-chunks frame, and lets
    /// `sync_prev_sweep_texture` tell a live partial (whose prev slot the
    /// LiveDecoded promote owns) from a cached cut shown during live.
    pub(crate) fn gpu_holds_live_sweep(&self, site: usize) -> bool {
        self.sites.list[site]
            .gpu
            .radar
            .as_ref()
            .and_then(|renderer| {
                renderer
                    .lock()
                    .ok()
                    .and_then(|r| r.current_sweep_id().map(|id| id.starts_with("live|")))
            })
            .unwrap_or(false)
    }

    /// Request a volume render (all elevations for ray marching) of site
    /// `site`'s active scan.
    pub(crate) fn request_worker_render_volume(&mut self, site: usize) {
        let product = self.state.viz_state.product.to_worker_string().to_string();
        let track = &mut self.sites.list[site].render;
        self.render
            .coordinator
            .request_volume_render(track, &product);
    }

    /// Stop live mode streaming.
    pub(crate) fn stop_live_mode(&mut self, reason: crate::core::LiveExitReason) {
        log::info!("Stopping live mode: {:?}", reason);

        self.live.stop(&mut self.sites, reason);
        self.playback.state.exit_live(crate::core::FreezeAt::Keep);
        for site in self.sites.iter() {
            site.live.channel.stop();
        }

        // Halt playback: every remaining stop path (explicit stop, error,
        // site change) lands the cursor where it was, and leaving
        // playing=true at Realtime speed would keep it pacing "now" as if
        // still pinned. (Seek/jog no longer stop the stream — they detach.)
        self.playback.state.playing = false;
    }

    /// Handle a realtime streaming result from the channel of the site at
    /// `site` in the list. The result lands on that site's live state; only
    /// the focus site moves the session phase past acquiring.
    pub(crate) fn handle_realtime_result(
        &mut self,
        site: usize,
        result: nexrad::RealtimeResult,
        _ctx: &egui::Context,
    ) {
        let now = self.state.frame_now.secs();
        let from_focus = site == self.sites.focus;

        match result {
            nexrad::RealtimeResult::Started { site_id } => {
                log::debug!("Realtime streaming started for site: {}", site_id);
                self.live.mode_state.apply_site_event(
                    crate::core::SiteStreamEvent::Started,
                    from_focus,
                    now,
                );
            }
            nexrad::RealtimeResult::ChunkReceived {
                chunks_in_volume,
                is_volume_end,
                fetch_latency_ms,
                plan,
                arrival_stat,
            } => {
                if self.state.dev_mode {
                    self.state
                        .session_stats
                        .record_fetch_latency(fetch_latency_ms);
                }
                log::debug!(
                    "Realtime status: chunks_in_volume={} is_end={} latency={:.0}ms next_in={:?}",
                    chunks_in_volume,
                    is_volume_end,
                    fetch_latency_ms,
                    plan.as_ref().and_then(|p| p.next_available_in_secs(now)),
                );
                let awaiting_target = plan.as_ref().and_then(|p| p.next_target()).is_some();
                self.live.mode_state.apply_site_event(
                    crate::core::SiteStreamEvent::Chunk { awaiting_target },
                    from_focus,
                    now,
                );
                let volume = &mut self.sites.list[site].live.volume;
                volume.chunks_received = chunks_in_volume;
                if let Some(stat) = arrival_stat {
                    volume.record_chunk_arrival(stat);
                }

                // Record chunk latency for the acquisition drawer. No radial
                // timestamp is known at download time, so e2e stays None here.
                self.acquisition.state.record_chunk_latency(
                    chunks_in_volume,
                    fetch_latency_ms,
                    None,
                );
            }
            nexrad::RealtimeResult::ChunkData {
                data,
                chunk_index,
                source_sequence,
                elevation_number,
                chunk_index_in_sweep,
                chunks_in_sweep,
                is_start,
                is_end,
                timestamp,
                is_last_in_sweep,
            } => {
                log::debug!(
                    "Realtime chunk received: index={} is_start={} is_end={} size={} bytes ts={}",
                    chunk_index,
                    is_start,
                    is_end,
                    data.len(),
                    timestamp,
                );

                // Track realtime chunk as an acquisition operation
                let rt_site_id = self.sites.list[site].id.0.clone();
                let op_id =
                    self.acquisition
                        .state
                        .create_operation(state::OperationKind::RealtimeChunk {
                            site_id: rt_site_id,
                            chunk_index,
                            is_start,
                            is_end,
                            // The Network-tab grouping key is per-volume,
                            // not per-instant; truncating sub-second
                            // precision here is fine because two distinct
                            // volumes never share a wall-clock second.
                            scan_timestamp: timestamp.round() as i64,
                        });
                self.acquisition.state.mark_active(op_id);
                self.acquisition
                    .state
                    .mark_completed(op_id, data.len() as u64);

                if is_start {
                    log::debug!("Realtime: new volume started, forwarding start chunk to worker");
                }

                // Forward chunk to worker for incremental ingest
                let site_id = self.sites.list[site].id.0.clone();
                let file_name = format!("live_{}_{}.nexrad", site_id, timestamp);
                if is_start {
                    self.state.session_stats.pipeline.processing = true;
                }

                // The streaming loop derives `is_last_in_sweep` from the VCP
                // mapper at emission time (so it's correct even under filter
                // mode where chunk_index no longer maps 1:1 to sequence).
                let is_last_in_sweep = is_last_in_sweep.unwrap_or(false);

                log::debug!(
                    "Realtime: forwarding chunk {} to worker for ingest (site={}, ts={}, last_in_sweep={})",
                    chunk_index,
                    site_id,
                    timestamp,
                    is_last_in_sweep,
                );
                self.render.coordinator.ingest_chunk(
                    data,
                    site_id,
                    timestamp,
                    chunk_index,
                    source_sequence,
                    elevation_number,
                    chunk_index_in_sweep,
                    chunks_in_sweep,
                    is_start,
                    is_end,
                    file_name,
                    is_last_in_sweep,
                );
            }
            nexrad::RealtimeResult::Error(msg) => {
                log::error!("Realtime streaming error: {}", msg);
                // A failed channel stops its own site; the session stops
                // with the last site still streaming.
                let others_streaming = self
                    .sites
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != site)
                    .map(|(_, other)| other.live.channel.is_active());
                if crate::core::session_stream_ended(others_streaming) {
                    self.stop_live_mode(crate::core::LiveExitReason::ConnectionError);
                    // Preserve error message (stop_live_mode clears it)
                    self.live.mode_state.error_message = Some(msg.clone());
                } else {
                    let live = &mut self.sites.list[site].live;
                    live.channel.stop();
                    live.volume.clear();
                    live.engine.borrow_mut().reset_volume_observations();
                }

                // Track error as a failed acquisition operation
                let err_site_id = self.sites.list[site].id.0.clone();
                let op_id =
                    self.acquisition
                        .state
                        .create_operation(state::OperationKind::RealtimeChunk {
                            site_id: err_site_id,
                            chunk_index: 0,
                            is_start: false,
                            is_end: false,
                            scan_timestamp: 0,
                        });
                self.acquisition.state.mark_failed(op_id, msg);
            }
        }
    }
}
