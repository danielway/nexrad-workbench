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
        let site_id = self.state.viz_state.site_id.clone();
        log::info!("Starting live mode for site: {}", site_id);

        // Get current time
        let now = js_sys::Date::now() / 1000.0;

        // Initialize live mode state. Going live pins the playhead to "now"
        // (the realtime lock) but does NOT start playback — play/pause is
        // decoupled and, while live, drives the lookback replay instead. The
        // playhead is kept on "now" by `tick_live`, independent of `playing`.
        self.live.mode_state.start(now);
        self.playback.state.clear_lookback();
        self.playback.state.set_playback_position(now);
        self.playback.state.time_model.enable_realtime_lock();

        // Ensure the timeline is zoomed in far enough to show individual sweeps
        // and chunks. Live mode enforces micro-mode as the widest allowed zoom.
        const LIVE_DEFAULT_ZOOM: f64 = 2.0;
        if self.playback.state.timeline_zoom < LIVE_DEFAULT_ZOOM {
            self.playback.state.timeline_zoom = LIVE_DEFAULT_ZOOM;
            self.playback.state.center_view_on(now);
        }

        self.state.status_message = "Connecting to live stream...".to_string();

        // Push the current elevation filter into the channel before starting
        // so the streaming loop's init-time backfill targets the user's
        // selected elevation rather than a default.
        self.live
            .channel
            .sync_filter(&self.state.viz_state.elevation_selection);
        self.live.channel.start(
            ctx.clone(),
            site_id,
            self.acquisition.coordinator.facade().clone(),
            self.live.engine.clone(),
        );
    }

    /// Per-frame live tick — drives the playhead while streaming, independent
    /// of the `playing` flag (which now belongs to playback/lookback).
    ///
    /// - LIVE-NOW (realtime lock on): pin the playhead to wall-clock now.
    /// - LIVE-LOOKBACK (lock off, `lookback_active`): slide the loop window so
    ///   its end follows the latest frame as new volumes complete. `advance`
    ///   (driven by `playing` in the bottom panel) does the actual looping.
    ///
    /// In both states, keep the live edge on-screen. No-op when not live.
    pub(crate) fn tick_live(&mut self) {
        if !self.live.mode_state.is_active() {
            return;
        }
        let now = crate::state::TimeModel::wall_clock_time();

        if self.playback.state.time_model.locked_to_realtime {
            // LIVE-NOW: pin to wall-clock now.
            self.playback.state.time_model.snap_to_now();
        } else if self.playback.state.lookback_active {
            // LIVE-LOOKBACK: own the frame window. Prefer the exact last-N-frame
            // span; before any matching frame is cached, fall back to a time
            // window of ~N volumes so `render_loop` builds `sweep_frames` from
            // recent data — not all history. `render_loop` turns these bounds
            // into the macro frame list; the backfill pump fills the window in,
            // and this widens to the precise span as frames land.
            let (start, end) = self
                .timeline
                .scans
                .lookback_window(
                    &self.state.viz_state.elevation_selection,
                    now,
                    crate::LOOKBACK_FRAMES,
                )
                .unwrap_or((now - crate::LOOKBACK_SPAN_SECS, now));
            self.playback
                .state
                .time_model
                .set_bounds_preserving(start, end);
        }

        self.keep_now_on_screen(now);
    }

    /// Nudge the timeline view minimally so "now" stays visible. Pan/zoom is
    /// otherwise free; this only fires when "now" would fall outside the
    /// visible range. (Relocated from the old `playing`-gated block in the
    /// bottom panel so it runs in both LIVE-NOW and LIVE-LOOKBACK.)
    fn keep_now_on_screen(&mut self, now: f64) {
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

    /// Build the elevation list from a scan's VCP data.
    pub(crate) fn build_elevation_list(
        scan: &crate::state::radar_data::Scan,
    ) -> Vec<crate::state::ElevationListEntry> {
        state::playback_manager::build_elevation_list(scan)
    }

    /// Update the canvas overlay text with sweep timing and elevation info.
    pub(crate) fn update_overlay_from_sweep(&mut self, start: f64, end: f64, elevation_deg: f32) {
        self.state
            .viz_state
            .update_overlay(start, end, elevation_deg, self.state.use_local_time);
    }

    /// Send a render request to the worker for the current scan/elevation/product.
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
    pub(crate) fn request_worker_render(&mut self) {
        use state::playback_manager::DesiredDisplay;

        let desired = state::playback_manager::resolve_desired_display(
            &self.state.viz_state.site_id,
            self.playback.state.playback_position(),
            &self.state.viz_state.elevation_selection,
            self.state.viz_state.product,
            &self.timeline.scans,
            MAX_SCAN_AGE_SECS,
            self.live_render_sources(),
        );

        match desired {
            DesiredDisplay::LivePartial { .. } => {
                // The chunk-ingest → `render_live` path already owns the GPU
                // for the actively-collecting cut. Don't request a cached blob
                // for it and don't blank — leave the live partial in place.
            }
            DesiredDisplay::Cached(identity) => {
                if self.render.coordinator.request_render_for(identity)
                    && !self.state.session_stats.pipeline.processing
                {
                    self.state.session_stats.pipeline.processing = true;
                }
            }
            DesiredDisplay::Blank => {
                // Don't clear a valid live partial: in a between-chunks frame
                // the in-progress elevation can momentarily read `None` and
                // resolve to `Blank` even though the GPU holds a good `live|*`
                // sweep. Only blank when archive owns the canvas.
                let protecting_live_partial =
                    self.live.mode_state.is_active() && self.gpu_holds_live_sweep();
                if !protecting_live_partial {
                    self.clear_display_no_sweep();
                }
            }
        }
    }

    /// The live cut feeding [`state::playback_manager::resolve_desired_display`]:
    /// `Some((collecting_elevation, anchor_key_ms))` while streaming with a
    /// fully-known volume, else `None` (which collapses the resolver to the
    /// cache path). Shared by `request_worker_render` and the `Decoded` upload
    /// gate in `handle_decoded_outcome`.
    pub(crate) fn live_render_sources(&self) -> Option<(u8, i64)> {
        if !self.live.mode_state.is_active() {
            return None;
        }
        let anchor_ms = self
            .live
            .mode_state
            .current_volume
            .as_ref()
            .map(|a| a.scan_key.scan_start.0);
        self.live
            .engine
            .borrow()
            .observations()
            .current_in_progress_elevation
            .zip(anchor_ms)
    }

    /// Whether the main GPU texture currently holds a live partial sweep
    /// (`current_sweep_id` like `live|{elev}`, set by
    /// `handle_live_decoded_outcome`). Lets `request_worker_render` avoid
    /// blanking a valid partial during a between-chunks frame, and lets
    /// `sync_prev_sweep_texture` tell a live partial (whose prev slot the
    /// LiveDecoded promote owns) from a cached cut shown during live.
    pub(crate) fn gpu_holds_live_sweep(&self) -> bool {
        self.gpu
            .gpu
            .as_ref()
            .and_then(|renderer| {
                renderer
                    .lock()
                    .ok()
                    .and_then(|r| r.current_sweep_id().map(|id| id.starts_with("live|")))
            })
            .unwrap_or(false)
    }

    /// Request volume render (all elevations for ray marching).
    pub(crate) fn request_worker_render_volume(&mut self) {
        let product = self.state.viz_state.product.to_worker_string().to_string();
        self.render.coordinator.request_volume_render(&product);
    }

    /// Stop live mode streaming.
    #[allow(dead_code)] // Called from UI when user stops live mode
    pub(crate) fn stop_live_mode(&mut self, reason: state::LiveExitReason) {
        log::info!("Stopping live mode: {:?}", reason);

        self.live.stop(reason);
        self.playback.state.time_model.disable_realtime_lock();
        self.playback.state.clear_lookback();
        self.live.channel.stop();

        // Halt playback unless the user is actively scrubbing/jogging — those
        // paths set the new position themselves. Without this, we leave
        // playing=true at Realtime speed and position=wall-clock, so the
        // cursor keeps pace with "now" and mimics still being locked.
        if !matches!(
            reason,
            state::LiveExitReason::UserSeeked | state::LiveExitReason::UserJogged
        ) {
            self.playback.state.playing = false;
        }

        self.state.status_message = self
            .live
            .mode_state
            .last_exit_reason
            .map(|r| r.message().to_string())
            .unwrap_or_default();
    }

    /// Handle a realtime streaming result.
    pub(crate) fn handle_realtime_result(
        &mut self,
        result: nexrad::RealtimeResult,
        _ctx: &egui::Context,
    ) {
        // Get current time
        let now = js_sys::Date::now() / 1000.0;

        match result {
            nexrad::RealtimeResult::Started { site_id } => {
                log::debug!("Realtime streaming started for site: {}", site_id);
                self.live.mode_state.handle_streaming_started(now);
                self.state.status_message = format!("Live: connected to {}", site_id);
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
                self.live.mode_state.handle_realtime_chunk(
                    chunks_in_volume,
                    is_volume_end,
                    now,
                    plan.as_ref(),
                );

                if let Some(stat) = arrival_stat {
                    self.live.mode_state.record_chunk_arrival(stat);
                }

                // Record chunk latency for the acquisition drawer
                self.acquisition.state.record_chunk_latency(
                    chunks_in_volume,
                    fetch_latency_ms,
                    None, // radial timestamps populated after ingest
                    None,
                );
            }
            nexrad::RealtimeResult::ChunkData {
                data,
                chunk_index,
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
                let rt_site_id = self.state.viz_state.site_id.clone();
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
                    self.state.status_message = "Live: receiving new volume...".to_string();
                    log::debug!("Realtime: new volume started, forwarding start chunk to worker");
                }

                // Forward chunk to worker for incremental ingest
                let site_id = self.state.viz_state.site_id.clone();
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
                    is_start,
                    is_end,
                    file_name,
                    is_last_in_sweep,
                );
            }
            nexrad::RealtimeResult::Error(msg) => {
                log::error!("Realtime streaming error: {}", msg);
                self.stop_live_mode(state::LiveExitReason::ConnectionError);
                // Preserve error message (stop_live_mode clears it)
                self.live.mode_state.error_message = Some(msg.clone());
                self.state.status_message = format!("Live error: {}", msg);

                // Track error as a failed acquisition operation
                let err_site_id = self.state.viz_state.site_id.clone();
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
