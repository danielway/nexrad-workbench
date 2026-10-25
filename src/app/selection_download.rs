//! The shared download-queue pump.
//!
//! Archive acquisition is reactive: [`WorkbenchApp::pump_implicit_prefetch`]
//! enqueues the scans the current view needs (scoped to the active filter).
//! This module advances that queue — reaping finished downloads, filling free
//! concurrency slots up to `max_parallel`, and updating the timeline/progress
//! overlays as work completes. Every site queues into the one queue; each
//! download runs for its item's site, and the timeline's ghosts show the
//! focus site's items (the timeline shows the focus site's track).

use crate::nexrad::download_queue::QueueAction;
use crate::WorkbenchApp;
use eframe::egui;

impl WorkbenchApp {
    /// Advance the download queue: reap finished downloads and fill as many
    /// free concurrency slots as possible. No-op when the queue is empty.
    pub(crate) fn advance_download_queue(&mut self, ctx: &egui::Context) {
        let focus = self.sites.focus().id.clone();

        if !self.acquisition.coordinator.download_queue.has_work() {
            return;
        }

        // 0. Serve the playhead: drop pending *reactive* work the cursor has
        // scrubbed far away from (cancelling its drawer operations), then
        // order what remains nearest-first in the playback direction.
        // Selection/Explicit items are user-requested and never playhead-
        // pruned (their lifecycles are owned by their own flows); active
        // downloads are never cancelled — they finish and free their slots
        // naturally.
        let playhead = self.playback.state.playback_position();
        let forward =
            self.playback.state.time_model.direction == crate::core::PlaybackDirection::Forward;
        let speed = self.playback.state.speed.timeline_seconds_per_real_second();
        let playing = self.playback.state.playing;
        let (win_start, win_end) =
            crate::core::acquisition::prefetch_window(playhead, speed, playing, forward);
        // Keep a fixed two-scan margin around the (possibly degenerate)
        // window so small scrubs don't churn the queue.
        let margin = 2 * crate::FALLBACK_SCAN_DURATION_SECS;
        let (keep_start, keep_end) = (win_start - margin, win_end + margin);
        let pruned = self
            .acquisition
            .coordinator
            .download_queue
            .prune_pending(|item| {
                item.origin != crate::nexrad::download_queue::QueueOrigin::Reactive
                    || (item.scan_end >= keep_start && item.scan_start <= keep_end)
            });
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
        let finished: Vec<(crate::data::SiteId, i64)> = self
            .acquisition
            .coordinator
            .download_queue
            .active_items()
            .filter(|item| {
                !self
                    .acquisition
                    .coordinator
                    .download_channel
                    .is_download_pending(&item.site.0, item.scan_start)
            })
            .map(|item| (item.site.clone(), item.scan_start))
            .collect();
        for (site, start) in finished {
            self.acquisition
                .coordinator
                .download_queue
                .mark_active_done(&site, start);
        }

        // 2. Refresh the timeline-ghost list from the queue state.
        self.state.download_progress.active_scans = self.focus_ghosts(&focus, true);

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
                    site,
                    file_name,
                    scan_start,
                    scan_end,
                    elevation_filter,
                    operation_id,
                } => {
                    self.state.download_progress.phase = crate::state::DownloadPhase::Downloading;
                    self.state.download_progress.batch_completed += 1;
                    if site == focus {
                        self.state
                            .download_progress
                            .active_scans
                            .push((scan_start, scan_end));
                    }

                    // Mark the item's own acquisition operation active and pin
                    // it to this download's scan_start. The id rides on the
                    // queue item (not FIFO order) so priority dispatch and
                    // pruning can't mis-pair operations with downloads.
                    if let Some(op_id) = operation_id {
                        self.acquisition.state.mark_active(op_id);
                        self.acquisition
                            .coordinator
                            .download_queue
                            .set_operation_id(&site, scan_start, op_id);
                    }

                    self.acquisition.coordinator.download_channel.download_file(
                        ctx.clone(),
                        site.0,
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
            self.state.download_progress.active_scans = self.focus_ghosts(&focus, true);
            self.state.download_progress.pending_scans = self.focus_ghosts(&focus, false);
        }
    }

    /// The focus site's active (or pending) queue items as timeline ghosts.
    fn focus_ghosts(&self, focus: &crate::data::SiteId, active: bool) -> Vec<(i64, i64)> {
        let queue = &self.acquisition.coordinator.download_queue;
        let items: Vec<_> = if active {
            queue.active_items().collect()
        } else {
            queue.pending_items().collect()
        };
        items
            .into_iter()
            .filter(|item| &item.site == focus)
            .map(|item| (item.scan_start, item.scan_end))
            .collect()
    }
}
