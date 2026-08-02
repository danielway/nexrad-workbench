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

        for outcome in self.render.coordinator.try_recv() {
            match outcome {
                nexrad::WorkerOutcome::Ingested(result) => {
                    self.handle_ingested_outcome(result);
                }
                nexrad::WorkerOutcome::ChunkIngested(result) => {
                    self.handle_chunk_ingested_outcome(result);
                }
                nexrad::WorkerOutcome::Decoded(result) => {
                    self.handle_decoded_outcome(result);
                }
                nexrad::WorkerOutcome::LiveDecoded(result) => {
                    self.handle_live_decoded_outcome(result);
                }
                nexrad::WorkerOutcome::VolumeDecoded(volume_data) => {
                    self.handle_volume_decoded_outcome(volume_data);
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

        if let Some(result) = self.acquisition.coordinator.download_channel.try_recv() {
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

                // Build timeline from metadata
                self.timeline.scans = RadarTimeline::from_metadata(metadata);

                // Reconcile the request ledger against what the timeline now
                // actually holds: satisfied requests retire, ingested-but-
                // missing cuts become Unavailable, stale entries age out.
                self.acquisition.request_ledger.observe_timeline(
                    &self.timeline.scans,
                    crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
                    js_sys::Date::now(),
                );

                // Get time ranges (may be non-contiguous)
                let ranges = self.timeline.scans.time_ranges();
                if !ranges.is_empty() {
                    // Position playback at the end of the most recent range
                    let most_recent_end = ranges.last().unwrap().end;

                    // Only auto-position on initial load or site change,
                    // not when refreshing after a download.
                    if self.state.auto_position_on_timeline_load {
                        self.state.auto_position_on_timeline_load = false;
                        let ts = self.playback.state.playback_position();
                        let in_any_range = ranges.iter().any(|r| r.contains(ts));
                        if !in_any_range {
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

    /// Set the active scan for rendering — the scan key + elevation list
    /// owned by `RenderCoordinator`. The on-GPU `viz_state.displayed`
    /// slot is updated separately by the decode-result handlers once
    /// the worker actually delivers pixels for the requested sweep.
    pub(crate) fn set_active_scan(
        &mut self,
        scan_key: data::ScanKey,
        elevations: Vec<u8>,
        _displayed_ts: f64,
    ) {
        self.render.coordinator.set_scan(scan_key, elevations);
    }

    /// Like [`Self::set_active_scan`] but for the live chunk-by-chunk
    /// path where the elevation list grows incrementally.
    pub(crate) fn advance_active_scan_chunk(
        &mut self,
        scan_key: data::ScanKey,
        new_elevations: &[u8],
        _displayed_ts: f64,
    ) {
        self.render.coordinator.set_scan_key(scan_key);
        self.render.coordinator.add_elevations(new_elevations);
    }

    /// Clear the active scan. Used when no scan is in range, on site
    /// change, and on error paths.
    pub(crate) fn clear_active_scan(&mut self) {
        self.render.coordinator.clear_scan_key();
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

        // The scan's data is now in IDB but the timeline hasn't observed it
        // yet — hold the ledger entry in AwaitingTimeline across that gap
        // (matched within tolerance because this key is the re-keyed
        // volume-header time, not the listing timestamp it was enqueued as).
        self.acquisition.request_ledger.note_ingested(
            scan_start_secs as i64,
            crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
            js_sys::Date::now(),
        );
        self.set_active_scan(
            result.scan_key.clone(),
            result.elevation_numbers,
            scan_start_secs,
        );
        // Refresh timeline to include the new scan
        self.state
            .push_command(crate::core::Intent::RefreshTimeline {
                auto_position: false,
            });

        // Request eviction check
        self.state.push_command(crate::core::Intent::CheckEviction);

        // Force a fresh render
        self.render.coordinator.force_fresh_render();

        // Trigger render for the ingested scan
        self.request_worker_render();
        if self.volume_3d_active() {
            self.request_worker_render_volume();
        }
    }

    fn handle_chunk_ingested_outcome(&mut self, result: ChunkIngestResult) {
        use crate::core::worker_ingest::{
            reduce_chunk_ingested, ChunkIngestEnv, ChunkIngestSlices,
        };

        let is_live = self.live.mode_state.is_active();

        // Update scan key, growing elevation list, and displayed timestamp
        // through the single owner so they can never drift.
        let had_elevations = !self.render.coordinator.available_elevations().is_empty();
        self.advance_active_scan_chunk(
            result.scan_key.clone(),
            &result.elevations_completed,
            result.context.timestamp_secs,
        );

        // Assemble the frame context and run the pure reducer over the core
        // state slices. All decisions (and the core-state mutations they
        // imply) happen in `reduce_chunk_ingested`; the shell only executes
        // the described actions below.
        let actions = {
            let env = ChunkIngestEnv {
                is_live,
                site_id: &self.state.viz_state.site_id,
                product_worker_string: self.state.viz_state.product.to_worker_string(),
                now_secs: self.state.frame_now.secs(),
                had_elevations,
                available_elevations: self.render.coordinator.available_elevations(),
                frame_projection: self.live.frame_projection.as_ref(),
                volume_3d_active: self.volume_3d_active(),
            };
            let mut engine = self.live.engine.borrow_mut();
            let slices = ChunkIngestSlices {
                live_mode: &mut self.live.mode_state,
                engine: &mut engine,
                elevation_selection: &mut self.state.viz_state.elevation_selection,
                playback: &mut self.playback.state,
            };
            reduce_chunk_ingested(env, slices, &result)
        };

        // Execute the described actions in the struct's field order.
        if let Some(secs) = actions.record_chunk_collection_end_secs {
            self.live.channel.record_chunk_collection_end_secs(secs);
        }
        if let Some(lag) = actions.record_availability_lag_secs {
            self.live.channel.record_availability_lag_secs(lag);
        }
        if let Some((elevation, product)) = actions.render_live {
            self.render.coordinator.render_live(elevation, product);
        }
        if actions.promote_prev_texture {
            if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
                if let Ok(mut r) = renderer.lock() {
                    r.promote_current_to_previous(gl);
                }
            }
        }
        if let Some(msg) = actions.status_message {
            self.state.status_message = msg;
        }
        for intent in actions.intents {
            self.state.push_command(intent);
        }
        if actions.mark_processing_done {
            self.state.session_stats.pipeline.mark_processing_done();
        }
        if actions.force_fresh_render {
            self.render.coordinator.force_fresh_render();
        }
        if actions.request_render {
            self.request_worker_render();
        }
        if actions.request_volume_render {
            self.request_worker_render_volume();
        }
    }

    fn handle_decoded_outcome(&mut self, result: DecodeResult) {
        use crate::core::worker_decoded::{reduce_decoded, DecodedEnv, DecodedSlices};

        // Processing complete → transition to rendering.
        self.state.session_stats.pipeline.mark_processing_done();
        self.state.session_stats.pipeline.rendering = true;

        // Assemble the frame context and run the pure reducer. All decisions
        // (current-scan gate, cache entry, pending-prev clearing, display
        // angle, ghost retention, overlay gate) happen in `reduce_decoded`;
        // the shell executes the described actions below — the GPU upload
        // block itself stays here.
        let actions = {
            let env = DecodedEnv {
                dev_mode: self.state.dev_mode,
                site_id: &self.state.viz_state.site_id,
                playback_position: self.playback.state.playback_position(),
                elevation_selection: &self.state.viz_state.elevation_selection,
                product: self.state.viz_state.product,
                max_scan_age_secs: MAX_SCAN_AGE_SECS,
                live_cut: self.live_render_sources(),
                sweep_animation: self.state.effective_sweep_animation(
                    &self.playback.state,
                    self.live.mode_state.is_active(),
                ),
                storm_cells_visible: self.state.viz_state.storm_cells_visible,
                in_flight_scans: &self.state.download_progress.in_flight_scans,
                pending_scans_empty: self.state.download_progress.pending_scans.is_empty(),
            };
            let slices = DecodedSlices {
                playback_manager: &mut self.render.playback_manager,
            };
            reduce_decoded(env, slices, &self.timeline.scans, &result)
        };

        // Execute the described actions in the struct's field order.
        if let Some(ms) = actions.record_render_time {
            self.state.session_stats.record_render_time(ms);
        }

        let t_gpu = web_time::Instant::now();
        let mut gpu_upload_succeeded = false;
        if actions.upload_to_gpu {
            if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
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

                    // Run storm cell detection if enabled
                    if actions.run_storm_cells {
                        self.state.viz_state.detected_storm_cells = r.detect_storm_cells(
                            self.state.viz_state.center_lat,
                            self.state.viz_state.center_lon,
                            self.state.viz_state.storm_cell_threshold_dbz,
                        );
                    }
                }
            }
        }
        let gpu_upload_ms = t_gpu.elapsed().as_secs_f64() * 1000.0;

        if gpu_upload_succeeded {
            self.state.viz_state.displayed = actions.displayed_on_upload;
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
            self.update_overlay_from_sweep(start, end, angle);
        }
    }

    fn handle_live_decoded_outcome(&mut self, result: DecodeResult) {
        use crate::core::worker_decoded::{
            reduce_live_decoded, reduce_live_decoded_azimuths, LiveDecodedEnv,
        };

        // Phase 1: display decisions. The GPU renderer's current sweep id
        // (the promote decision's input) is read here and passed in as env.
        let mut actions = {
            let gpu_current_sweep_id = self.gpu.gpu.as_ref().and_then(|renderer| {
                renderer
                    .lock()
                    .ok()
                    .and_then(|r| r.current_sweep_id().map(str::to_string))
            });
            let env = LiveDecodedEnv {
                playhead_attached: !self.live.is_detached(&self.playback.state),
                live_volume: self.live.radar_model.volume.as_ref(),
                gpu_current_sweep_id,
                storm_cells_visible: self.state.viz_state.storm_cells_visible,
            };
            reduce_live_decoded(env, &result)
        };

        // Execute the described actions: the GPU upload block stays here.
        if actions.upload_to_gpu {
            if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
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

                    // Roll semantics decided by the reducer: promote →
                    // snapshot `displayed` into `previous_displayed` (the
                    // canonical source for overlay/timeline prev info),
                    // else overwrite in place.
                    if let Some(new_displayed) = actions.new_displayed.take() {
                        if actions.promote_prev_texture {
                            let prior = self.state.viz_state.displayed.replace(new_displayed);
                            self.state.viz_state.previous_displayed = prior;
                        } else {
                            self.state.viz_state.displayed = Some(new_displayed);
                        }
                    }

                    if actions.run_storm_cells {
                        self.state.viz_state.detected_storm_cells = r.detect_storm_cells(
                            self.state.viz_state.center_lat,
                            self.state.viz_state.center_lon,
                            self.state.viz_state.storm_cell_threshold_dbz,
                        );
                    }
                }
            }
        }

        if let Some((start, end, angle)) = actions.update_overlay {
            self.update_overlay_from_sweep(start, end, angle);
        }

        // Phase 2: azimuth bookkeeping — runs attached or detached, after
        // the GPU upload so the log interleaving matches the inline order.
        reduce_live_decoded_azimuths(&mut self.live.mode_state, &result);
    }

    fn handle_volume_decoded_outcome(&mut self, volume_data: VolumeData) {
        log::debug!(
            "Volume decode complete: {} sweeps, {:.1}KB, product={}, {:.0}ms",
            volume_data.sweeps.len(),
            volume_data.buffer.len() as f64 / 1024.0,
            volume_data.product,
            volume_data.total_ms,
        );

        // Upload to volume ray renderer
        if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.volume_ray, &self.gpu.gl) {
            if let Ok(mut r) = renderer.lock() {
                r.update_volume(
                    gl,
                    &volume_data.buffer,
                    volume_data.word_size,
                    &volume_data.sweeps,
                    self.state.viz_state.center_lat,
                    self.state.viz_state.center_lon,
                );
            }
        }

        // Update LUT for the volume product
        if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
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
        self.state.status_message = format!("Worker error: {}", message);
        self.state.errors.push(crate::core::AppError::Worker {
            kind,
            message: message.clone(),
            scan_timestamp_secs: failed_scan_timestamp_secs,
        });

        // A failed ingest means the fetched scan never reached IDB — move
        // its ledger entry to the short Failed backoff so re-fetch becomes
        // possible without a per-frame storm.
        if let Some(ts) = failed_scan_timestamp_secs {
            self.acquisition.request_ledger.note_failed(
                ts as i64,
                crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
                js_sys::Date::now(),
            );
        }

        // When the worker reports that the requested (elevation, product) has
        // no pre-computed sweep, clear the stale canvas so the user sees what
        // the timeline already knows — nothing matches their current filter.
        // Dispatch on the typed kind so transient errors (worker disconnect,
        // IDB failure) keep the last-good view instead of blanking.
        if kind == crate::core::WorkerErrorKind::NotFound {
            self.clear_display_no_sweep();
        }

        // Clean up the "processing" timeline ghost for the failed scan.
        // Prefer the scan attributed to the failing worker request so the
        // right ghost is removed even after the user scrolled away and
        // the active scan key now points elsewhere.
        let cleanup_ts = failed_scan_timestamp_secs.or_else(|| {
            self.render
                .coordinator
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
