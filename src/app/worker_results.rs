//! Worker-result and cache-load outcome handlers.
//!
//! Drains responses from the decode worker pool (`render.try_recv()`),
//! the cache loader, and download channels. Each worker outcome variant
//! has its own `handle_*_outcome` method; this module hosts all of them
//! plus the small helpers (`set_active_scan`, `advance_active_scan_chunk`,
//! `clear_active_scan`) that bridge worker output and `RenderCoordinator`.

use crate::core::{
    CacheLoadResult, ChunkIngestResult, DecodeResult, IngestResult, RadarTimeline, VolumeData,
};
use crate::{data, nexrad, WorkbenchApp, MAX_SCAN_AGE_SECS};
use eframe::egui;

impl WorkbenchApp {
    pub(crate) fn handle_worker_results(&mut self, _ctx: &egui::Context) {
        if let Some(result) = self.acquisition.coordinator.cache_load_channel.try_recv() {
            self.handle_cache_load_outcome(result);
        }

        let outcomes = self.render.coordinator.try_recv();
        if !outcomes.is_empty() {
            // Liveness stamp for the worker-load staleness backstop: as long as
            // results keep arriving, a non-zero pending count is real work
            // rather than a leaked entry from a dead worker.
            self.state.session_stats.last_worker_outcome_ms = self.state.frame_now.millis();
        }
        // Several chunk acks can land in one drain (a backfill burst); post
        // at most one live render per streaming site for them, after the
        // loop (see `SiteLiveRenderCoalescer`).
        let mut live_renders = crate::core::worker_ingest::SiteLiveRenderCoalescer::default();
        for outcome in outcomes {
            match outcome {
                nexrad::WorkerOutcome::Ingested(result) => {
                    self.handle_ingested_outcome(result);
                }
                nexrad::WorkerOutcome::ChunkIngested(result) => {
                    let completed = result.elevations_completed.clone();
                    let site = result.scan_key.site.clone();
                    let render_live = self.handle_chunk_ingested_outcome(result);
                    live_renders.observe(&site, render_live, &completed);
                }
                nexrad::WorkerOutcome::Decoded(result) => {
                    self.handle_decoded_outcome(result);
                }
                nexrad::WorkerOutcome::LiveDecoded { site, result } => {
                    // A partial for a site no longer in the list (switched
                    // away while the render was in the worker) is dropped.
                    match crate::core::sites::route_site(&site, &self.sites.ids()) {
                        Some(site) => self.handle_live_decoded_outcome(site, result),
                        None => log::debug!("Live partial for {} dropped: site not shown", site),
                    }
                }
                nexrad::WorkerOutcome::VolumeDecoded(volume_data) => {
                    self.handle_volume_decoded_outcome(volume_data);
                }
                nexrad::WorkerOutcome::Detected(reply) => {
                    // The site whose request this answers; a reply nobody
                    // awaits any more (a site change reset it) is dropped.
                    let id = reply.request_id;
                    match self.sites.iter().position(|s| s.cells.awaits(id)) {
                        Some(site) => {
                            let disposition = self.sites.list[site].cells.accept(reply);
                            log::debug!("storm cells: reply {:?}", disposition);
                            self.sync_storm_cells_for(site);
                        }
                        None => log::debug!("storm cells: reply {id} dropped"),
                    }
                }
                nexrad::WorkerOutcome::DetectFailed {
                    request_id,
                    message,
                } => {
                    log::warn!("storm cells: detection failed: {message}");
                    let owner = self.sites.iter().position(|s| s.cells.awaits(request_id));
                    if let Some(site) = owner {
                        self.sites.list[site].cells.fail(request_id);
                        self.sync_storm_cells_for(site);
                    }
                }
                nexrad::WorkerOutcome::WorkerError {
                    id,
                    kind,
                    message,
                    failed_scan_timestamp_secs,
                } => {
                    self.handle_worker_error_outcome(id, kind, message, failed_scan_timestamp_secs);
                }
            }
        }
        for (site, elevation, product) in live_renders.finish() {
            self.render
                .coordinator
                .render_live(site, elevation, product);
        }

        // Byte progress first: a download's reports all precede its result.
        while let Some((start, progress)) = self
            .acquisition
            .coordinator
            .download_channel
            .try_recv_progress()
        {
            self.state.download_progress.bytes.record(start, progress);
        }
        if let Some(result) = self.acquisition.coordinator.download_channel.try_recv() {
            self.state
                .download_progress
                .bytes
                .finish(result.scan_start_secs());
            self.handle_download_outcome(result);
        }

        if let Some(result) = self
            .acquisition
            .coordinator
            .download_channel
            .try_recv_listing()
        {
            self.handle_listing_outcome(result);
        }
    }

    fn handle_cache_load_outcome(&mut self, result: CacheLoadResult) {
        match result {
            CacheLoadResult::Success {
                site_id,
                metadata,
                total_cache_size,
            } => {
                log::debug!(
                    "Timeline loaded from cache: {} scan(s) for site {}",
                    metadata.len(),
                    site_id
                );

                // Update cache size in session stats
                self.state.session_stats.cache_size_bytes = total_cache_size;

                // A load for a site no longer in the list is dropped.
                let Some(i) = crate::core::sites::route_site(
                    &crate::data::SiteId::new(site_id),
                    &self.sites.ids(),
                ) else {
                    return;
                };
                let site = &mut self.sites.list[i];

                // Build timeline from metadata
                site.scans = RadarTimeline::from_metadata(metadata);

                // Reconcile the request ledger against what the timeline now
                // actually holds: satisfied requests retire, ingested-but-
                // missing cuts become Unavailable, stale entries age out.
                site.ledger.observe_timeline(
                    &site.scans,
                    crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
                    js_sys::Date::now(),
                );

                // Get time ranges (may be non-contiguous). The playhead is
                // shared, so only the focus site's load positions it.
                let ranges = site.scans.time_ranges();
                if !ranges.is_empty() && i == self.sites.focus {
                    // Position playback at the end of the most recent range
                    let most_recent_end = ranges.last().unwrap().end;

                    // Only auto-position on initial load or site change,
                    // not when refreshing after a download.
                    if self.state.auto_position_on_timeline_load {
                        self.state.auto_position_on_timeline_load = false;
                        let ts = self.playback.state.playback_position();
                        let in_any_range = ranges.iter().any(|r| r.contains(ts));
                        // A live playhead follows now; repositioning it would
                        // trip the seek-while-live guard (first-visit pick
                        // with a warm cache).
                        if !in_any_range && !self.playback.state.selection.is_live() {
                            self.playback.state.set_playback_position(most_recent_end);
                            self.playback.state.center_view_on(most_recent_end);
                        }
                    }

                    log::debug!("Timeline has {} contiguous range(s)", ranges.len());
                }
            }
            CacheLoadResult::Error(msg) => {
                log::error!("Cache load failed: {}", msg);
            }
        }
    }

    /// Set site `site`'s active scan for rendering — the scan key +
    /// elevation list on its render track. The site's on-GPU `displayed`
    /// slot is updated separately by the decode-result handlers once the
    /// worker actually delivers pixels for the requested sweep.
    pub(crate) fn set_active_scan(
        &mut self,
        site: usize,
        scan_key: data::ScanKey,
        elevations: Vec<u8>,
    ) {
        self.sites.list[site].render.set_scan(scan_key, elevations);
    }

    /// Like [`Self::set_active_scan`] but for the live chunk-by-chunk
    /// path where the elevation list grows incrementally.
    pub(crate) fn advance_active_scan_chunk(
        &mut self,
        site: usize,
        scan_key: data::ScanKey,
        new_elevations: &[u8],
    ) {
        let track = &mut self.sites.list[site].render;
        track.set_scan_key(scan_key);
        track.add_elevations(new_elevations);
    }

    /// Clear site `site`'s active scan. Used when no scan is in range, on
    /// site change, and on error paths.
    pub(crate) fn clear_active_scan(&mut self, site: usize) {
        self.sites.list[site].render.clear_scan_key();
    }

    fn handle_ingested_outcome(&mut self, result: IngestResult) {
        // Processing stays active through decode — don't mark done yet.
        // Transition to decoding phase. Don't remove the ghost
        // yet — it stays visible until the timeline refreshes
        // and a real scan block replaces it (the ghost renderer's
        // overlap check handles the visual transition).
        self.state.download_progress.phase = crate::state::DownloadPhase::Decoding;
        log::debug!(
            "Ingest complete: {} ({} records, {} elevations, {} sweeps, {:.0}ms, fetch: {:.0}ms)",
            result.scan_key,
            result.records_stored,
            result.elevation_numbers.len(),
            result.sweeps.len(),
            result.total_ms,
            result.context.fetch_latency_ms,
        );

        if self.state.dev_mode {
            self.state
                .session_stats
                .record_fetch_latency(result.context.fetch_latency_ms);
            self.state
                .session_stats
                .record_processing_time(result.total_ms);

            // Store detailed ingest timing for the detail modal.
            self.state.session_stats.last_ingest_detail = Some(crate::state::IngestTimingDetail {
                split_ms: result.split_ms,
                decompress_ms: result.decompress_ms,
                decode_ms: result.decode_ms,
                extract_ms: result.extract_ms,
                store_ms: result.store_ms,
                index_ms: result.index_ms,
            });
        }

        // The worker returns the scan_key it actually keyed under — the
        // decoded volume-header time, not the dispatch-time filename. Use it,
        // and derive the displayed timestamp from it, so the render lookup
        // and timeline position match the stored blob exactly.
        let scan_start_secs = result.scan_key.scan_start.as_secs_f64();

        // Refresh timeline to include the new scan
        self.state
            .push_command(crate::core::Intent::RefreshTimeline {
                auto_position: false,
            });

        // Request eviction check
        self.state.push_command(crate::core::Intent::CheckEviction);

        // The rest is the ingesting site's; a scan for a site no longer in
        // the list is stored but not rendered.
        let Some(site) = crate::core::sites::route(&result.scan_key, &self.sites.ids()) else {
            log::debug!(
                "Ingest for {} not rendered: site not shown",
                result.scan_key
            );
            return;
        };

        // The scan's data is now in IDB but the timeline hasn't observed it
        // yet — hold the ledger entry in AwaitingTimeline across that gap
        // (matched within tolerance because this key is the re-keyed
        // volume-header time, not the listing timestamp it was enqueued as).
        self.sites.list[site].ledger.note_ingested(
            scan_start_secs as i64,
            crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
            js_sys::Date::now(),
        );
        self.set_active_scan(site, result.scan_key.clone(), result.elevation_numbers);

        // Force a fresh render
        self.sites.list[site].render.force_fresh_render();

        // Trigger render for the ingested scan
        self.request_worker_render(site);
        if self.volume_3d_active() {
            self.request_worker_render_volume(site);
        }
    }

    /// Returns the live render this acknowledgement asks for, which the
    /// caller coalesces over the drain instead of posting it here.
    fn handle_chunk_ingested_outcome(&mut self, result: ChunkIngestResult) -> Option<(u8, String)> {
        use crate::core::worker_ingest::{
            reduce_chunk_ingested, ChunkIngestEnv, ChunkIngestSlices,
        };

        let is_live = self.live.mode_state.is_active();

        // A chunk from a stream whose site is no longer in the list (it was
        // switched away while the chunk was in the worker) is dropped.
        let Some(site) = crate::core::sites::route(&result.scan_key, &self.sites.ids()) else {
            log::debug!("Chunk for {} dropped: site not shown", result.scan_key);
            return None;
        };

        // Update scan key and the growing elevation list through the single
        // owner so they can never drift.
        let had_elevations = !self.sites.list[site]
            .render
            .available_elevations()
            .is_empty();
        self.advance_active_scan_chunk(site, result.scan_key.clone(), &result.elevations_completed);

        // Assemble the frame context and run the pure reducer over the core
        // state slices. All decisions (and the core-state mutations they
        // imply) happen in `reduce_chunk_ingested`; the shell only executes
        // the described actions below.
        let actions = {
            let volume_3d_active = self.volume_3d_active();
            let is_focus = site == self.sites.focus;
            let target = &mut self.sites.list[site];
            let env = ChunkIngestEnv {
                is_live,
                site_id: &target.id.0,
                is_focus,
                product_worker_string: self.state.viz_state.product.to_worker_string(),
                now_secs: self.state.frame_now.secs(),
                had_elevations,
                available_elevations: target.render.available_elevations(),
                frame_projection: target.live.frame_projection.as_ref(),
                volume_3d_active,
            };
            let mut engine = target.live.engine.borrow_mut();
            let slices = ChunkIngestSlices {
                live_mode: &mut self.live.mode_state,
                volume: &mut target.live.volume,
                engine: &mut engine,
                elevation_selection: &mut self.state.viz_state.elevation_selection,
                playback: &mut self.playback.state,
            };
            reduce_chunk_ingested(env, slices, &result)
        };

        // Execute the described actions in the struct's field order.
        let channel = &self.sites.list[site].live.channel;
        if let Some(secs) = actions.record_chunk_collection_end_secs {
            channel.record_chunk_collection_end_secs(secs);
        }
        if let Some(lag) = actions.record_availability_lag_secs {
            channel.record_availability_lag_secs(lag);
        }
        if actions.promote_prev_texture {
            if let (Some(ref renderer), Some(ref gl)) =
                (&self.sites.list[site].gpu.radar, &self.gpu.gl)
            {
                if let Ok(mut r) = renderer.lock() {
                    r.promote_current_to_previous(gl);
                }
            }
        }
        for intent in actions.intents {
            self.state.push_command(intent);
        }
        if actions.mark_processing_done {
            self.state.session_stats.pipeline.mark_processing_done();
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
        actions.render_live
    }

    fn handle_decoded_outcome(&mut self, result: DecodeResult) {
        use crate::core::worker_decoded::{reduce_decoded, DecodedEnv, DecodedSlices};

        // Processing complete → transition to rendering.
        self.state.session_stats.pipeline.mark_processing_done();
        self.state.session_stats.pipeline.rendering = true;

        // A sweep for a site no longer in the list is dropped; only the
        // shared pipeline bookkeeping runs.
        let Some(site) = crate::core::sites::route(&result.context.scan_key, &self.sites.ids())
        else {
            log::debug!(
                "Decode for {} dropped: site not shown",
                result.context.scan_key
            );
            self.state.session_stats.pipeline.mark_render_done();
            self.retire_in_flight_scan(result.context.scan_key.scan_start.as_secs());
            return;
        };
        let live_cut = self.live_render_sources(site);

        // Assemble the frame context and run the pure reducer. All decisions
        // (current-scan gate, cache entry, pending-prev clearing, display
        // angle, ghost retention, overlay gate) happen in `reduce_decoded`;
        // the shell executes the described actions below — the GPU upload
        // block itself stays here.
        let whole_sweeps_only = self.live_shows_whole_sweeps();
        let actions = {
            let target = &mut self.sites.list[site];
            let env = DecodedEnv {
                dev_mode: self.state.dev_mode,
                site_id: &target.id.0,
                playback_position: self.playback.state.playback_position(),
                elevation_selection: &self.state.viz_state.elevation_selection,
                product: self.state.viz_state.product,
                max_scan_age_secs: MAX_SCAN_AGE_SECS,
                live_cut,
                whole_sweeps_only,
                sweep_animation: self.state.effective_sweep_animation(
                    &self.playback.state,
                    self.live
                        .canvas_follows_live(&target.live, &self.playback.state),
                    &target.live.radar_model,
                ),
                in_flight_scans: &self.state.download_progress.in_flight_scans,
                pending_scans_empty: self.state.download_progress.pending_scans.is_empty(),
            };
            let slices = DecodedSlices {
                playback_manager: &mut target.crossfade,
            };
            reduce_decoded(env, slices, &target.scans, &result)
        };

        // Execute the described actions in the struct's field order.
        if let Some(ms) = actions.record_render_time {
            self.state.session_stats.record_render_time(ms);
        }

        let t_gpu = web_time::Instant::now();
        let mut gpu_upload_succeeded = false;
        if actions.upload_to_gpu {
            if let (Some(ref renderer), Some(ref gl)) =
                (&self.sites.list[site].gpu.radar, &self.gpu.gl)
            {
                if let Ok(mut r) = renderer.lock() {
                    r.update_data(
                        gl,
                        &result.azimuths,
                        &result.gate_values,
                        result.azimuth_count,
                        result.gate_count,
                        result.first_gate_range_km,
                        result.gate_interval_km,
                        result.max_range_km,
                        result.offset,
                        result.scale,
                        result.azimuth_spacing_deg,
                        &result.radial_times,
                    );
                    r.set_current_sweep_id(Some(actions.gpu_sweep_id));
                    r.update_color_table(gl, &result.product);
                    gpu_upload_succeeded = true;
                }
            }
        }
        let gpu_upload_ms = t_gpu.elapsed().as_secs_f64() * 1000.0;

        if gpu_upload_succeeded {
            self.sites.list[site].displayed = actions.displayed_on_upload;
            // Detection for the new sweep, decided by the same rule as the
            // per-frame check (which then sees it as already computed).
            self.sync_storm_cells_for(site);
        }

        // Store detailed render timing for the detail modal (dev mode only).
        if actions.record_render_detail {
            self.state.session_stats.last_render_detail = Some(crate::state::RenderTimingDetail {
                fetch_ms: result.fetch_ms,
                deser_ms: result.deser_ms,
                marshal_ms: result.marshal_ms,
                gpu_upload_ms,
            });
        }

        // GPU upload complete.
        self.state.session_stats.pipeline.mark_render_done();

        self.state
            .download_progress
            .in_flight_scans
            .retain(|&(start, _)| start != actions.remove_in_flight_scan_start);
        if actions.clear_download_progress {
            self.state.download_progress.clear();
        }

        if let Some((start, end, angle)) = actions.update_overlay {
            self.update_overlay_from_sweep(site, start, end, angle);
        }
    }

    /// Drop a scan's in-flight ghost and, when nothing else is in flight or
    /// pending, the download progress with it.
    fn retire_in_flight_scan(&mut self, scan_start: i64) {
        let progress = &mut self.state.download_progress;
        progress
            .in_flight_scans
            .retain(|&(start, _)| start != scan_start);
        if progress.in_flight_scans.is_empty() && progress.pending_scans.is_empty() {
            progress.clear();
        }
    }

    fn handle_live_decoded_outcome(&mut self, site: usize, result: DecodeResult) {
        use crate::core::worker_decoded::{
            reduce_live_decoded, reduce_live_decoded_azimuths, LiveDecodedEnv,
        };

        // Phase 1: display decisions. The GPU renderer's current sweep id
        // (the promote decision's input) is read here and passed in as env.
        let mut actions = {
            let gpu_current_sweep_id =
                self.sites.list[site]
                    .gpu
                    .radar
                    .as_ref()
                    .and_then(|renderer| {
                        renderer
                            .lock()
                            .ok()
                            .and_then(|r| r.current_sweep_id().map(str::to_string))
                    });
            let live = &self.sites.list[site].live;
            let env = LiveDecodedEnv {
                playhead_attached: self.live.canvas_follows_live(live, &self.playback.state),
                whole_sweeps_only: self.live_shows_whole_sweeps(),
                live_volume: live.radar_model.volume.as_ref(),
                gpu_current_sweep_id,
            };
            reduce_live_decoded(env, &result)
        };

        // Execute the described actions: the GPU upload block stays here.
        if actions.upload_to_gpu {
            let mut uploaded = false;
            if let (Some(ref renderer), Some(ref gl)) =
                (&self.sites.list[site].gpu.radar, &self.gpu.gl)
            {
                if let Ok(mut r) = renderer.lock() {
                    if actions.promote_prev_texture {
                        r.promote_current_to_previous(gl);
                    }

                    r.update_data(
                        gl,
                        &result.azimuths,
                        &result.gate_values,
                        result.azimuth_count,
                        result.gate_count,
                        result.first_gate_range_km,
                        result.gate_interval_km,
                        result.max_range_km,
                        result.offset,
                        result.scale,
                        result.azimuth_spacing_deg,
                        &result.radial_times,
                    );
                    r.set_current_sweep_id(Some(actions.gpu_sweep_id));
                    r.update_color_table(gl, &result.product);
                    uploaded = true;
                }
            }
            // Roll semantics decided by the reducer: promote → snapshot
            // `displayed` into `previous_displayed` (the canonical source for
            // overlay/timeline prev info), else overwrite in place. Written
            // after the renderer borrow ends, since both live on the site.
            if let Some(new_displayed) = actions.new_displayed.take().filter(|_| uploaded) {
                let target = &mut self.sites.list[site];
                if actions.promote_prev_texture {
                    target.previous_displayed = target.displayed.replace(new_displayed);
                } else {
                    target.displayed = Some(new_displayed);
                }
            }
            // Detection for the new chunk, decided by the same rule as the
            // per-frame check. Runs after the lock above is released.
            self.sync_storm_cells_for(site);
        }

        if let Some((start, end, angle)) = actions.update_overlay {
            self.update_overlay_from_sweep(site, start, end, angle);
        }

        // Phase 2: azimuth bookkeeping — runs attached or detached, after
        // the GPU upload so the log interleaving matches the inline order.
        reduce_live_decoded_azimuths(&mut self.sites.list[site].live.volume, &result);
    }

    fn handle_volume_decoded_outcome(&mut self, volume_data: VolumeData) {
        log::debug!(
            "Volume decode complete: {} sweeps, {:.1}KB, product={}, {:.0}ms",
            volume_data.sweeps.len(),
            volume_data.buffer.len() as f64 / 1024.0,
            volume_data.product,
            volume_data.total_ms,
        );

        // A volume reply names no site; the 3D view draws the focus.
        let target = self.sites.focus();

        // Upload to volume ray renderer
        if let (Some(ref renderer), Some(ref gl)) = (&target.gpu.volume_ray, &self.gpu.gl) {
            if let Ok(mut r) = renderer.lock() {
                r.update_volume(
                    gl,
                    &volume_data.buffer,
                    volume_data.word_size,
                    &volume_data.sweeps,
                    target.lat,
                    target.lon,
                );
            }
        }

        // Update LUT for the volume product
        if let (Some(ref renderer), Some(ref gl)) = (&target.gpu.radar, &self.gpu.gl) {
            if let Ok(mut r) = renderer.lock() {
                r.update_color_table(gl, &volume_data.product);
            }
        }
    }

    fn handle_worker_error_outcome(
        &mut self,
        id: u64,
        kind: crate::core::WorkerErrorKind,
        message: String,
        failed_scan_timestamp_secs: Option<f64>,
    ) {
        log::warn!(
            "Worker error (request {}, kind {:?}): {}",
            id,
            kind,
            message
        );
        // A worker error names no site; downloads are the focus's.
        let focus = self.sites.focus_mut();
        match focus
            .retries
            .decide(failed_scan_timestamp_secs.map(|ts| ts as i64), &message)
        {
            // A truncated download: no error, no backoff. Forget the request
            // and its finished queue item so the pumps download the scan
            // again if it is still wanted (the anchor pump does so on the
            // next frame while the playhead is on it).
            crate::core::acquisition::IngestFailureAction::RetryQuietly { scan_start } => {
                log::info!("Truncated archive for scan {scan_start}; downloading it again");
                focus
                    .ledger
                    .forget(scan_start, crate::SCAN_CACHE_MATCH_TOLERANCE_SECS);
                self.acquisition
                    .coordinator
                    .download_queue
                    .forget_done(scan_start);
            }
            crate::core::acquisition::IngestFailureAction::Report => {
                self.state.errors.push(crate::core::AppError::Worker {
                    kind,
                    message: message.clone(),
                    scan_timestamp_secs: failed_scan_timestamp_secs,
                });

                // A failed ingest means the fetched scan never reached IDB —
                // move its ledger entry to the short Failed backoff so
                // re-fetch becomes possible without a per-frame storm.
                if let Some(ts) = failed_scan_timestamp_secs {
                    focus.ledger.note_failed(
                        ts as i64,
                        crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
                        js_sys::Date::now(),
                    );
                }
            }
        }

        // When the worker reports that the requested (elevation, product) has
        // no pre-computed sweep, clear the stale canvas so the user sees what
        // the timeline already knows — nothing matches their current filter.
        // Dispatch on the typed kind so transient errors (worker disconnect,
        // IDB failure) keep the last-good view instead of blanking.
        if kind == crate::core::WorkerErrorKind::NotFound {
            self.clear_display_no_sweep(self.sites.focus);
        }

        // Clean up the "processing" timeline ghost for the failed scan.
        // Prefer the scan attributed to the failing worker request so the
        // right ghost is removed even after the user scrolled away and
        // the active scan key now points elsewhere.
        let cleanup_ts = failed_scan_timestamp_secs.or_else(|| {
            self.sites
                .focus()
                .render
                .scan_key()
                .map(|k| k.scan_start.as_secs_f64())
        });
        if let Some(ts) = cleanup_ts {
            // The in_flight_scans queue is keyed by archive-derived i64
            // seconds; truncate the failure timestamp on this comparison
            // boundary. Sub-second precision matters for live volumes but
            // not for archive-style ghost removal.
            let ts_i64 = ts.round() as i64;
            self.state
                .download_progress
                .in_flight_scans
                .retain(|&(start, _)| start != ts_i64);
        }
        self.state.session_stats.pipeline.processing = false;
        self.state.session_stats.pipeline.rendering = false;
        if self.state.download_progress.in_flight_scans.is_empty()
            && self.state.download_progress.pending_scans.is_empty()
        {
            self.state.download_progress.clear();
        }
    }
}
