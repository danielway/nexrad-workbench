//! Worker-result and cache-load outcome handlers.
//!
//! Drains responses from the decode worker pool (`render.try_recv()`),
//! the cache loader, and download channels. Each worker outcome variant
//! has its own `handle_*_outcome` method; this module hosts all of them
//! plus the small helpers (`set_active_scan`, `advance_active_scan_chunk`,
//! `clear_active_scan`) that bridge worker output and `RenderCoordinator`.

use crate::state::playback_manager::{sweep_cache_key, CachedSweepData};
use crate::state::SweepIdentity;
use crate::{data, nexrad, state, WorkbenchApp, MAX_SCAN_AGE_SECS};
use eframe::egui;

impl WorkbenchApp {
    pub(crate) fn handle_worker_results(&mut self, _ctx: &egui::Context) {
        if let Some(result) = self.acquisition.coordinator.cache_load_channel.try_recv() {
            self.handle_cache_load_outcome(result);
        }

        for outcome in self.render.try_recv() {
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

    fn handle_cache_load_outcome(&mut self, result: nexrad::CacheLoadResult) {
        match result {
            nexrad::CacheLoadResult::Success {
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
                self.state.radar_timeline = state::RadarTimeline::from_metadata(metadata);

                // Get time ranges (may be non-contiguous)
                let ranges = self.state.radar_timeline.time_ranges();
                if !ranges.is_empty() {
                    // Set overall bounds from first to last
                    let start = ranges.first().unwrap().start;
                    let end = ranges.last().unwrap().end;
                    self.state.playback_state.data_start_timestamp = Some(start as i64);
                    self.state.playback_state.data_end_timestamp = Some(end as i64);

                    // Position playback at the end of the most recent range
                    let most_recent_end = ranges.last().unwrap().end;

                    // Only auto-position on initial load or site change,
                    // not when refreshing after a download.
                    if self.state.auto_position_on_timeline_load {
                        self.state.auto_position_on_timeline_load = false;
                        let ts = self.state.playback_state.playback_position();
                        let in_any_range = ranges.iter().any(|r| r.contains(ts));
                        if !in_any_range {
                            self.state
                                .playback_state
                                .set_playback_position(most_recent_end);
                            self.state.playback_state.center_view_on(most_recent_end);
                        }
                    }

                    log::debug!("Timeline has {} contiguous range(s)", ranges.len());
                }
            }
            nexrad::CacheLoadResult::Error(msg) => {
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
        self.render.set_scan(scan_key, elevations);
    }

    /// Like [`Self::set_active_scan`] but for the live chunk-by-chunk
    /// path where the elevation list grows incrementally.
    pub(crate) fn advance_active_scan_chunk(
        &mut self,
        scan_key: data::ScanKey,
        new_elevations: &[u8],
        _displayed_ts: f64,
    ) {
        self.render.set_scan_key(scan_key);
        self.render.add_elevations(new_elevations);
    }

    /// Clear the active scan. Used when no scan is in range, on site
    /// change, and on error paths.
    pub(crate) fn clear_active_scan(&mut self) {
        self.render.clear_scan_key();
    }

    fn handle_ingested_outcome(&mut self, result: nexrad::IngestResult) {
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

        // Worker now returns the typed scan_key on the result (sourced from
        // the dispatch-time context); no parse step.
        self.set_active_scan(
            result.scan_key.clone(),
            result.elevation_numbers,
            result.context.timestamp_secs,
        );
        // Refresh timeline to include the new scan
        self.state.push_command(state::AppCommand::RefreshTimeline {
            auto_position: false,
        });

        // Request eviction check
        self.state.push_command(state::AppCommand::CheckEviction);

        // Force a fresh render
        self.render.force_fresh_render();

        // Trigger render for the ingested scan
        self.request_worker_render();
        if self.state.viz_state.volume_3d_enabled {
            self.request_worker_render_volume();
        }
    }

    fn handle_chunk_ingested_outcome(&mut self, result: nexrad::ChunkIngestResult) {
        let is_live = self.state.live_mode_state.is_active();
        let source = "Realtime";

        // Build enriched log with projection-derived chunk positioning.
        let chunk_vol_index = result.context.chunk_index + 1; // 1-based for display
        let elev_nums: Vec<u8> = result
            .chunk_elev_spans
            .iter()
            .map(|&(e, _, _, _)| e)
            .collect();
        let total_azimuths: u32 = result
            .chunk_elev_spans
            .iter()
            .map(|&(_, _, _, count)| count)
            .sum();

        // Azimuth angle range from the chunk's azimuth data
        let az_range_str =
            if let Some(&(_, first_az, last_az)) = result.chunk_elev_az_ranges.first() {
                format!("{:.1}°–{:.1}°", first_az, last_az)
            } else {
                "n/a".to_string()
            };

        // Look up chunk-in-sweep and remaining from projection metadata.
        // chunk_index is 0-based where 0 = Start chunk (sequence 1), so
        // chunk_vol_index (= chunk_index + 1) already equals the 1-based sequence.
        let sequence = chunk_vol_index as usize;
        let (chunk_in_sweep_str, remaining_str) = self
            .state
            .live_mode_state
            .chunk_position_in_sweep(sequence)
            .map(|(idx, total)| {
                let in_sweep = format!("{}/{}", idx + 1, total);
                let remaining_in_sweep = total.saturating_sub(idx + 1);
                (in_sweep, format!("{}", remaining_in_sweep))
            })
            .unwrap_or_else(|| ("?/?".to_string(), "?".to_string()));

        log::debug!(
            "{}: chunk ingested scan={} vol_chunk={} sweep_chunk={} remaining_in_sweep={} \
             elevs={:?} azimuths={} az_range={} \
             elevs_completed={:?} sweeps_stored={} is_end={} vcp={:?} {:.1}ms",
            source,
            result.scan_key,
            chunk_vol_index,
            chunk_in_sweep_str,
            remaining_str,
            elev_nums,
            total_azimuths,
            az_range_str,
            result.elevations_completed,
            result.sweeps_stored,
            result.is_end,
            result.vcp.as_ref().map(|v| v.number),
            result.total_ms,
        );

        // Update scan key, growing elevation list, and displayed timestamp
        // through the single owner so they can never drift.
        let had_elevations = !self.render.available_elevations().is_empty();
        self.advance_active_scan_chunk(
            result.scan_key.clone(),
            &result.elevations_completed,
            result.context.timestamp_secs,
        );

        // Only update live_mode_state when actually in live mode
        if is_live {
            // Adopt the live volume anchor — provisional from the streaming
            // loop's IDB key, confirmed (when present) from the radial-parsed
            // header time. `set_or_confirm_volume` handles same-volume vs.
            // new-volume internally and runs `try_capture_forecast` on the
            // transition that first makes start time + VCP pattern both
            // known.
            let scan_key = data::ScanKey::from_secs_f64(
                &self.state.viz_state.site_id,
                result.context.timestamp_secs,
            );
            self.state.live_mode_state.set_or_confirm_volume(
                scan_key,
                result.context.timestamp_secs,
                result.volume_header_time_secs,
            );

            if !result.chunk_elev_spans.is_empty() {
                self.state
                    .live_mode_state
                    .record_chunk_elev_spans(&result.chunk_elev_spans);
            }

            // Push the most recent chunk's collection-end time down to the
            // streaming loop so the next projection anchors on the current
            // chunk's actual collection time (not the volume's start time).
            // Without this, forward-chunk projections come out as
            // volume_start + small_offset, landing in the past once the
            // volume is past its first chunk.
            if let Some(chunk_max_secs) = result.chunk_max_time_secs {
                self.streaming
                    .record_chunk_collection_end_secs(chunk_max_secs);
            }

            // Record the empirical availability lag (S3 upload − ACTUAL
            // chunk collection time) into the projector's stats bucket.
            // Uses the chunk's latest-radial time (when the radar finished
            // this chunk) paired with the most recent arrival stat's
            // Last-Modified header.
            if let Some(chunk_max_secs) = result.chunk_max_time_secs {
                // Lag requires both a parsed collection time AND the chunk's
                // S3 Last-Modified header. Stamp collection time unconditionally
                // and lag only when both are finite.
                let s3_at = self
                    .state
                    .live_mode_state
                    .chunk_arrivals
                    .last()
                    .and_then(|a| a.s3_last_modified_at);
                let lag_secs = s3_at
                    .map(|s3| s3 - chunk_max_secs)
                    .filter(|v| v.is_finite());
                if let Some(lag) = lag_secs {
                    self.streaming.record_availability_lag_secs(lag);
                }
                // Back-fill onto the most recent arrival so the diagnostics
                // modal can compute per-chunk collection-space intervals
                // and (when available) per-chunk availability lag.
                self.state
                    .live_mode_state
                    .attach_collection_data_to_last_arrival(
                        chunk_max_secs,
                        lag_secs.map(|lag| (lag * 1000.0) as i64),
                    );
            }
            if !result.elevations_completed.is_empty() {
                self.state
                    .live_mode_state
                    .record_elevations(&result.elevations_completed);
            }
            if let Some(ref vcp) = result.vcp {
                // Snap the user's selected elevation angle to the closest
                // entry in the new VCP when the pattern changes. Both
                // panels read the elevation list lazily via
                // AppState::current_elevation_list(), so no cache to
                // refresh — this resolve is the only thing that needs
                // to fire on a VCP transition.
                let prev_count = self
                    .state
                    .live_mode_state
                    .current_vcp_pattern
                    .as_ref()
                    .map(|p| p.elevations.len())
                    .unwrap_or(0);
                self.state.live_mode_state.record_vcp(vcp);
                if prev_count != vcp.elevations.len() {
                    let entries = state::playback_manager::build_elevation_list_from_vcp(vcp);
                    self.state
                        .viz_state
                        .elevation_selection
                        .resolve_for_vcp(&entries);
                }
            }

            self.state.live_mode_state.record_in_progress_elevation(
                result.current_elevation,
                result.current_elevation_radials,
            );

            // Record per-chunk azimuth ranges for the current elevation
            if let Some(cur_elev) = result.current_elevation {
                for &(elev, first_az, last_az) in &result.chunk_elev_az_ranges {
                    if elev == cur_elev {
                        let radial_count = result
                            .chunk_elev_spans
                            .iter()
                            .find(|&&(e, _, _, _)| e == elev)
                            .map(|&(_, _, _, c)| c)
                            .unwrap_or(0);
                        self.state.live_mode_state.current_elev_chunks.push((
                            first_az,
                            last_az,
                            radial_count,
                        ));
                    }
                }
            }

            if !result.sweeps.is_empty() {
                self.state
                    .live_mode_state
                    .update_sweep_metas(result.sweeps.clone());
            }

            self.state
                .live_mode_state
                .record_last_radial(result.last_radial_azimuth, result.last_radial_time_secs);

            // ── Log: sweep storage ────────────────────────────────────
            if !result.elevations_completed.is_empty() {
                for &completed_elev in &result.elevations_completed {
                    if let Some(meta) = result
                        .sweeps
                        .iter()
                        .find(|s| s.elevation_number == completed_elev)
                    {
                        log::debug!(
                            "{}: sweep stored elev={} angle={:.1}° start_az={:.1}° \
                             time={:.1}–{:.1}s dur={:.2}s products={} vol_chunk={}",
                            source,
                            completed_elev,
                            meta.elevation,
                            meta.start_azimuth,
                            meta.start,
                            meta.end,
                            meta.end - meta.start,
                            result.sweeps_stored,
                            chunk_vol_index,
                        );
                    } else {
                        log::debug!(
                            "{}: sweep stored elev={} (no CachedSweep) products={} vol_chunk={}",
                            source,
                            completed_elev,
                            result.sweeps_stored,
                            chunk_vol_index,
                        );
                    }
                }
            }

            // ── Log + dispatch: live partial-sweep render ─────────────
            // Always render whatever elevation is currently being
            // accumulated — the user expects to see live progress
            // regardless of which elevation was previously displayed.
            if !result.is_end {
                if let Some(target_elev) = result.current_elevation {
                    let product = self.state.viz_state.product.to_worker_string().to_string();

                    // Summarize what the accumulator holds for this elevation
                    let accum_radials = result.current_elevation_radials.unwrap_or(0);
                    let accum_chunks: usize = self
                        .state
                        .live_mode_state
                        .chunk_elev_spans
                        .iter()
                        .filter(|&&(e, _, _, _)| e == target_elev)
                        .count();
                    let accum_az_range = self
                        .state
                        .live_mode_state
                        .current_elev_chunks
                        .iter()
                        .fold((f32::MAX, f32::MIN), |(lo, hi), &(first_az, last_az, _)| {
                            (lo.min(first_az), hi.max(last_az))
                        });
                    let az_str = if accum_az_range.0 < f32::MAX {
                        format!("{:.1}°–{:.1}°", accum_az_range.0, accum_az_range.1)
                    } else {
                        "n/a".to_string()
                    };

                    log::debug!(
                        "{}: render_live dispatched elev={} product={} accum_radials={} \
                         accum_chunks={} accum_az={} vol_chunk={}",
                        source,
                        target_elev,
                        product,
                        accum_radials,
                        accum_chunks,
                        az_str,
                        chunk_vol_index,
                    );

                    self.render.render_live(target_elev, product);
                }
            }
        }

        // Refresh timeline when new elevations are written to cache
        if !result.elevations_completed.is_empty() {
            log::debug!(
                "{}: {} new elevation(s) cached, refreshing timeline (total available: {:?})",
                source,
                result.elevations_completed.len(),
                self.render.available_elevations(),
            );
            self.state.push_command(state::AppCommand::RefreshTimeline {
                auto_position: !is_live,
            });

            if is_live {
                self.state.status_message = format!(
                    "Live: {} elevation(s) cached",
                    self.render.available_elevations().len()
                );
            }
        }

        if result.is_end {
            if is_live {
                if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
                    if let Ok(mut r) = renderer.lock() {
                        r.promote_current_to_previous(gl);
                    }
                }
                let now = js_sys::Date::now() / 1000.0;
                self.state.live_mode_state.handle_volume_complete(now);
                self.state.status_message = format!(
                    "Live: volume complete ({} elevations)",
                    self.render.available_elevations().len()
                );
            } else {
                let now = js_sys::Date::now() / 1000.0;
                self.state.playback_state.set_playback_position(now);
            }

            log::debug!(
                "{}: volume complete — {} elevations, triggering render",
                source,
                self.render.available_elevations().len()
            );
            self.state.push_command(state::AppCommand::RefreshTimeline {
                auto_position: !is_live,
            });
            self.state.push_command(state::AppCommand::CheckEviction);
            self.state.session_stats.pipeline.mark_processing_done();

            self.render.force_fresh_render();
            if !is_live {
                self.request_worker_render();
                if self.state.viz_state.volume_3d_enabled {
                    self.request_worker_render_volume();
                }
            }
        } else if !had_elevations && !self.render.available_elevations().is_empty() {
            log::debug!(
                "{}: first elevation available, triggering initial render",
                source
            );
            self.render.force_fresh_render();
            if !is_live {
                self.request_worker_render();
                if self.state.viz_state.volume_3d_enabled {
                    self.request_worker_render_volume();
                }
            }
        }
    }

    fn handle_decoded_outcome(&mut self, result: nexrad::DecodeResult) {
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
        self.playback_manager.cache_sweep(
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
                sweep_start_secs: result.sweep_start_secs,
                sweep_end_secs: result.sweep_end_secs,
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
        let result_identity = state::SweepIdentity::new(
            result.context.scan_key.clone(),
            result.context.elevation_number,
            result.product.clone(),
        );
        let current_target = state::playback_manager::resolve_active_sweep_target(
            &self.state.viz_state.site_id,
            self.state.playback_state.playback_position(),
            &self.state.viz_state.elevation_selection,
            self.state.viz_state.product,
            &self.state.radar_timeline,
            MAX_SCAN_AGE_SECS,
        );
        let is_current_scan = current_target.as_ref() == Some(&result_identity);
        if self.state.effective_sweep_animation() && !is_current_scan {
            log::debug!("[sweep-anim] cached bg decode: {}", result_sweep_id);
            // Clear pending tracker so sync_prev_sweep_texture can load from cache
            if self.playback_manager.pending_prev_sweep_key() == Some(&result_sweep_id) {
                self.playback_manager.set_pending_prev_sweep_key(None);
            }
        }
        let t_gpu = web_time::Instant::now();
        // In live mode, LiveDecoded drives the GPU — skip Decoded
        // uploads so completed-elevation IDB renders don't overwrite
        // the current partial sweep.
        let skip_gpu_upload = self.state.live_mode_state.is_active();
        let mut gpu_upload_succeeded = false;
        if is_current_scan && !skip_gpu_upload {
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
            .state
            .radar_timeline
            .find_scan_at_timestamp(result.context.scan_key.scan_start.as_secs_f64())
            .and_then(|scan| scan.target_elevation_angle(result.context.elevation_number))
            .unwrap_or(result.mean_elevation);

        if gpu_upload_succeeded {
            self.state.viz_state.displayed = Some(state::DisplayedSweep {
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
            && !self.state.download_selection_in_progress
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

    fn handle_live_decoded_outcome(&mut self, result: nexrad::DecodeResult) {
        log::debug!(
            "Live decode: {}x{}, {} radials, {}, {:.0}ms",
            result.azimuth_count,
            result.gate_count,
            result.radial_count,
            result.product,
            result.total_ms,
        );

        // VCP target angle for the cut (mirrors archived path).
        let display_angle = self
            .state
            .live_radar_model
            .volume
            .as_ref()
            .and_then(|v| v.target_elevation_angle(result.context.elevation_number))
            .unwrap_or(result.mean_elevation);

        if let (Some(ref renderer), Some(ref gl)) = (&self.gpu.gpu, &self.gpu.gl) {
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
                let new_displayed = state::DisplayedSweep {
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
        if result.sweep_end_secs > 0.0 {
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
            if self.state.live_mode_state.sweep_start_azimuth.is_none() {
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
                self.state.live_mode_state.sweep_start_azimuth = Some(first_az);
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

            let first_az = self
                .state
                .live_mode_state
                .sweep_start_azimuth
                .unwrap_or(0.0);
            log::debug!(
                "Live azimuth range: chrono_first={:.1} chrono_last={:.1} count={}",
                first_az,
                last_az,
                result.azimuths.len(),
            );
            self.state.live_mode_state.live_data_azimuth_range = Some((first_az, last_az));
        }
    }

    fn handle_volume_decoded_outcome(&mut self, volume_data: nexrad::VolumeData) {
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
        kind: nexrad::WorkerErrorKind,
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

        // When the worker reports that the requested (elevation, product) has
        // no pre-computed sweep, clear the stale canvas so the user sees what
        // the timeline already knows — nothing matches their current filter.
        // Dispatch on the typed kind so transient errors (worker disconnect,
        // IDB failure) keep the last-good view instead of blanking.
        if kind == nexrad::WorkerErrorKind::NotFound {
            self.clear_display_no_sweep();
        }

        // Clean up the "processing" timeline ghost for the failed scan.
        // Prefer the scan attributed to the failing worker request so the
        // right ghost is removed even after the user scrolled away and
        // the active scan key now points elsewhere.
        let cleanup_ts = failed_scan_timestamp_secs
            .or_else(|| self.render.scan_key().map(|k| k.scan_start.as_secs_f64()));
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
            && !self.state.download_selection_in_progress
        {
            self.state.download_progress.clear();
        }
    }
}
