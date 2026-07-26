//! Worker-result and cache-load outcome handlers.
//!
//! Drains responses from the decode worker pool (`render.try_recv()`),
//! the cache loader, and download channels. Each worker outcome variant
//! has its own `handle_*_outcome` method; this module hosts all of them
//! plus the small helpers (`set_active_scan`, `advance_active_scan_chunk`,
//! `clear_active_scan`) that bridge worker output and `RenderCoordinator`.

use crate::core::playback_manager::{sweep_cache_key, CachedSweepData};
use crate::core::{
    CacheLoadResult, ChunkIngestResult, DecodeResult, IngestResult, RadarTimeline, SweepIdentity,
    VolumeData,
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
        if self.state.viz_state.volume_3d_enabled {
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
                volume_3d_enabled: self.state.viz_state.volume_3d_enabled,
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
        // Processing complete → transition to rendering.
        self.state.session_stats.pipeline.mark_processing_done();
        self.state.session_stats.pipeline.rendering = true;

        log::debug!(
            "Decode complete: {}x{} (az x gates), {} radials, product={}, {:.0}ms",
            result.azimuth_count,
            result.gate_count,
            result.radial_count,
            result.product,
            result.total_ms,
        );

        if self.state.dev_mode {
            self.state.session_stats.record_render_time(result.total_ms);
        }

        // Cache decoded data for stateless sweep animation
        let result_sweep_id = sweep_cache_key(
            &result.context.scan_key.to_storage_key(),
            result.context.elevation_number,
            &result.product,
        );
        self.render.playback_manager.cache_sweep(
            result_sweep_id.clone(),
            CachedSweepData {
                gate_values: result.gate_values.clone(),
                azimuths: result.azimuths.clone(),
                azimuth_count: result.azimuth_count,
                gate_count: result.gate_count,
                first_gate_range_km: result.first_gate_range_km,
                gate_interval_km: result.gate_interval_km,
                max_range_km: result.max_range_km,
                offset: result.offset,
                scale: result.scale,
                azimuth_spacing_deg: result.azimuth_spacing_deg,
                radial_times: result.radial_times.clone(),
                product: result.product.clone(),
            },
        );

        // Upload decoded data to GPU renderer — but only if this
        // result is for the currently displayed scan. Background
        // prev-sweep decodes are cached but not uploaded here;
        // sync_prev_sweep_texture picks them up next frame.
        // Only upload to the primary GPU texture if this result
        // matches what advance_playback intended: same scan key AND
        // same elevation number. Without the elevation check, SAILS
        // VCPs (duplicate 0.5° at elev 1 and 2) cause oscillation
        // where prefetch/sync requests fight the main render path.
        //
        // Re-run the resolver against current playback state and compare
        // identities: the result is for the main slot iff its identity
        // exactly matches what the resolver would request right now.
        // Stale results from rapid clicks or in-flight prefetches fail
        // this check and stay cached (for prev-sweep upload) without
        // clobbering the main GPU texture.
        let result_identity = SweepIdentity::new(
            result.context.scan_key.clone(),
            result.context.elevation_number,
            result.product.clone(),
        );
        // The result is for the main slot iff the unified resolver would
        // currently ask for exactly this cached sweep. When live is collecting
        // this cut the resolver returns `LivePartial`, so a cached `Decoded`
        // for it is *not* current and stays cached for prev-sweep use — it
        // never clobbers the live partial. This precedence replaces the old
        // `skip_gpu_upload = is_active()` mode flag: completed cached cuts now
        // upload during live, the actively-collecting cut does not.
        let desired = crate::core::playback_manager::resolve_desired_display(
            &self.state.viz_state.site_id,
            self.playback.state.playback_position(),
            &self.state.viz_state.elevation_selection,
            self.state.viz_state.product,
            &self.timeline.scans,
            MAX_SCAN_AGE_SECS,
            self.live_render_sources(),
        );
        let is_current_scan = desired
            == crate::core::playback_manager::DesiredDisplay::Cached(result_identity.clone());
        if self.state.effective_sweep_animation(&self.playback.state) && !is_current_scan {
            log::debug!("[sweep-anim] cached bg decode: {}", result_sweep_id);
            // Clear pending tracker so sync_prev_sweep_texture can load from cache
            if self.render.playback_manager.pending_prev_sweep_key() == Some(&result_sweep_id) {
                self.render
                    .playback_manager
                    .set_pending_prev_sweep_key(None);
            }
        }
        let t_gpu = web_time::Instant::now();
        let mut gpu_upload_succeeded = false;
        if is_current_scan {
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
                    r.set_current_sweep_id(Some(result_sweep_id));
                    r.update_color_table(gl, &result.product);
                    gpu_upload_succeeded = true;

                    // Run storm cell detection if enabled
                    if self.state.viz_state.storm_cells_visible {
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

        // Capture the new on-GPU main-slot sweep identity. The
        // *previous-slot* identity is owned by `sync_prev_sweep_texture`
        // (archive) or `handle_live_decoded_outcome`'s promote branch
        // (live) — those represent what's in the prev-sweep GPU texture,
        // which is the time-ordered prior sweep, NOT the prior main upload.
        // Don't conflate the two: scrubbing across non-adjacent times
        // would otherwise mis-mark the timeline previous border.
        // Display angle for the cut: prefer the VCP target ("commanded")
        // angle so labels read 0.5° rather than 0.44° (the encoder
        // average wobbles a few hundredths of a degree per spin).
        let display_angle = self
            .timeline
            .scans
            .find_scan_at_timestamp(result.context.scan_key.scan_start.as_secs_f64())
            .and_then(|scan| scan.target_elevation_angle(result.context.elevation_number))
            .unwrap_or(result.mean_elevation);

        if gpu_upload_succeeded {
            self.state.viz_state.displayed = Some(crate::core::DisplayedSweep {
                identity: result_identity.clone(),
                start_time: result.sweep_start_secs,
                end_time: result.sweep_end_secs,
                elevation_deg: display_angle,
            });
        }

        // Store detailed render timing for the detail modal (dev mode only).
        if self.state.dev_mode {
            self.state.session_stats.last_render_detail = Some(crate::state::RenderTimingDetail {
                fetch_ms: result.fetch_ms,
                deser_ms: result.deser_ms,
                marshal_ms: result.marshal_ms,
                gpu_upload_ms,
            });
        }

        // GPU upload complete.
        self.state.session_stats.pipeline.mark_render_done();

        // Remove this scan from in-flight ghost tracking. The queue is
        // keyed by archive-derived i64 seconds; truncate the result's
        // scan-start (millis) at the same boundary.
        let result_scan_start_i64 = result.context.scan_key.scan_start.as_secs();
        self.state
            .download_progress
            .in_flight_scans
            .retain(|&(start, _)| start != result_scan_start_i64);
        // If no more in-flight or pending, fully clear progress.
        if self.state.download_progress.in_flight_scans.is_empty()
            && self.state.download_progress.pending_scans.is_empty()
        {
            self.state.download_progress.clear();
        }

        // Refine canvas overlay with precise decoded data
        if result.sweep_start_secs > 0.0 {
            self.update_overlay_from_sweep(
                result.sweep_start_secs,
                result.sweep_end_secs,
                display_angle,
            );
        }
    }

    fn handle_live_decoded_outcome(&mut self, result: DecodeResult) {
        log::debug!(
            "Live decode: {}x{}, {} radials, {}, {:.0}ms",
            result.azimuth_count,
            result.gate_count,
            result.radial_count,
            result.product,
            result.total_ms,
        );

        // While the playhead is detached (browsing the archive with the
        // stream ingesting in the background), the canvas belongs to the
        // scrubbed position — skip the GPU upload, `displayed` mutation,
        // and overlay refresh. The azimuth bookkeeping below still runs so
        // sweep compositing is correct the instant the user re-pins.
        let playhead_attached = !self.live.is_detached(&self.playback.state);

        // VCP target angle for the cut (mirrors archived path).
        let display_angle = self
            .live
            .radar_model
            .volume
            .as_ref()
            .and_then(|v| v.target_elevation_angle(result.context.elevation_number))
            .unwrap_or(result.mean_elevation);

        if let (true, Some(ref renderer), Some(ref gl)) =
            (playhead_attached, &self.gpu.gpu, &self.gpu.gl)
        {
            if let Ok(mut r) = renderer.lock() {
                // Build a live sweep ID so we can detect elevation transitions
                let live_elev = result.context.elevation_number;
                let live_sweep_id = format!("live|{}", live_elev);

                // If the current texture has data from a different sweep
                // (complete or different live elevation), promote it to
                // previous so it becomes the background for compositing
                // partial data. The `should_promote` branch below
                // (post-update_data) snapshots `displayed` into
                // `previous_displayed`, which is the canonical source for
                // overlay/timeline prev info.
                let should_promote = r.current_sweep_id().is_some_and(|id| id != live_sweep_id);
                if should_promote {
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
                r.set_current_sweep_id(Some(live_sweep_id));
                r.update_color_table(gl, &result.product);

                // Capture the live on-GPU identity. `should_promote` flags
                // an elevation transition within the live volume — only at
                // those boundaries is the prior `displayed` semantically a
                // *different* sweep, so we only roll it into
                // `previous_displayed` then. Repeated partial-sweep uploads
                // for the *same* elevation overwrite `displayed` in place.
                let new_displayed = crate::core::DisplayedSweep {
                    identity: SweepIdentity::new(
                        result.context.scan_key.clone(),
                        result.context.elevation_number,
                        result.product.clone(),
                    ),
                    start_time: result.sweep_start_secs,
                    end_time: result.sweep_end_secs,
                    elevation_deg: display_angle,
                };
                if should_promote {
                    let prior = self.state.viz_state.displayed.replace(new_displayed);
                    self.state.viz_state.previous_displayed = prior;
                } else {
                    self.state.viz_state.displayed = Some(new_displayed);
                }

                // Re-run storm cell detection on the freshly-uploaded live
                // sweep so the overlay tracks the incoming chunks rather
                // than freezing until the user toggles the feature.
                if self.state.viz_state.storm_cells_visible {
                    self.state.viz_state.detected_storm_cells = r.detect_storm_cells(
                        self.state.viz_state.center_lat,
                        self.state.viz_state.center_lon,
                        self.state.viz_state.storm_cell_threshold_dbz,
                    );
                }
            }
        }

        // Update overlay staleness so the age counter reflects
        // the most recently received live data.
        if playhead_attached && result.sweep_end_secs > 0.0 {
            self.update_overlay_from_sweep(
                result.sweep_start_secs,
                result.sweep_end_secs,
                display_angle,
            );
        }

        // Store the chronological azimuth range for sweep compositing.
        // Must use chronological first/last (from radial timestamps), NOT
        // sorted min/max. Once a sweep wraps past 0°, the sorted range
        // spans ~360° and the shader thinks the entire circle has current
        // data, hiding the previous sweep.
        if !result.azimuths.is_empty() {
            // Chronological first = sweep start azimuth (set once per sweep).
            // Chronological last = most recent radial's azimuth from the live state.
            if self.live.mode_state.sweep_start_azimuth.is_none() {
                // First live decode for this sweep: use the earliest radial
                // by collection time as the sweep start.
                let first_az = if !result.radial_times.is_empty() {
                    let min_time_idx = result
                        .radial_times
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    result.azimuths[min_time_idx]
                } else {
                    result.azimuths[0]
                };
                self.live.mode_state.sweep_start_azimuth = Some(first_az);
            }

            // The trailing edge of received data: latest radial by collection time.
            let last_az = if !result.radial_times.is_empty() {
                let max_time_idx = result
                    .radial_times
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                    .map(|(i, _)| i)
                    .unwrap_or(result.azimuths.len() - 1);
                result.azimuths[max_time_idx]
            } else {
                *result.azimuths.last().unwrap()
            };

            let first_az = self.live.mode_state.sweep_start_azimuth.unwrap_or(0.0);
            log::debug!(
                "Live azimuth range: chrono_first={:.1} chrono_last={:.1} count={}",
                first_az,
                last_az,
                result.azimuths.len(),
            );
            self.live.mode_state.live_data_azimuth_range = Some((first_az, last_az));
        }
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
