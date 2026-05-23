//! Drain the [`AppCommand`] queue and execute each command.
//!
//! Commands flow from UI panels via [`AppState::push_command`] and are
//! drained once per frame. Most commands either spawn an async task or
//! flip a flag the caller (`update()`) uses to fan out work in the same
//! frame (download selection / pump queue).

use crate::{state, WorkbenchApp};
use eframe::egui;

impl WorkbenchApp {
    /// Drain the command queue and execute each command.
    ///
    /// Returns flags for `(download_selection, download_at_position, pump_queue)`.
    /// `update()` uses these flags to fan out the actual queue work after
    /// worker results have been drained (so newly-completed downloads are
    /// visible to the queue pump).
    pub(crate) fn dispatch_commands(&mut self, ctx: &egui::Context) -> (bool, bool, bool) {
        let commands = self.state.drain_commands();
        let mut do_download_selection = false;
        let mut do_download_at_position = false;
        let mut do_pump_queue = false;
        for cmd in commands {
            match cmd {
                state::AppCommand::ClearCache => {
                    if !self.acquisition.cache_load_channel.is_loading() {
                        self.acquisition
                            .cache_load_channel
                            .clear_cache(ctx.clone(), self.acquisition.facade().clone());
                    } else {
                        // Re-enqueue if channel is busy
                        self.state.push_command(state::AppCommand::ClearCache);
                    }
                }
                state::AppCommand::WipeAll => {
                    let facade = self.acquisition.facade().clone();
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
                state::AppCommand::RefreshTimeline { auto_position } => {
                    if auto_position {
                        self.state.auto_position_on_timeline_load = true;
                    }
                    if !self.acquisition.cache_load_channel.is_loading() {
                        self.acquisition.cache_load_channel.load_site_timeline(
                            ctx.clone(),
                            self.acquisition.facade().clone(),
                            self.state.viz_state.site_id.clone(),
                        );
                    } else {
                        self.state.push_command(state::AppCommand::RefreshTimeline {
                            auto_position: false,
                        });
                    }
                }
                state::AppCommand::CheckEviction => {
                    let facade = self.acquisition.facade().clone();
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
                state::AppCommand::StartLive => {
                    self.start_live_mode(ctx);
                }
                state::AppCommand::DownloadSelection => {
                    do_download_selection = true;
                }
                state::AppCommand::DownloadAtPosition => {
                    do_download_at_position = true;
                }
                state::AppCommand::PauseQueue => {
                    self.state.acquisition.pause();
                }
                state::AppCommand::ResumeQueue => {
                    self.state.acquisition.resume();
                    do_pump_queue = true;
                }
                state::AppCommand::RetryFailed(op_id) => {
                    self.state.acquisition.retry_failed(op_id);
                    do_pump_queue = true;
                }
                state::AppCommand::SkipFailed(op_id) => {
                    self.state.acquisition.skip_failed(op_id);
                    do_pump_queue = true;
                }
                state::AppCommand::CancelOperation(op_id) => {
                    self.state.acquisition.cancel_operation(op_id);
                }
                state::AppCommand::ReorderOperation(op_id, delta) => {
                    self.state.acquisition.reorder_operation(op_id, delta);
                }
                state::AppCommand::RetryWorker => match self.render.create_worker(ctx.clone()) {
                    Ok(()) => {
                        self.state.worker_init_error = None;
                        self.state.status_message = "Decode worker initialized".to_string();
                    }
                    Err(msg) => {
                        self.state.worker_init_error = Some(msg);
                    }
                },
                state::AppCommand::RefreshAlerts => {
                    self.state.alerts.refresh_requested = true;
                }
                state::AppCommand::OpenAlert(id) => {
                    self.state.alerts.selected_alert_id = Some(id);
                }
                state::AppCommand::CloseAlert => {
                    self.state.alerts.selected_alert_id = None;
                    self.state.alerts.list_modal_open = false;
                }
            }
        }

        (
            do_download_selection,
            do_download_at_position,
            do_pump_queue,
        )
    }
}
