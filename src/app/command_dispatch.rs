//! Drain the [`AppCommand`] queue and execute each command.
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
//!    (`DownloadSelection`, `DownloadAtPosition`, `pump_queue`).
//!
//! The three deferred flags wait for worker results because newly-decoded
//! sweeps may add scans to the cache; running the download/pump path
//! against that fresh state avoids issuing duplicate downloads.

use crate::{state, WorkbenchApp};
use eframe::egui;

/// Deferred work signaled by [`WorkbenchApp::dispatch_commands`].
///
/// `update()` fans these out **after** worker results have been drained
/// so the download/queue path sees the latest cache state.
#[derive(Default)]
pub(crate) struct CommandOutcome {
    /// Kick off a download of every scan in the selection range.
    pub download_selection: bool,
    /// Kick off a single download at the current playback cursor.
    pub download_at_position: bool,
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
        cmd: state::AppCommand,
        outcome: &mut CommandOutcome,
    ) {
        use state::AppCommand;
        match cmd {
            // ---- Storage lifecycle ------------------------------------
            AppCommand::ClearCache => self.handle_clear_cache(ctx),
            AppCommand::WipeAll => self.handle_wipe_all(),
            AppCommand::CheckEviction => self.handle_check_eviction(ctx),

            // ---- Timeline ---------------------------------------------
            AppCommand::RefreshTimeline { auto_position } => {
                self.handle_refresh_timeline(ctx, auto_position);
            }

            // ---- Live mode --------------------------------------------
            AppCommand::StartLive => self.start_live_mode(ctx),

            // ---- Downloads (deferred to after worker results) ---------
            AppCommand::DownloadSelection => outcome.download_selection = true,
            AppCommand::DownloadAtPosition => outcome.download_at_position = true,

            // ---- Queue management -------------------------------------
            // Mutations that may unblock work flip `pump_queue` so the
            // post-results queue pump runs in the same frame.
            AppCommand::PauseQueue => self.acquisition.state.pause(),
            AppCommand::ResumeQueue => {
                self.acquisition.state.resume();
                outcome.pump_queue = true;
            }
            AppCommand::RetryFailed(op_id) => {
                self.acquisition.state.retry_failed(op_id);
                outcome.pump_queue = true;
            }
            AppCommand::SkipFailed(op_id) => {
                self.acquisition.state.skip_failed(op_id);
                outcome.pump_queue = true;
            }
            AppCommand::CancelOperation(op_id) => self.acquisition.state.cancel_operation(op_id),
            AppCommand::ReorderOperation(op_id, delta) => {
                self.acquisition.state.reorder_operation(op_id, delta);
            }

            // ---- Worker lifecycle -------------------------------------
            AppCommand::RetryWorker => self.handle_retry_worker(ctx),

            // ---- Alerts -----------------------------------------------
            AppCommand::RefreshAlerts => self.diagnostics.alerts.refresh_requested = true,
            AppCommand::OpenAlert(id) => self.diagnostics.alerts.selected_alert_id = Some(id),
            AppCommand::CloseAlert => {
                self.diagnostics.alerts.selected_alert_id = None;
                self.diagnostics.alerts.list_modal_open = false;
            }
        }
    }

    fn handle_clear_cache(&mut self, ctx: &egui::Context) {
        if !self.acquisition.coordinator.cache_load_channel.is_loading() {
            self.acquisition
                .coordinator
                .cache_load_channel
                .clear_cache(ctx.clone(), self.acquisition.coordinator.facade().clone());
        } else {
            // Cache loader is busy; replay the request on the next frame.
            self.state.push_command(state::AppCommand::ClearCache);
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
            self.state.push_command(state::AppCommand::RefreshTimeline {
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
        match self.render.create_worker(ctx.clone()) {
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
