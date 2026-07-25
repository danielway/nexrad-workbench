//! The shared download-queue pump.
//!
//! Archive acquisition is reactive: [`WorkbenchApp::pump_implicit_prefetch`]
//! enqueues the scans the current view needs (scoped to the active filter).
//! This module advances that queue — reaping finished downloads, filling free
//! concurrency slots up to `max_parallel`, and updating the timeline/progress
//! overlays as work completes.

use crate::nexrad::download_queue::QueueAction;
use crate::WorkbenchApp;
use eframe::egui;

impl WorkbenchApp {
    /// Advance the download queue: reap finished downloads and fill as many
    /// free concurrency slots as possible. No-op when the queue is empty.
    pub(crate) fn advance_download_queue(&mut self, ctx: &egui::Context) {
        let site_id = self.state.viz_state.site_id.clone();

        if !self.acquisition.coordinator.download_queue.has_work() {
            return;
        }

        // 0. Serve the playhead: drop pending work the cursor has scrubbed
        // far away from (cancelling its drawer operations), then order what
        // remains nearest-first in the playback direction. Active downloads
        // are never cancelled — they finish and free their slots naturally.
        let playhead = self.playback.state.playback_position();
        let forward =
            self.playback.state.time_model.direction == crate::core::PlaybackDirection::Forward;
        let speed = self.playback.state.speed.timeline_seconds_per_real_second();
        let playing = self.playback.state.playing;
        let (win_start, win_end) =
            crate::nexrad::download_queue::prefetch_window(playhead, speed, playing, forward);
        // Keep a generous 3x margin around the prefetch window so small
        // scrubs don't churn the queue.
        let span = win_end - win_start;
        let (keep_start, keep_end) = (win_start - span, win_end + span);
        let pruned = self
            .acquisition
            .coordinator
            .download_queue
            .prune_pending(|item| item.scan_end >= keep_start && item.scan_start <= keep_end);
        for item in pruned {
            if let Some(op_id) = item.operation_id {
                self.acquisition.state.cancel_operation(op_id);
            }
        }
        self.acquisition
            .coordinator
            .download_queue
            .reprioritize(playhead as i64, forward);

        // 1. Sweep all Active items and mark any whose download has finished.
        let finished_starts: Vec<i64> = self
            .acquisition
            .coordinator
            .download_queue
            .active_items()
            .filter_map(|item| {
                if !self
                    .acquisition
                    .coordinator
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
            self.acquisition
                .coordinator
                .download_queue
                .mark_active_done(start);
        }

        // 2. Refresh the timeline-ghost list from the queue state.
        self.state.download_progress.active_scans = self
            .acquisition
            .coordinator
            .download_queue
            .active_items()
            .map(|item| (item.scan_start, item.scan_end))
            .collect();

        // 3. Fill as many concurrency slots as possible.
        let is_paused = self.acquisition.state.is_paused();
        let mut completed_this_pump = false;
        loop {
            match self
                .acquisition
                .coordinator
                .download_queue
                .advance(is_paused)
            {
                QueueAction::StartDownload {
                    date,
                    file_name,
                    scan_start,
                    scan_end,
                    elevation_filter,
                    operation_id,
                    remaining,
                } => {
                    self.state.status_message =
                        format!("Downloading {} ({} remaining)", file_name, remaining);
                    self.state.download_progress.phase = crate::state::DownloadPhase::Downloading;
                    self.state.download_progress.batch_completed += 1;
                    self.state
                        .download_progress
                        .active_scans
                        .push((scan_start, scan_end));

                    // Mark the item's own acquisition operation active and pin
                    // it to this download's scan_start. The id rides on the
                    // queue item (not FIFO order) so priority dispatch and
                    // pruning can't mis-pair operations with downloads.
                    if let Some(op_id) = operation_id {
                        self.acquisition.state.mark_active(op_id);
                        self.acquisition
                            .coordinator
                            .download_queue
                            .set_operation_id(scan_start, op_id);
                    }

                    self.acquisition.coordinator.download_channel.download_file(
                        ctx.clone(),
                        site_id.clone(),
                        date,
                        file_name,
                        scan_start,
                        self.acquisition.coordinator.facade().clone(),
                        elevation_filter,
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
            self.state.download_progress.pending_scans.clear();
            self.state.download_progress.active_scans.clear();
            self.state.download_progress.phase = crate::state::DownloadPhase::Done;
            // Full clear only if no in-flight scans remain.
            if self.state.download_progress.in_flight_scans.is_empty() {
                self.state.download_progress.clear();
            }
            log::debug!("Download queue drained");
        } else {
            // 4. Republish the active + pending (queued) ghost lists from the
            // post-dispatch queue state. Active is rebuilt here (rather than
            // trusting the in-loop pushes alone) and pending is filled so the
            // strip's queued cells render — without this, `pending_scans` was
            // only ever cleared and queued cells were invisible (the queued
            // hatch had no data). Skipped on a drain, which clears both.
            self.state.download_progress.active_scans = self
                .acquisition
                .coordinator
                .download_queue
                .active_items()
                .map(|item| (item.scan_start, item.scan_end))
                .collect();
            self.state.download_progress.pending_scans = self
                .acquisition
                .coordinator
                .download_queue
                .pending_items()
                .map(|item| (item.scan_start, item.scan_end))
                .collect();
        }
    }
}
