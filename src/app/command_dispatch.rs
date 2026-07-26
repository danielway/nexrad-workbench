//! Drain the [`Intent`] queue and execute each command.
//!
//! Commands flow from UI panels via [`AppState::push_command`] and are
//! drained once per frame. Each command falls into one of three shapes:
//!
//! 1. **Immediate state mutation** — flips a flag or mutates `AppState`
//!    directly (e.g. `PauseQueue`, `OpenAlert`).
//! 2. **Async side-effect** — spawns a future via `spawn_local`
//!    (e.g. `WipeAll`, `CheckEviction`).
//! 3. **Deferred fan-out** — recorded on [`CommandOutcome`] so the
//!    `update()` loop can run the work after worker results land
//!    (`pump_queue`).
//!
//! The deferred `pump_queue` flag waits for worker results because
//! newly-decoded sweeps may add scans to the cache; running the queue pump
//! against that fresh state avoids issuing duplicate downloads. (Archive
//! acquisition itself is reactive — see `pump_implicit_prefetch`.)

use crate::{state, WorkbenchApp};
use eframe::egui;

/// Deferred work signaled by [`WorkbenchApp::dispatch_commands`].
///
/// `update()` fans these out **after** worker results have been drained
/// so the download/queue path sees the latest cache state.
#[derive(Default)]
pub(crate) struct CommandOutcome {
    /// Acquisition queue state changed (resume / retry / skip); pump it
    /// so newly-unblocked operations can advance this frame.
    pub pump_queue: bool,
}

impl WorkbenchApp {
    /// Drain the command queue and execute each command.
    ///
    /// Returns a [`CommandOutcome`] describing deferred work for
    /// `update()` to fan out after worker results have settled.
    pub(crate) fn dispatch_commands(&mut self, ctx: &egui::Context) -> CommandOutcome {
        let mut outcome = CommandOutcome::default();
        for cmd in self.state.drain_commands() {
            self.handle_command(ctx, cmd, &mut outcome);
        }
        outcome
    }

    fn handle_command(
        &mut self,
        ctx: &egui::Context,
        cmd: crate::core::Intent,
        outcome: &mut CommandOutcome,
    ) {
        use crate::core::Intent;
        match cmd {
            // ---- Site selection ---------------------------------------
            Intent::OpenExternalUrl(url) => {
                self.apply_effects(ctx, vec![crate::core::Effect::OpenUrl(url)])
            }
            Intent::LocateMeForSite => {
                self.apply_effects(ctx, vec![crate::core::Effect::LocateForSite])
            }
            Intent::SubmitZip(raw) => {
                self.apply_effects(ctx, vec![crate::core::Effect::GeocodeZip(raw)])
            }

            // ---- Storage lifecycle ------------------------------------
            Intent::ClearCache => self.handle_clear_cache(ctx),
            Intent::WipeAll => self.handle_wipe_all(),
            Intent::CheckEviction => self.handle_check_eviction(ctx),

            // ---- Timeline ---------------------------------------------
            Intent::RefreshTimeline { auto_position } => {
                self.handle_refresh_timeline(ctx, auto_position);
            }

            // ---- Live mode --------------------------------------------
            Intent::StartLive => self.start_live_mode(ctx),
            Intent::ReturnToLive => self.return_to_live(ctx),

            // ---- Loop presets -----------------------------------------
            Intent::ApplyLoopPreset(preset) => self.apply_loop_preset(preset, ctx),
            Intent::ClearLoop => self.playback.state.clear_selection(),

            // ---- Queue management -------------------------------------
            // Mutations that may unblock work flip `pump_queue` so the
            // post-results queue pump runs in the same frame.
            Intent::PauseQueue => self.acquisition.state.pause(),
            Intent::ResumeQueue => {
                self.acquisition.state.resume();
                outcome.pump_queue = true;
            }
            Intent::RetryFailed(op_id) => {
                self.handle_retry_failed(op_id);
                outcome.pump_queue = true;
            }
            Intent::FetchScan {
                scan_start,
                elevation_filter,
            } => {
                self.handle_fetch_scan(scan_start, elevation_filter);
                outcome.pump_queue = true;
            }
            Intent::SkipFailed(op_id) => {
                self.acquisition.state.skip_failed(op_id);
                outcome.pump_queue = true;
            }
            Intent::CancelOperation(op_id) => self.acquisition.state.cancel_operation(op_id),
            Intent::ReorderOperation(op_id, delta) => {
                self.acquisition.state.reorder_operation(op_id, delta);
            }

            // ---- Worker lifecycle -------------------------------------
            Intent::RetryWorker => self.handle_retry_worker(ctx),

            // ---- Diagnostics overlays (alerts / mPING / GPS) ----------
            Intent::Diagnostics(intent) => self.handle_diagnostics_intent(ctx, intent),
            Intent::ShowAlertOnMap(id) => self.handle_show_alert_on_map(id),
        }
    }

    /// Apply a diagnostics overlay intent through the pure
    /// [`crate::core::diagnostics::reduce`], executing any effects it returns.
    pub(crate) fn handle_diagnostics_intent(
        &mut self,
        ctx: &egui::Context,
        intent: crate::core::diagnostics::DiagnosticsIntent,
    ) {
        let effects = crate::core::diagnostics::reduce(
            crate::core::diagnostics::DiagnosticsStateMut {
                alerts: &mut self.diagnostics.alerts,
                mping: &mut self.diagnostics.mping,
                gps: &mut self.diagnostics.gps,
                gps_layer_active: &mut self.state.layer_state.geo.gps_location,
            },
            intent,
        );
        self.apply_effects(ctx, effects);
    }

    /// "Show on map": enable the alert's overlay class and center the 2D view on
    /// its bbox centroid (pure [`crate::core::diagnostics::compute_alert_focus`]),
    /// then close the detail modal. Cross-cuts diagnostics + viz, so it lives in
    /// the shell where both are reachable.
    fn handle_show_alert_on_map(&mut self, id: String) {
        let Some(alert) = self.diagnostics.alerts.find(&id) else {
            return;
        };
        let focus = crate::core::diagnostics::compute_alert_focus(alert);

        if focus.is_warning {
            self.state.layer_state.geo.alerts_warnings = true;
        } else {
            self.state.layer_state.geo.alerts_other = true;
        }
        if let Some((center_lat, center_lon)) = focus.center {
            self.state.viz_state.center_lat = center_lat;
            self.state.viz_state.center_lon = center_lon;
            self.state
                .viz_state
                .set_pan_offset(eframe::egui::Vec2::ZERO);
            self.state
                .viz_state
                .camera
                .center_on(center_lat, center_lon);
        }
        // The original "Show on map" closed the detail modal after focusing.
        self.diagnostics.alerts.selected_alert_id = None;
    }

    /// Retry a failed archive download — the documented two-state-machine
    /// trap. `AcquisitionState::retry_failed` resets the *operation* to Queued,
    /// but the download pump dispatches from `DownloadQueueManager` items, whose
    /// failed item was marked Done. So we must ALSO re-enqueue a `QueueItem`
    /// (reusing the same operation id so both machines stay correlated). Without
    /// the requeue the retry resets the drawer row but never re-fetches.
    fn handle_retry_failed(&mut self, op_id: crate::core::OperationId) {
        // Reconstruct the scan's download params from the operation kind before
        // we flip it back to Queued.
        let scan_params = self.acquisition.state.find(op_id).and_then(|op| {
            if let state::OperationKind::ArchiveDownload {
                file_name,
                scan_start,
                scan_end,
                ..
            } = &op.kind
            {
                Some((file_name.clone(), *scan_start, *scan_end))
            } else {
                None
            }
        });

        // Reset the operation row (Queued, front of pending) regardless.
        self.acquisition.state.retry_failed(op_id);

        let Some((file_name, scan_start, scan_end)) = scan_params else {
            // Non-download operations (listings/realtime) have no queue item to
            // re-enqueue; the status reset is all retry means for them.
            return;
        };

        // Derive the UTC date the archive file lives under, and scope the fetch
        // to the active elevation filter (mirroring the reactive-prefetch path).
        let Some(date) = chrono::DateTime::from_timestamp(scan_start, 0).map(|dt| dt.date_naive())
        else {
            return;
        };
        let elevation_filter = self.active_elevation_filter();

        let item = crate::nexrad::download_queue::QueueItem::new(
            date,
            file_name,
            scan_start,
            scan_end,
            elevation_filter,
        )
        .with_operation(op_id);
        self.acquisition.coordinator.download_queue.requeue(item);
    }

    /// Explicitly fetch one scan (the inspector's tap-to-fetch / "fetch whole
    /// scan"). `elevation_filter = Some(n)` scopes decode/storage to that tilt
    /// ("fetch this sweep"); `None` stores the whole volume. Creates a tracked
    /// operation and re-enqueues, reusing the same `requeue` path the failed-cell
    /// retry uses so the two state machines stay correlated.
    fn handle_fetch_scan(&mut self, scan_start: i64, elevation_filter: Option<u8>) {
        let site_id = self.state.viz_state.site_id.clone();
        let Some(date) = chrono::DateTime::from_timestamp(scan_start, 0).map(|dt| dt.date_naive())
        else {
            return;
        };

        // Resolve the archive file name + scan span from the listing. Without a
        // listing there's nothing to fetch (the inspector only offers fetch when
        // the scan is known to exist server-side).
        let Some((file_name, scan_end)) = self
            .acquisition
            .coordinator
            .archive_index
            .get(&site_id, &date)
            .and_then(|listing| {
                listing
                    .scan_at_or_before(scan_start)
                    .filter(|(_, b)| {
                        (b.start - scan_start).abs() <= crate::FALLBACK_SCAN_DURATION_SECS
                    })
                    .map(|(file, b)| (file.name.clone(), b.end))
            })
        else {
            self.state.status_message =
                "Can't fetch yet — still listing the archive for that time".to_string();
            return;
        };

        let op_id =
            self.acquisition
                .state
                .create_operation(state::OperationKind::ArchiveDownload {
                    site_id: site_id.clone(),
                    file_name: file_name.clone(),
                    scan_start,
                    scan_end,
                });
        let item = crate::nexrad::download_queue::QueueItem::new(
            date,
            file_name,
            scan_start,
            scan_end,
            elevation_filter,
        )
        .with_operation(op_id);
        self.acquisition.coordinator.download_queue.requeue(item);
    }

    /// The active elevation filter for fetch scoping: a Fixed cut scopes ingest
    /// to that elevation; Latest fetches the whole volume.
    fn active_elevation_filter(&self) -> Option<u8> {
        match &self.state.viz_state.elevation_selection {
            crate::core::ElevationSelection::Fixed {
                elevation_number, ..
            } => Some(*elevation_number),
            crate::core::ElevationSelection::Latest => None,
        }
    }

    fn handle_clear_cache(&mut self, ctx: &egui::Context) {
        if !self.acquisition.coordinator.cache_load_channel.is_loading() {
            self.acquisition
                .coordinator
                .cache_load_channel
                .clear_cache(ctx.clone(), self.acquisition.coordinator.facade().clone());
            // An explicit cache wipe is one of the cases where blanking IS
            // correct (spec §11.2): the data the displayed frame came from is
            // gone, so drop it rather than holding a stale frame with a
            // discrepancy caption.
            self.clear_display_no_scan();
        } else {
            // Cache loader is busy; replay the request on the next frame.
            self.state.push_command(crate::core::Intent::ClearCache);
        }
    }

    fn handle_wipe_all(&mut self) {
        let facade = self.acquisition.coordinator.facade().clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = facade.clear_all().await {
                log::error!("Failed to clear IndexedDB: {}", e);
            }
            if let Some(window) = web_sys::window() {
                if let Ok(Some(storage)) = window.local_storage() {
                    let _ = storage.clear();
                }
                let _ = window.location().reload();
            }
        });
    }

    fn handle_refresh_timeline(&mut self, ctx: &egui::Context, auto_position: bool) {
        if auto_position {
            self.state.auto_position_on_timeline_load = true;
        }
        if !self.acquisition.coordinator.cache_load_channel.is_loading() {
            self.acquisition
                .coordinator
                .cache_load_channel
                .load_site_timeline(
                    ctx.clone(),
                    self.acquisition.coordinator.facade().clone(),
                    self.state.viz_state.site_id.clone(),
                );
        } else {
            // Cache loader is busy; replay (without auto-position) next frame.
            self.state
                .push_command(crate::core::Intent::RefreshTimeline {
                    auto_position: false,
                });
        }
    }

    fn handle_check_eviction(&mut self, ctx: &egui::Context) {
        let facade = self.acquisition.coordinator.facade().clone();
        let quota = self.state.storage_settings.quota_bytes;
        let target = self.state.storage_settings.eviction_target_bytes;
        let ctx_clone = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match facade.check_and_evict(quota, target).await {
                Ok((evicted, count, quota_warning)) => {
                    if evicted {
                        log::debug!("Eviction complete: removed {} scans", count);
                    }
                    if let Some(warning) = quota_warning {
                        log::warn!("Quota warning: {}", warning);
                    }
                }
                Err(e) => {
                    log::error!("Eviction check failed: {}", e);
                }
            }
            ctx_clone.request_repaint();
        });
    }

    fn handle_retry_worker(&mut self, ctx: &egui::Context) {
        match self.render.coordinator.create_worker(ctx.clone()) {
            Ok(()) => {
                self.state.worker_init_error = None;
                self.state.status_message = "Decode worker initialized".to_string();
            }
            Err(msg) => {
                self.state.worker_init_error = Some(msg);
            }
        }
    }
}
