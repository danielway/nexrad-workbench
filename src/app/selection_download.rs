//! Process_selection_download — the user-initiated archive download path.
//!
//! Drives the selection-download state machine: enqueues scans in the
//! current selection range, advances up to `max_parallel` concurrent
//! downloads, and updates the timeline/progress overlays as work completes.

use crate::nexrad::download_queue::{QueueAction, QueueItem};
use crate::{nexrad, state, WorkbenchApp, SCAN_CACHE_MATCH_TOLERANCE_SECS};
use eframe::egui;

impl WorkbenchApp {
    /// Process selection download: download scans in the selected time range serially.
    ///
    /// `download_type` is `None` when pumping the existing queue (no new command),
    /// `Some(true)` for a position-download, or `Some(false)` for a range-selection download.
    pub(crate) fn process_selection_download(
        &mut self,
        ctx: &egui::Context,
        download_type: Option<bool>,
    ) {
        let site_id = self.state.viz_state.site_id.clone();

        // If we have items in the queue, try to advance the state machine.
        // The queue allows up to `max_parallel` concurrent downloads; we both
        // reap completed slots and fill empty slots on every poll.
        if self.acquisition.download_queue.has_work() {
            // 1. Sweep all Active items and mark any whose download has finished.
            let finished_starts: Vec<i64> = self
                .acquisition
                .download_queue
                .active_items()
                .filter_map(|item| {
                    if !self
                        .acquisition
                        .download_channel
                        .is_download_pending(&site_id, item.scan_start)
                    {
                        Some(item.scan_start)
                    } else {
                        None
                    }
                })
                .collect();
            for start in finished_starts {
                self.acquisition.download_queue.mark_active_done(start);
            }

            // 2. Refresh the timeline-ghost list from the queue state.
            self.state.download_progress.active_scans = self
                .acquisition
                .download_queue
                .active_items()
                .map(|item| (item.scan_start, item.scan_end))
                .collect();

            // 3. Fill as many concurrency slots as possible.
            let is_paused = self.state.acquisition.is_paused();
            let mut completed_this_pump = false;
            loop {
                match self.acquisition.download_queue.advance(is_paused) {
                    QueueAction::StartDownload {
                        idx: _,
                        date,
                        file_name,
                        scan_start,
                        scan_end,
                        remaining,
                    } => {
                        self.state.status_message =
                            format!("Downloading {} ({} remaining)", file_name, remaining);
                        self.state.download_progress.phase =
                            crate::state::DownloadPhase::Downloading;
                        self.state.download_progress.batch_completed += 1;
                        self.state
                            .download_progress
                            .active_scans
                            .push((scan_start, scan_end));

                        // Mark next acquisition operation as active and pin it
                        // to this download's scan_start so correlation survives
                        // concurrent completions.
                        if let Some(op_id) = self.state.acquisition.next_queued_id() {
                            self.state.acquisition.mark_active(op_id);
                            self.acquisition
                                .download_queue
                                .set_operation_id(scan_start, op_id);
                        }

                        self.acquisition.download_channel.download_file(
                            ctx.clone(),
                            site_id.clone(),
                            date,
                            file_name,
                            scan_start,
                            self.acquisition.facade().clone(),
                        );
                    }
                    QueueAction::Complete => {
                        completed_this_pump = true;
                        break;
                    }
                    QueueAction::Saturated | QueueAction::Paused => {
                        break;
                    }
                }
            }

            if completed_this_pump {
                self.state.download_selection_in_progress = false;
                self.state.download_progress.pending_scans.clear();
                self.state.download_progress.active_scans.clear();
                self.state.download_progress.phase = crate::state::DownloadPhase::Done;
                // Full clear only if no in-flight scans remain.
                if self.state.download_progress.in_flight_scans.is_empty() {
                    self.state.download_progress.clear();
                }
                self.state.status_message = "Selection download complete".to_string();
                log::debug!("Selection download complete");
            }
            return;
        }

        // No queue — check if a new download command was issued or a pending
        // download is being resumed after a listing arrived.
        let is_position_download = match download_type {
            Some(is_pos) => {
                // Fresh user action (not a pending resume) — reset pending state
                if self
                    .acquisition
                    .pending_download
                    .as_ref()
                    .is_none_or(|p| p.is_position != is_pos)
                {
                    self.acquisition.pending_download = None;
                }
                is_pos
            }
            None => return, // Just pumping queue, nothing to do
        };

        // Get the download range: either from selection or from current position.
        // For position download, we use a temporary wide window to determine which
        // date listings to fetch, then narrow to the exact scan below.
        let (sel_start, sel_end) = if is_position_download {
            let pos = self.state.playback_state.playback_position();
            (pos, pos)
        } else {
            match self.state.playback_state.selection_range() {
                Some(range) => range,
                None => {
                    log::warn!("Download selection requested but no valid selection");
                    return;
                }
            }
        };

        let sel_start_i64 = sel_start as i64;
        let sel_end_i64 = sel_end as i64;

        // Determine the date range for listing lookups
        let start_date = match chrono::DateTime::from_timestamp(sel_start_i64, 0) {
            Some(dt) => dt.date_naive(),
            None => return,
        };
        let end_date = match chrono::DateTime::from_timestamp(sel_end_i64, 0) {
            Some(dt) => dt.date_naive(),
            None => return,
        };

        log::debug!(
            "Building download queue for selection: {} to {} ({} to {})",
            sel_start_i64,
            sel_end_i64,
            start_date,
            end_date
        );

        // Collect all files whose scan boundaries intersect the selection
        let mut files_to_download: Vec<QueueItem> = Vec::new();
        let mut current_date = start_date;

        while current_date <= end_date {
            if let Some(listing) = self.acquisition.archive_index.get(&site_id, &current_date) {
                if is_position_download {
                    // Single-position: find the exact scan containing the playback position
                    if let Some((file, boundary)) = listing.find_scan_containing(sel_start_i64) {
                        let is_cached = self.state.radar_timeline.scans.iter().any(|s| {
                            (s.start_time as i64 - file.timestamp).abs()
                                < SCAN_CACHE_MATCH_TOLERANCE_SECS
                        });
                        if !is_cached {
                            files_to_download.push(QueueItem::new(
                                current_date,
                                file.name.clone(),
                                boundary.start,
                                boundary.end,
                            ));
                        }
                    } else {
                        // No scan covers this time in the cached listing.
                        // Check if we already re-fetched this date's listing.
                        let already_refetched = self
                            .acquisition
                            .pending_download
                            .as_ref()
                            .is_some_and(|p| p.refetched_dates.contains(&current_date));

                        if !already_refetched {
                            // The listing may be stale (e.g. archives created
                            // after it was cached), so invalidate and re-fetch
                            // once. Store intent so we resume when it arrives.
                            log::debug!(
                                "No scan at {} in cached listing for {}/{}; re-fetching",
                                sel_start_i64,
                                site_id,
                                current_date
                            );
                            let pending =
                                self.acquisition.pending_download.get_or_insert_with(|| {
                                    nexrad::acquisition_coordinator::PendingDownload {
                                        is_position: true,
                                        refetched_dates: std::collections::HashSet::new(),
                                    }
                                });
                            pending.refetched_dates.insert(current_date);
                            self.acquisition
                                .archive_index
                                .remove(&site_id, &current_date);
                            if !self
                                .acquisition
                                .download_channel
                                .is_listing_pending(&site_id, &current_date)
                            {
                                self.acquisition.download_channel.fetch_listing(
                                    ctx.clone(),
                                    site_id.clone(),
                                    current_date,
                                );
                            }
                            self.state.status_message =
                                format!("Re-fetching archive listing for {}...", current_date);
                            return;
                        }

                        // Already re-fetched — no scan here, skip.
                        log::debug!(
                            "No scan at {} in listing for {}/{} after re-fetch; skipping",
                            sel_start_i64,
                            site_id,
                            current_date
                        );
                    }
                } else {
                    // Range selection: find all scans that intersect [sel_start, sel_end]
                    for (file, boundary) in listing.scans_intersecting(sel_start_i64, sel_end_i64) {
                        let is_cached = self.state.radar_timeline.scans.iter().any(|s| {
                            (s.start_time as i64 - file.timestamp).abs()
                                < SCAN_CACHE_MATCH_TOLERANCE_SECS
                        });
                        if !is_cached {
                            files_to_download.push(QueueItem::new(
                                current_date,
                                file.name.clone(),
                                boundary.start,
                                boundary.end,
                            ));
                        }
                    }
                }
            } else {
                // Need to fetch the listing first. Store intent so we resume
                // when the listing arrives (via handle_listing_outcome).
                if !self
                    .acquisition
                    .download_channel
                    .is_listing_pending(&site_id, &current_date)
                {
                    log::debug!("Fetching listing for {}/{}", site_id, current_date);
                    self.acquisition.download_channel.fetch_listing(
                        ctx.clone(),
                        site_id.clone(),
                        current_date,
                    );
                }
                self.acquisition.pending_download.get_or_insert_with(|| {
                    nexrad::acquisition_coordinator::PendingDownload {
                        is_position: is_position_download,
                        refetched_dates: std::collections::HashSet::new(),
                    }
                });
                self.state.status_message =
                    format!("Fetching archive listing for {}...", current_date);
                return;
            }

            current_date += chrono::Duration::days(1);
        }

        // Queue building complete — clear pending state
        self.acquisition.pending_download = None;

        if files_to_download.is_empty() {
            self.state.status_message = "No new scans to download in selection".to_string();
            log::debug!("No new scans to download in selection");
            return;
        }

        // Sort by start timestamp
        files_to_download.sort_by_key(|item| item.scan_start);

        log::debug!(
            "Queued {} files for download in selection",
            files_to_download.len()
        );

        // Start downloading
        self.state.download_selection_in_progress = true;

        // Cancel any existing acquisition operations (selection change = cancel all + rebuild)
        self.state.acquisition.cancel_all();
        self.acquisition.download_queue.set_queue(files_to_download);

        // Create acquisition operations for each file in the queue
        for item in self.acquisition.download_queue.items() {
            self.state
                .acquisition
                .create_operation(state::OperationKind::ArchiveDownload {
                    site_id: site_id.clone(),
                    file_name: item.file_name.clone(),
                    scan_start: item.scan_start,
                    scan_end: item.scan_end,
                });
        }

        // Populate download progress for timeline ghosts and pipeline display
        {
            let progress = &mut self.state.download_progress;
            progress.pending_scans = self
                .acquisition
                .download_queue
                .items()
                .iter()
                .map(|item| (item.scan_start, item.scan_end))
                .collect();
            progress.batch_total = self.acquisition.download_queue.len() as u32;
            progress.batch_completed = 0;
            progress.phase = crate::state::DownloadPhase::Downloading;
            progress.active_scans.clear();
        }

        // Kick off as many downloads as the concurrency limit allows.
        let is_paused = self.state.acquisition.is_paused();
        while let QueueAction::StartDownload {
            idx: _,
            date,
            file_name,
            scan_start,
            scan_end,
            remaining,
        } = self.acquisition.download_queue.advance(is_paused)
        {
            self.state.status_message =
                format!("Downloading {} ({} remaining)", file_name, remaining);
            self.state
                .download_progress
                .active_scans
                .push((scan_start, scan_end));

            if let Some(op_id) = self.state.acquisition.next_queued_id() {
                self.state.acquisition.mark_active(op_id);
                self.acquisition
                    .download_queue
                    .set_operation_id(scan_start, op_id);
            }

            self.acquisition.download_channel.download_file(
                ctx.clone(),
                site_id.clone(),
                date,
                file_name,
                scan_start,
                self.acquisition.facade().clone(),
            );
        }
    }
}
