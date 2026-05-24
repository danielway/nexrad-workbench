//! Download-pipeline outcome handlers and queue pump.
//!
//! Handles results from the archive download channel (`handle_download_outcome`
//! / `handle_listing_outcome`), drives the prioritized download queue
//! (`pump_download_queue`), and processes user-initiated selection downloads
//! (`process_selection_download`). Also drains queued realtime streaming
//! results (`handle_streaming_results`).

use crate::{
    app::command_dispatch::CommandOutcome, nexrad, state, WorkbenchApp, FALLBACK_SCAN_DURATION_SECS,
};
use eframe::egui;

impl WorkbenchApp {
    pub(crate) fn handle_download_outcome(&mut self, result: nexrad::DownloadResult) {
        // Extract scan and timing info from result
        let (scan_opt, is_cache_hit) = match &result {
            nexrad::DownloadResult::Success {
                scan,
                fetch_latency_ms,
                decode_latency_ms,
            } => {
                if self.state.dev_mode {
                    self.state
                        .session_stats
                        .record_fetch_latency(*fetch_latency_ms);
                    self.state
                        .session_stats
                        .record_processing_time(*decode_latency_ms);
                }
                (Some(scan), false)
            }
            nexrad::DownloadResult::CacheHit(scan) => (Some(scan), true),
            _ => (None, false),
        };

        if let Some(scan) = scan_opt {
            let fetch_latency = match &result {
                nexrad::DownloadResult::Success {
                    fetch_latency_ms, ..
                } => *fetch_latency_ms,
                _ => 0.0,
            };

            // Move this scan's boundary to in-flight tracking (ghost stays
            // visible until processing completes in the Decoded handler).
            let scan_ts = scan.key.scan_start.as_secs();
            let scan_end = self
                .acquisition
                .coordinator
                .download_queue
                .find_by_scan_start(scan_ts)
                .map(|item| item.scan_end)
                .unwrap_or(scan_ts + FALLBACK_SCAN_DURATION_SECS);
            self.state
                .download_progress
                .in_flight_scans
                .push((scan_ts, scan_end));

            if is_cache_hit {
                self.state.status_message = format!("Loaded from cache: {}", scan.file_name);

                // Cache hit: skip ingest, go straight to decode.
                // Ghost stays until timeline refresh shows the real scan.
                self.state.download_progress.phase = crate::state::DownloadPhase::Decoding;

                // Cache hit: records already in IDB. Resolve elevation list
                // from timeline metadata (when available) and route both the
                // scan key and elevation list through the single-owner
                // helper.
                let elev_nums: Vec<u8> = self
                    .state
                    .radar_timeline
                    .find_recent_scan(scan_ts as f64, 1.0)
                    .map(|tl_scan| {
                        let mut nums: Vec<u8> =
                            tl_scan.sweeps.iter().map(|s| s.elevation_number).collect();
                        nums.sort_unstable();
                        nums.dedup();
                        nums
                    })
                    .unwrap_or_default();
                self.set_active_scan(scan.key.clone(), elev_nums, scan_ts as f64);

                self.render.coordinator.force_fresh_render();
                self.request_worker_render();
                if self.state.viz_state.volume_3d_enabled {
                    self.request_worker_render_volume();
                }
            } else {
                self.state.status_message =
                    format!("Downloaded: {} ({} bytes)", scan.file_name, scan.data.len());

                // Transition to ingesting phase
                self.state.download_progress.phase = crate::state::DownloadPhase::Ingesting;

                // Fresh download: send raw bytes to worker for ingest.
                // Worker splits records, probes elevations, stores in IDB,
                // then returns metadata. We render on the Ingested callback.
                self.state.session_stats.pipeline.processing = true;
                self.render.coordinator.ingest(
                    scan.data.clone(),
                    scan.key.site.0.clone(),
                    scan.key.scan_start.as_secs_f64(),
                    scan.file_name.clone(),
                    fetch_latency,
                );
            }

            // Refresh timeline to show the new/loaded scan
            self.state.push_command(state::AppCommand::RefreshTimeline {
                auto_position: false,
            });
        }

        // Mark acquisition operation completed on success
        if let Some(scan) = scan_opt {
            let scan_ts = scan.key.scan_start.as_secs();
            if let Some(op_id) = self
                .acquisition
                .coordinator
                .download_queue
                .take_operation_id(scan_ts)
            {
                self.acquisition
                    .state
                    .mark_completed(op_id, scan.data.len() as u64);
            }
        }

        if let nexrad::DownloadResult::Error {
            message,
            scan_start,
        } = &result
        {
            self.state.status_message = format!("Download failed: {}", message);
            log::error!("Download failed: {}", message);

            // Mark this scan's acquisition operation as failed
            if let Some(op_id) = self
                .acquisition
                .coordinator
                .download_queue
                .take_operation_id(*scan_start)
            {
                self.acquisition.state.mark_failed(op_id, message.clone());
            }

            // Transition the failed queue item out of Active so the concurrency
            // slot frees up for the next pump.
            self.acquisition
                .coordinator
                .download_queue
                .mark_active_done(*scan_start);
            self.state
                .download_progress
                .active_scans
                .retain(|&(s, _)| s != *scan_start);

            // Clear download progress on error if no more work remains
            if !self.acquisition.coordinator.download_queue.has_work() {
                self.acquisition.coordinator.download_queue.clear();
                self.state.download_progress.clear();
            }
        }
    }

    pub(crate) fn handle_listing_outcome(&mut self, result: nexrad::ListingResult) {
        match result {
            nexrad::ListingResult::Success {
                site_id,
                date,
                listing,
            } => {
                log::debug!(
                    "Archive listing received: {} files for {}/{}",
                    listing.files.len(),
                    site_id,
                    date
                );
                self.acquisition
                    .coordinator
                    .archive_index
                    .insert(&site_id, date, listing);

                // Rebuild shadow scan boundaries for the timeline
                if site_id == self.state.viz_state.site_id {
                    self.state.shadow_scan_boundaries = self
                        .acquisition
                        .coordinator
                        .archive_index
                        .all_boundaries_for_site(&site_id);
                }

                // Resume pending download now that the listing is available
                if let Some(pending) = &self.acquisition.coordinator.pending_download {
                    if pending.is_position {
                        self.state
                            .push_command(state::AppCommand::DownloadAtPosition);
                    } else {
                        self.state
                            .push_command(state::AppCommand::DownloadSelection);
                    }
                }
            }
            nexrad::ListingResult::Error(msg) => {
                log::error!("Listing request failed: {}", msg);
                // Abandon pending download on listing failure
                if self.acquisition.coordinator.pending_download.is_some() {
                    self.acquisition.coordinator.pending_download = None;
                    self.state.status_message =
                        format!("Download cancelled: listing fetch failed ({})", msg);
                }
            }
        }
    }

    /// Kick off or continue selection/position downloads.
    ///
    /// Reads the deferred-fan-out flags from the per-frame
    /// [`CommandOutcome`] produced by [`Self::dispatch_commands`] and
    /// drives whichever of the three paths apply (single-position
    /// download, selection-range download, or just pumping the
    /// in-progress queue).
    pub(crate) fn pump_download_queue(&mut self, ctx: &egui::Context, outcome: &CommandOutcome) {
        let download_type = if outcome.download_at_position {
            Some(true)
        } else if outcome.download_selection {
            Some(false)
        } else {
            None // Just pumping existing queue, or nothing to do
        };
        let queue_has_work = self.acquisition.coordinator.download_queue.has_work();
        if outcome.download_selection
            || outcome.download_at_position
            || outcome.pump_queue
            || queue_has_work
        {
            self.process_selection_download(ctx, download_type);
        }
    }

    /// Drain the realtime channel and manage live-mode lifecycle.
    pub(crate) fn handle_streaming_results(&mut self, ctx: &egui::Context) {
        // Push the user's elevation selection down to the realtime channel
        // each frame. The manager diffs internally so this is a no-op when
        // the selection hasn't changed; on a real change it bumps the
        // channel's filter epoch so any in-flight sleep wakes within ~250ms.
        self.streaming
            .sync_filter(&self.state.viz_state.elevation_selection);

        for result in self.streaming.poll() {
            self.handle_realtime_result(result, ctx);
        }

        // Stop realtime channel if live mode was stopped by UI
        if !self.state.live_mode_state.is_active() && self.streaming.is_active() {
            log::debug!("Stopping realtime channel (live mode ended)");
            self.streaming.stop();
        }

        // The plan's per-chunk `projected_available_at_secs` is an absolute
        // timestamp, so the countdown advances frame-by-frame off `now`
        // without needing a separate heartbeat to mirror the loop's sleep.
        // Phase flips happen at ChunkReceived time in
        // `LiveModeState::handle_realtime_chunk` based on plan.next_target.
    }
}
