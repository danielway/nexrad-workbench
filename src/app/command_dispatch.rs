//! Drain the [`Intent`] queue and execute each command.
//!
//! Commands flow from UI panels via [`AppState::push_command`] and are
//! drained once per frame. Each command falls into one of three shapes:
//!
//! 1. **Immediate state mutation** — flips a flag or mutates `AppState`
//!    directly (e.g. `PauseQueue`, `OpenAlert`).
//! 2. **Async side-effect** — spawns a future via `spawn_local`
//!    (e.g. `CheckEviction`).
//! 3. **Deferred fan-out** — recorded on [`CommandOutcome`] so the
//!    `update()` loop can run the work after worker results land
//!    (`pump_queue`).
//!
//! The deferred `pump_queue` flag waits for worker results because
//! newly-decoded sweeps may add scans to the cache; running the queue pump
//! against that fresh state avoids issuing duplicate downloads. (Archive
//! acquisition itself is reactive — see `pump_implicit_prefetch`.)
//!
//! # The routing partition
//!
//! Dispatch happens in two halves so the *wiring* is testable, not just the
//! individual reducers:
//!
//! - [`dispatch_state_only`] owns every intent that is a pure state
//!   transition over the [`DispatchState`] bundle — no `spawn_local`, no GPU,
//!   no [`egui::Context`], no worker `postMessage`. It runs headlessly, so
//!   `mod dispatch_tests` can dispatch a real intent into a real state bundle
//!   and assert the *observable* outcome. That is the one defect class the
//!   per-reducer unit tests cannot see: an intent wired to the wrong handler
//!   (`SkipFailed` routed to the retry path) compiles and passes every
//!   reducer test.
//! - Whatever [`dispatch_state_only`] does not own comes back as
//!   `Some(intent)` and [`WorkbenchApp::handle_command`] runs it against the
//!   full shell.
//!
//! Both matches are exhaustive, so a new [`Intent`] variant forces an explicit
//! decision on which side of the partition it belongs — and
//! `dispatch_tests::{state_only_intents_never_reach_the_shell,
//! shell_intents_are_left_for_the_shell}` pin the partition itself.

use crate::{state, subsystem, WorkbenchApp};
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

/// The slice of [`WorkbenchApp`] that the state-only intents mutate.
///
/// Deliberately narrow: everything a state-only handler can reach is listed
/// here, and everything listed here is reachable without a browser. That is
/// what lets [`dispatch_state_only`] be driven from a headless test fixture.
/// The GPU, the worker pool, the persistence manager, the modals and the
/// timeline inventory are absent because no state-only arm touches them.
pub(crate) struct DispatchState<'a> {
    pub state: &'a mut state::AppState,
    pub playback: &'a mut subsystem::Playback,
    pub acquisition: &'a mut subsystem::Acquisition,
    pub live: &'a mut subsystem::Live,
    pub diagnostics: &'a mut subsystem::Diagnostics,
    pub chrome: &'a mut subsystem::Chrome,
}

/// Handle the intents that are pure state transitions. Returns `Some(intent)`
/// for intents that need the shell (browser I/O, GPU, worker dispatch, egui
/// context), which `handle_command` then executes.
///
/// The returned intent is always the one that came in, untouched — the shell
/// half owns it end-to-end rather than finishing a partially-applied
/// transition.
pub(crate) fn dispatch_state_only(
    intent: crate::core::Intent,
    s: &mut DispatchState<'_>,
    outcome: &mut CommandOutcome,
) -> Option<crate::core::Intent> {
    use crate::core::Intent;
    match intent {
        // ---- Live mode / transport --------------------------------
        Intent::StopLive(placement) => {
            s.transport(|env, slices| {
                crate::core::transport::reduce_stop_live(env, slices, placement)
            });
        }
        Intent::CenterTimelineOnNow => {
            let now = s.state.frame_now.secs();
            s.playback.state.center_view_on(now);
        }
        Intent::TogglePlayPause => s.transport(crate::core::transport::reduce_toggle_play_pause),

        // ---- Loop presets -----------------------------------------
        Intent::ClearLoop => s.playback.state.clear_selection(),

        // ---- Queue management -------------------------------------
        // Mutations that may unblock work flip `pump_queue` so the
        // post-results queue pump runs in the same frame.
        Intent::PauseQueue => s.acquisition.state.pause(),
        Intent::ResumeQueue => {
            s.acquisition.state.resume();
            outcome.pump_queue = true;
        }
        Intent::SetActivitySheetOpen(open) => s.chrome.activity_sheet_open = open,
        Intent::SetActivityDetailsOpen(open) => s.chrome.activity_details_open = open,
        Intent::RetryAllFailed => {
            for id in s.acquisition.state.failed_operation_ids() {
                s.retry_failed(id);
            }
            outcome.pump_queue = true;
        }
        Intent::SetAutofetchWhileScrubbing(on) => s.state.autofetch_while_scrubbing = on,
        Intent::SetPauseStreamWhileReviewing(on) => s.state.pause_stream_while_reviewing = on,
        Intent::RetryFailed(op_id) => {
            s.retry_failed(op_id);
            outcome.pump_queue = true;
        }
        Intent::FetchScan {
            scan_start,
            elevation_filter,
        } => {
            s.fetch_scan(scan_start, elevation_filter);
            outcome.pump_queue = true;
        }
        Intent::SkipFailed(op_id) => {
            s.acquisition.state.skip_failed(op_id);
            outcome.pump_queue = true;
        }
        Intent::CancelOperation(op_id) => s.acquisition.state.cancel_operation(op_id),
        Intent::ReorderOperation(op_id, delta) => {
            s.acquisition.state.reorder_operation(op_id, delta);
        }

        // ---- Canvas / map overlays --------------------------------
        Intent::ShowAlertOnMap(id) => s.show_alert_on_map(id),
        Intent::PlaceDistancePoint { lat, lon } => s.place_distance_point(lat, lon),
        Intent::SetGeoLayer(layer, on) => layer.set(&mut s.state.layer_state.geo, on),

        // ---- Needs the shell --------------------------------------
        // Listed explicitly (not `_`) so a new intent forces a decision
        // about which half of the partition owns it. The reason each one
        // is here:
        //   SelectSite ........ tears down GPU/render/stream state
        //   OpenExternalUrl ... web_sys window.open
        //   LocateMeForSite ... browser geolocation
        //   SubmitZip ......... network geocode lookup
        //   ClearCache ........ IDB clear + GPU texture wipe
        //   ResetSettings ..... localStorage reset + reload
        //   CheckEviction ..... spawn_local IDB eviction
        //   RefreshTimeline ... IDB cache-load channel (needs egui ctx)
        //   StartLive ......... opens the realtime stream (needs egui ctx)
        //   GoLive ............ StartLive
        //   ReturnToLive ...... StartLive on the cold path
        //   ApplyLoopPreset ... ReturnToLive on the re-tether path
        //   RetryWorker ....... constructs the decode worker
        //   Diagnostics ....... reducer effects (geolocation / URL open)
        shell @ (Intent::SelectSite { .. }
        | Intent::OpenExternalUrl(_)
        | Intent::LocateMeForSite
        | Intent::SubmitZip(_)
        | Intent::ClearCache
        | Intent::ResetSettings
        | Intent::CheckEviction
        | Intent::RefreshTimeline { .. }
        | Intent::StartLive
        | Intent::GoLive
        | Intent::ReturnToLive
        | Intent::ApplyLoopPreset(_)
        | Intent::RetryWorker
        | Intent::Diagnostics(_)) => return Some(shell),
    }
    None
}

impl DispatchState<'_> {
    /// Run one pure transport reducer over the core state and execute the
    /// [`crate::core::transport::TransportActions`] it describes.
    ///
    /// The reducers ([`crate::core::transport::reduce_toggle_play_pause`],
    /// [`crate::core::transport::reduce_stop_live`]) share an Env/Slices/Actions
    /// signature, so the shell side is this one adapter.
    fn transport(
        &mut self,
        reduce: impl FnOnce(
            &crate::core::transport::TransportEnv,
            crate::core::transport::TransportSlices<'_>,
        ) -> crate::core::transport::TransportActions,
    ) {
        let actions = reduce(
            &crate::core::transport::TransportEnv {
                now_secs: self.state.frame_now.secs(),
                pause_stream_while_reviewing: self.state.pause_stream_while_reviewing,
            },
            crate::core::transport::TransportSlices {
                live_mode: &mut self.live.mode_state,
                engine: &mut self.live.engine.borrow_mut(),
                playback: &mut self.playback.state,
            },
        );
        if actions.stop_channel {
            self.live.channel.stop();
        }
        if let Some(msg) = actions.status_message {
            self.state.status_message = msg;
        }
    }

    /// Place the next distance-tool endpoint at `(lat, lon)`. Which endpoint
    /// that is comes from the pure
    /// [`crate::core::canvas::decide_distance_click`].
    fn place_distance_point(&mut self, lat: f64, lon: f64) {
        let viz = &mut self.state.viz_state;
        match crate::core::canvas::decide_distance_click(
            viz.distance_start.is_some(),
            viz.distance_end.is_some(),
        ) {
            crate::core::canvas::DistancePlacement::Start => {
                viz.distance_start = Some((lat, lon));
                viz.distance_end = None;
            }
            crate::core::canvas::DistancePlacement::End => {
                viz.distance_end = Some((lat, lon));
            }
        }
    }

    /// "Show on map": enable the alert's overlay class and center the 2D view on
    /// its bbox centroid (pure [`crate::core::diagnostics::compute_alert_focus`]),
    /// then close the detail modal. Cross-cuts diagnostics + viz, so it lives in
    /// the dispatch bundle where both are reachable.
    fn show_alert_on_map(&mut self, id: String) {
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
        // Both surfaces must close so a detail opened from the list exposes the
        // newly focused map instead of immediately resurfacing the list.
        self.diagnostics.alerts.selected_alert_id = None;
        self.diagnostics.alerts.list_modal_open = false;
    }

    /// Retry a failed archive download — the documented two-state-machine
    /// trap. `AcquisitionState::retry_failed` resets the *operation* to Queued,
    /// but the download pump dispatches from `DownloadQueueManager` items, whose
    /// failed item was marked Done. So we must ALSO re-enqueue a `QueueItem`
    /// (reusing the same operation id so both machines stay correlated). Without
    /// the requeue the retry resets the drawer row but never re-fetches.
    fn retry_failed(&mut self, op_id: crate::core::OperationId) {
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
    fn fetch_scan(&mut self, scan_start: i64, elevation_filter: Option<u8>) {
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

        // State-only intents are applied against the narrow bundle first; what
        // comes back is the shell's half of the partition.
        let cmd = {
            let mut bundle = DispatchState {
                state: &mut self.state,
                playback: &mut self.playback,
                acquisition: &mut self.acquisition,
                live: &mut self.live,
                diagnostics: &mut self.diagnostics,
                chrome: &mut self.chrome,
            };
            dispatch_state_only(cmd, &mut bundle, outcome)
        };
        let Some(cmd) = cmd else {
            return;
        };

        match cmd {
            // ---- Site selection + external links ----------------------
            Intent::SelectSite { site_id, lat, lon } => {
                self.apply_site_selection(&site_id, lat, lon)
            }
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
            Intent::ResetSettings => self.handle_reset_settings(),
            Intent::CheckEviction => self.handle_check_eviction(ctx),

            // ---- Timeline ---------------------------------------------
            Intent::RefreshTimeline { auto_position } => {
                self.handle_refresh_timeline(ctx, auto_position);
            }

            // ---- Live mode --------------------------------------------
            Intent::StartLive => self.start_live_mode(ctx),
            Intent::GoLive => {
                // GO LIVE from any surface: drop the selection first (so the
                // pinned playhead isn't fenced by stale bounds), then open the
                // stream. Realtime lock is applied inside `enter_pinned_live`.
                self.playback.state.clear_selection();
                self.start_live_mode(ctx);
            }
            Intent::ReturnToLive => self.return_to_live(ctx),

            // ---- Loop presets -----------------------------------------
            Intent::ApplyLoopPreset(preset) => self.apply_loop_preset(preset, ctx),

            // ---- Worker lifecycle -------------------------------------
            Intent::RetryWorker => self.handle_retry_worker(ctx),

            // ---- Diagnostics overlays (alerts / mPING / GPS) ----------
            Intent::Diagnostics(intent) => self.handle_diagnostics_intent(ctx, intent),

            // ---- Already applied by `dispatch_state_only` -------------
            // Listed rather than caught by `_` so a new intent has to be
            // classified in BOTH halves of the partition before it compiles.
            Intent::StopLive(_)
            | Intent::CenterTimelineOnNow
            | Intent::TogglePlayPause
            | Intent::ClearLoop
            | Intent::PauseQueue
            | Intent::ResumeQueue
            | Intent::SetActivitySheetOpen(_)
            | Intent::SetActivityDetailsOpen(_)
            | Intent::RetryAllFailed
            | Intent::SetAutofetchWhileScrubbing(_)
            | Intent::SetPauseStreamWhileReviewing(_)
            | Intent::RetryFailed(_)
            | Intent::FetchScan { .. }
            | Intent::SkipFailed(_)
            | Intent::CancelOperation(_)
            | Intent::ReorderOperation(..)
            | Intent::ShowAlertOnMap(_)
            | Intent::PlaceDistancePoint { .. }
            | Intent::SetGeoLayer(..) => {
                unreachable!("state-only intent was not consumed by dispatch_state_only")
            }
        }
    }

    /// Apply a site selection: retarget viz, center the camera, remember the
    /// preference, close the modal, and queue the timeline/alerts refresh.
    ///
    /// Every site-picking surface (modal list, zip/geolocation result, canvas
    /// marker click) routes here through [`Intent::SelectSite`], so this is the
    /// single place a site change happens.
    fn apply_site_selection(&mut self, site_id: &str, lat: f64, lon: f64) {
        self.state.viz_state.site_id = site_id.to_string();
        self.state.viz_state.center_lat = lat;
        self.state.viz_state.center_lon = lon;
        self.state
            .viz_state
            .set_pan_offset(eframe::egui::Vec2::ZERO);
        self.state.viz_state.camera.center_on(lat, lon);
        self.state
            .push_command(crate::core::Intent::RefreshTimeline {
                auto_position: true,
            });
        self.state.push_command(crate::core::Intent::Diagnostics(
            crate::core::diagnostics::DiagnosticsIntent::RefreshAlerts,
        ));
        self.state.preferred_site = Some(site_id.to_string());
        self.chrome.site_modal_open = false;

        // Boot-tether deferred from a first visit (no site at launch): now that
        // a site exists, open tethered to live (spec §7). One-shot — consumed
        // here so later mid-session site re-selections don't auto-tether.
        if std::mem::take(&mut self.state.start_live_on_site_select) {
            self.state.push_command(crate::core::Intent::StartLive);
        }

        // Tear down the previous radar's stream/GPU/cache state now, in the
        // same step that retargeted `viz_state` — otherwise the frame between
        // here and the next `apply_frame_setup` would paint the old site's
        // sweep under the new site's projection.
        self.sync_to_active_site();
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
            // Reset the acquisition machinery to match the now-empty cache:
            // the queue, its drawer operations, the progress ghosts, the
            // prefetch settle (its resolved marker would otherwise suppress
            // the window pump until the playhead moves), and the request
            // ledger. The anchor pump then issues exactly ONE fetch for the
            // playhead scan, held Pending in the ledger through the whole
            // multi-second IDB-wipe blackout — this is what stops the old
            // ~25x re-request storm. An armed range selection deliberately
            // survives (it re-fills its span after the wipe).
            self.acquisition.coordinator.download_queue.clear();
            self.acquisition.state.cancel_all();
            self.state.download_progress.clear();
            self.acquisition.prefetch_settle = crate::core::acquisition::PrefetchSettle::default();
            self.acquisition.request_ledger.clear();
        } else {
            // Cache loader is busy; replay the request on the next frame.
            self.state.push_command(crate::core::Intent::ClearCache);
        }
    }

    fn handle_reset_settings(&mut self) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            log::error!("Unable to access localStorage while resetting settings");
            return;
        };
        let Ok(len) = storage.length() else {
            log::error!("Unable to read localStorage length while resetting settings");
            return;
        };
        let mut keys = Vec::with_capacity(len as usize);
        for i in 0..len {
            match storage.key(i) {
                Ok(Some(key)) => keys.push(key),
                Ok(None) => {}
                Err(e) => {
                    log::error!("Failed to enumerate localStorage keys: {:?}", e);
                    return;
                }
            }
        }
        for key in crate::state::SavedEvents::keys_to_reset(keys) {
            if let Err(e) = storage.remove_item(&key) {
                log::error!("Failed to remove setting {key}: {:?}", e);
                return;
            }
        }
        let _ = window.location().reload();
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

#[cfg(test)]
mod probe_tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Precondition for the whole dispatch harness: every state owner in the
    /// [`super::DispatchState`] bundle must be constructible with no browser
    /// surface (no GPU context, no worker, no egui `Context`). If one of these
    /// ever grows a constructor that touches the DOM, `dispatch_tests` stops
    /// being buildable — this test says so first, and points at which one.
    #[wasm_bindgen_test]
    fn state_bundle_is_headlessly_constructible() {
        let _s = crate::state::AppState::default();
        let _p = crate::subsystem::Playback::default();
        let _a = crate::subsystem::Acquisition::new(crate::data::MainThreadStore::new());
        let _t = crate::subsystem::Timeline::default();
        let _c = crate::subsystem::Chrome::new();
        let _d = crate::subsystem::Diagnostics::new();
        let _l = crate::subsystem::Live::new(crate::nexrad::live::realtime::RealtimeChannel::new());
    }
}

/// Wiring tests for [`super::dispatch_state_only`].
///
/// Every reducer these arms call is already unit-tested in isolation; what is
/// tested *here* is that each [`Intent`] reaches the right one. The assertions
/// are therefore all on observable state ("the queue reports paused", "that
/// operation is cancelled and its sibling is not"), never on "a function was
/// called" — and the copy-paste twins (`SkipFailed`/`RetryFailed`,
/// `CancelOperation`/`SkipFailed`, `TogglePlayPause`/`StopLive`) are asserted
/// on the property that *differs* between them, so a swapped pair fails.
#[cfg(test)]
mod dispatch_tests {
    use super::{dispatch_state_only, CommandOutcome, DispatchState};
    use crate::core::transport::LiveStopPlacement;
    use crate::core::OperationStatus;
    use crate::core::{
        ElevationSelection, FrameNow, GeoLayer, Intent, LiveExitReason, LivePhase, LoopPreset,
        OperationId,
    };
    use crate::nexrad::acquisition::archive_index::{ArchiveFileMeta, ArchiveListing};
    use crate::state::acquisition::QueueState;
    use crate::state::OperationKind;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Owns the real state bundle the dispatcher mutates, plus the
    /// [`CommandOutcome`] the deferred-work flags land on.
    struct Fixture {
        state: crate::state::AppState,
        playback: crate::subsystem::Playback,
        acquisition: crate::subsystem::Acquisition,
        live: crate::subsystem::Live,
        diagnostics: crate::subsystem::Diagnostics,
        chrome: crate::subsystem::Chrome,
        outcome: CommandOutcome,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                state: crate::state::AppState::default(),
                playback: crate::subsystem::Playback::default(),
                acquisition: crate::subsystem::Acquisition::new(crate::data::MainThreadStore::new()),
                live: crate::subsystem::Live::new(crate::nexrad::RealtimeChannel::new()),
                diagnostics: crate::subsystem::Diagnostics::new(),
                chrome: crate::subsystem::Chrome::new(),
                outcome: CommandOutcome::default(),
            }
        }

        /// Dispatch one intent into the bundle; returns whatever is left for
        /// the shell (`None` when the state half fully handled it).
        fn dispatch(&mut self, intent: Intent) -> Option<Intent> {
            let mut bundle = DispatchState {
                state: &mut self.state,
                playback: &mut self.playback,
                acquisition: &mut self.acquisition,
                live: &mut self.live,
                diagnostics: &mut self.diagnostics,
                chrome: &mut self.chrome,
            };
            dispatch_state_only(intent, &mut bundle, &mut self.outcome)
        }

        /// Dispatch an intent that must be fully applied here.
        fn apply(&mut self, intent: Intent) {
            let left = self.dispatch(intent.clone());
            assert!(
                left.is_none(),
                "{intent:?} should be handled state-only, but the shell got {left:?}"
            );
        }

        /// The operation ids in queue order.
        fn op_order(&self) -> Vec<OperationId> {
            self.acquisition
                .state
                .operations
                .iter()
                .map(|o| o.id)
                .collect()
        }

        fn status(&self, id: OperationId) -> OperationStatus {
            self.acquisition
                .state
                .find(id)
                .expect("operation")
                .status
                .clone()
        }
    }

    fn download_kind(scan_start: i64) -> OperationKind {
        OperationKind::ArchiveDownload {
            site_id: "KDMX".to_string(),
            file_name: format!("KDMX_{scan_start}"),
            scan_start,
            scan_end: scan_start + 300,
        }
    }

    fn warning_alert(id: &str, bbox: (f64, f64, f64, f64)) -> crate::alerts::Alert {
        crate::alerts::Alert {
            id: id.to_string(),
            event: "Tornado Warning".to_string(),
            headline: String::new(),
            description: String::new(),
            instruction: String::new(),
            severity: crate::alerts::AlertSeverity::Extreme,
            urgency: String::new(),
            certainty: String::new(),
            area_desc: String::new(),
            sender: String::new(),
            effective_secs: None,
            onset_secs: None,
            expires_secs: None,
            ends_secs: None,
            geometry: crate::alerts::types::AlertGeometry {
                polygons: Vec::new(),
                bbox: Some(bbox),
            },
            affected_zones: Vec::new(),
            fill_triangles: Vec::new(),
        }
    }

    // ---- the partition itself -----------------------------------------

    /// Every intent the state half owns is fully consumed by it — none leaks
    /// to the shell. Pins the partition from the state side, so a future
    /// refactor cannot quietly demote a state-only intent into shell work.
    #[wasm_bindgen_test]
    fn state_only_intents_never_reach_the_shell() {
        let state_only = vec![
            Intent::StopLive(LiveStopPlacement::LiveEdge),
            Intent::CenterTimelineOnNow,
            Intent::TogglePlayPause,
            Intent::ClearLoop,
            Intent::PauseQueue,
            Intent::ResumeQueue,
            Intent::RetryFailed(1),
            Intent::FetchScan {
                scan_start: 1_000,
                elevation_filter: None,
            },
            Intent::SkipFailed(1),
            Intent::CancelOperation(1),
            Intent::ReorderOperation(1, 1),
            Intent::ShowAlertOnMap("a1".to_string()),
            Intent::PlaceDistancePoint {
                lat: 41.0,
                lon: -93.0,
            },
            Intent::SetGeoLayer(GeoLayer::Cities, false),
        ];
        assert_eq!(state_only.len(), 14);
        for intent in state_only {
            let mut f = Fixture::new();
            assert_eq!(
                f.dispatch(intent.clone()),
                None,
                "{intent:?} must be handled without the shell"
            );
        }
    }

    /// The shell half is returned untouched — same variant, same payload — so
    /// the state half can never partially apply or rewrite a shell intent.
    /// This is also the only pin available for shell-side twins such as
    /// `GoLive` vs `ReturnToLive`, whose handlers both need an egui context.
    #[wasm_bindgen_test]
    fn shell_intents_are_left_for_the_shell() {
        let shell = vec![
            Intent::SelectSite {
                site_id: "KABR".to_string(),
                lat: 45.0,
                lon: -98.0,
            },
            Intent::OpenExternalUrl("https://example.com/changelog"),
            Intent::LocateMeForSite,
            Intent::SubmitZip("50309".to_string()),
            Intent::ClearCache,
            Intent::ResetSettings,
            Intent::CheckEviction,
            Intent::RefreshTimeline {
                auto_position: true,
            },
            Intent::StartLive,
            Intent::GoLive,
            Intent::ReturnToLive,
            Intent::ApplyLoopPreset(LoopPreset::PinToLive),
            Intent::RetryWorker,
            Intent::Diagnostics(crate::core::diagnostics::DiagnosticsIntent::RefreshAlerts),
        ];
        assert_eq!(shell.len(), 14);
        for intent in shell {
            let mut f = Fixture::new();
            let left = f.dispatch(intent.clone());
            assert_eq!(
                left.as_ref(),
                Some(&intent),
                "{intent:?} must reach the shell unchanged"
            );
            assert!(
                !f.outcome.pump_queue,
                "{intent:?} is shell work and must not flag a queue pump here"
            );
        }
    }

    // ---- queue lifecycle ----------------------------------------------

    /// `PauseQueue` pauses the acquisition queue (and only that — pausing does
    /// not unblock anything, so it must not request a pump).
    #[wasm_bindgen_test]
    fn pause_queue_pauses_the_acquisition_queue() {
        let mut f = Fixture::new();
        // `pause` only fires from Running, so give the queue something to do.
        f.acquisition.state.create_operation(download_kind(1_000));
        assert!(!f.acquisition.state.is_paused());

        f.apply(Intent::PauseQueue);

        assert!(f.acquisition.state.is_paused());
        assert!(!f.outcome.pump_queue);
    }

    /// `ResumeQueue` un-pauses AND asks the frame loop to pump — the pump flag
    /// is the half that actually restarts dispatch.
    #[wasm_bindgen_test]
    fn resume_queue_resumes_and_requests_a_pump() {
        let mut f = Fixture::new();
        f.acquisition.state.create_operation(download_kind(1_000));
        f.acquisition.state.pause();
        assert!(f.acquisition.state.is_paused());

        f.apply(Intent::ResumeQueue);

        assert!(!f.acquisition.state.is_paused());
        assert!(f.outcome.pump_queue);
    }

    /// `CancelOperation` cancels exactly the addressed operation; a sibling is
    /// untouched. Cancelling is also NOT a skip: it never forces the queue back
    /// to Running and never requests a pump (that is `SkipFailed`).
    #[wasm_bindgen_test]
    fn cancel_operation_cancels_only_that_operation() {
        let mut f = Fixture::new();
        let a = f.acquisition.state.create_operation(download_kind(1_000));
        let b = f.acquisition.state.create_operation(download_kind(2_000));

        f.apply(Intent::CancelOperation(a));

        assert_eq!(f.status(a), OperationStatus::Cancelled);
        assert_eq!(
            f.status(b),
            OperationStatus::Queued,
            "cancelling one operation must not disturb another"
        );
        assert!(
            !f.outcome.pump_queue,
            "CancelOperation must not request a queue pump (that is SkipFailed)"
        );

        // Cancelling the last operation SETTLES the queue. `skip_failed` — the
        // copy-paste twin — would force it back to Running instead, so this is
        // the assertion that separates the two even when both mark the row
        // Cancelled.
        f.apply(Intent::CancelOperation(b));
        assert_eq!(
            f.acquisition.state.queue_state,
            QueueState::Empty,
            "cancel settles the queue; skip_failed would force it Running"
        );
    }

    /// `ReorderOperation` moves the addressed operation in the requested
    /// direction: `+1` down the queue, `-1` back up.
    #[wasm_bindgen_test]
    fn reorder_operation_moves_in_the_requested_direction() {
        let mut f = Fixture::new();
        let a = f.acquisition.state.create_operation(download_kind(1_000));
        let b = f.acquisition.state.create_operation(download_kind(2_000));
        let c = f.acquisition.state.create_operation(download_kind(3_000));
        assert_eq!(f.op_order(), vec![a, b, c]);

        f.apply(Intent::ReorderOperation(a, 1));
        assert_eq!(f.op_order(), vec![b, a, c], "+1 must move DOWN the queue");

        f.apply(Intent::ReorderOperation(a, -1));
        assert_eq!(f.op_order(), vec![a, b, c], "-1 must move back UP");
    }

    /// `SkipFailed` gives up on the operation: it lands Cancelled and nothing
    /// is re-enqueued for download. Written to fail if it were wired to the
    /// retry path (which would leave it Queued *and* create a queue item).
    #[wasm_bindgen_test]
    fn skip_failed_cancels_the_operation_and_never_requeues_it() {
        let mut f = Fixture::new();
        let a = f.acquisition.state.create_operation(download_kind(1_000));
        f.acquisition.state.mark_active(a);
        f.acquisition.state.mark_failed(a, "boom".to_string());

        f.apply(Intent::SkipFailed(a));

        assert_eq!(
            f.status(a),
            OperationStatus::Cancelled,
            "skip abandons the operation; RetryFailed would re-queue it"
        );
        assert!(
            f.acquisition
                .coordinator
                .download_queue
                .find_by_scan_start(1_000)
                .is_none(),
            "skip must NOT re-enqueue the download (that is RetryFailed)"
        );
        // Skipping explicitly resumes the queue — the one thing that separates
        // it from a plain `CancelOperation`, which would settle it to Empty.
        assert_eq!(
            f.acquisition.state.queue_state,
            QueueState::Running,
            "skip resumes the queue; cancel_operation would settle it Empty"
        );
        assert!(f.outcome.pump_queue);
    }

    /// `RetryFailed` drives both state machines: the operation row goes back to
    /// Queued *and* a `QueueItem` is re-enqueued carrying the same operation id
    /// and the active elevation filter. Written to fail if it were wired to the
    /// skip path (which would leave it Cancelled with an empty download queue).
    #[wasm_bindgen_test]
    fn retry_failed_requeues_the_download_and_never_cancels_it() {
        let mut f = Fixture::new();
        let a = f.acquisition.state.create_operation(download_kind(1_000));
        f.acquisition.state.mark_active(a);
        f.acquisition.state.mark_failed(a, "boom".to_string());

        f.apply(Intent::RetryFailed(a));

        assert_eq!(
            f.status(a),
            OperationStatus::Queued,
            "retry resets the operation; SkipFailed would cancel it"
        );
        let item = f
            .acquisition
            .coordinator
            .download_queue
            .find_by_scan_start(1_000)
            .expect("retry must re-enqueue the download item");
        assert_eq!(item.operation_id, Some(a), "both machines stay correlated");
        // Default viz elevation selection is Fixed { elevation_number: 1 }.
        assert_eq!(item.elevation_filter, Some(1));
        assert!(f.outcome.pump_queue);
    }

    /// `FetchScan` resolves the archive listing, creates a tracked operation,
    /// and enqueues the file at the requested elevation scope.
    #[wasm_bindgen_test]
    fn fetch_scan_creates_an_operation_and_enqueues_the_listed_file() {
        let mut f = Fixture::new();
        let site = f.state.viz_state.site_id.clone();
        let date = chrono::DateTime::from_timestamp(1_000, 0)
            .unwrap()
            .date_naive();
        f.acquisition.coordinator.archive_index.insert(
            &site,
            date,
            ArchiveListing {
                files: vec![ArchiveFileMeta {
                    name: "KDMX19700101_001640_V06".to_string(),
                    timestamp: 1_000,
                }],
                fetched_at: 0.0,
            },
        );

        f.apply(Intent::FetchScan {
            scan_start: 1_000,
            elevation_filter: Some(3),
        });

        let op = f
            .acquisition
            .state
            .operations
            .back()
            .expect("fetch must create an operation");
        assert_eq!(op.status, OperationStatus::Queued);
        assert!(matches!(
            &op.kind,
            OperationKind::ArchiveDownload {
                file_name,
                scan_start: 1_000,
                ..
            } if file_name == "KDMX19700101_001640_V06"
        ));
        let item = f
            .acquisition
            .coordinator
            .download_queue
            .find_by_scan_start(1_000)
            .expect("fetch must enqueue the download item");
        assert_eq!(item.operation_id, Some(op.id));
        assert_eq!(
            item.elevation_filter,
            Some(3),
            "the intent's elevation scope must survive the trip"
        );
        assert!(f.outcome.pump_queue);
    }

    /// With no listing there is nothing to fetch: no operation, no queue item,
    /// and the user is told why.
    #[wasm_bindgen_test]
    fn fetch_scan_without_a_listing_explains_itself_instead_of_enqueuing() {
        let mut f = Fixture::new();

        f.apply(Intent::FetchScan {
            scan_start: 1_000,
            elevation_filter: None,
        });

        assert!(f.acquisition.state.operations.is_empty());
        assert!(f
            .acquisition
            .coordinator
            .download_queue
            .find_by_scan_start(1_000)
            .is_none());
        assert!(f.state.status_message.contains("still listing"));
    }

    // ---- transport / live ---------------------------------------------

    /// `TogglePlayPause` while tethered FREEZES: the playhead detaches and the
    /// lag clock starts, but the stream keeps running. Written to fail if it
    /// were wired to `StopLive`, which tears the stream down.
    #[wasm_bindgen_test]
    fn toggle_play_pause_freezes_without_stopping_the_stream() {
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(5_000.0);
        f.live.mode_state.phase = LivePhase::Streaming;
        f.playback.state.enter_pinned_live(5_000.0);

        f.apply(Intent::TogglePlayPause);

        assert!(!f.playback.state.playing);
        assert!(!f.playback.state.time_model.is_pinned());
        assert!(
            f.live.mode_state.is_active(),
            "a freeze must leave the stream running — stopping it is StopLive"
        );
        assert_eq!(f.live.mode_state.detached_since, Some(5_000.0));
        assert_eq!(f.live.mode_state.last_exit_reason, None);
    }

    /// `StopLive` tears the stream down and drops to ARCHIVE; `LiveEdge`
    /// placement snaps the playhead to now and reports the exit reason.
    /// Written to fail if it were wired to `TogglePlayPause`, which would leave
    /// the stream active and stamp `detached_since` instead.
    #[wasm_bindgen_test]
    fn stop_live_stops_the_stream_and_drops_to_archive() {
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(5_000.0);
        f.live.mode_state.phase = LivePhase::Streaming;
        f.playback.state.enter_pinned_live(4_000.0);
        f.playback.state.playing = true;

        f.apply(Intent::StopLive(LiveStopPlacement::LiveEdge));

        assert!(
            !f.live.mode_state.is_active(),
            "an explicit stop must tear the stream down — freezing it is TogglePlayPause"
        );
        assert_eq!(
            f.live.mode_state.last_exit_reason,
            Some(LiveExitReason::UserStopped)
        );
        assert_eq!(f.live.mode_state.detached_since, None);
        assert!(!f.playback.state.playing);
        assert!(!f.playback.state.time_model.is_pinned());
        assert!((f.playback.state.playback_position() - 5_000.0).abs() < 1e-9);
        assert_eq!(
            f.state.status_message,
            LiveExitReason::UserStopped.message()
        );
    }

    /// The `StopLive` payload reaches the reducer: `InPlace` keeps the playhead
    /// where it was and says nothing, unlike the `LiveEdge` case above.
    #[wasm_bindgen_test]
    fn stop_live_in_place_keeps_the_playhead_and_stays_silent() {
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(5_000.0);
        f.state.status_message = "unchanged".to_string();
        f.live.mode_state.phase = LivePhase::Streaming;
        f.playback.state.enter_pinned_live(4_000.0);

        f.apply(Intent::StopLive(LiveStopPlacement::InPlace));

        assert!((f.playback.state.playback_position() - 4_000.0).abs() < 1e-9);
        assert_eq!(f.state.status_message, "unchanged");
    }

    /// `CenterTimelineOnNow` scrolls the view so this frame's "now" sits in the
    /// middle of the strip.
    #[wasm_bindgen_test]
    fn center_timeline_on_now_centers_the_view_on_frame_now() {
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(10_000.0);
        f.playback.state.timeline_zoom = 1.0;
        f.playback.state.timeline_width_px = 100.0;
        f.playback.state.timeline_view_start = 0.0;

        f.apply(Intent::CenterTimelineOnNow);

        // view width = width_px / zoom = 100s, so the left edge is now - 50s.
        assert!((f.playback.state.timeline_view_start - 9_950.0).abs() < 1e-9);
    }

    /// `ClearLoop` drops the selection and the bounds it produced.
    #[wasm_bindgen_test]
    fn clear_loop_drops_the_selection() {
        let mut f = Fixture::new();
        f.playback.state.set_selection(100.0, 200.0);
        f.playback.state.apply_selection_as_bounds();
        assert_eq!(f.playback.state.selection_range(), Some((100.0, 200.0)));
        assert!(f.playback.state.loop_window.is_some());

        f.apply(Intent::ClearLoop);

        assert_eq!(f.playback.state.selection_range(), None);
        assert!(f.playback.state.loop_window.is_none());
        assert_eq!(f.playback.state.time_model.playback_bounds, None);
    }

    // ---- canvas / overlays --------------------------------------------

    /// `SetGeoLayer` addresses exactly one overlay and carries a value (not a
    /// flip), so repeating it is idempotent and siblings never move.
    #[wasm_bindgen_test]
    fn set_geo_layer_flips_only_the_named_layer() {
        let mut f = Fixture::new();
        let before: Vec<(GeoLayer, bool)> = GeoLayer::all()
            .iter()
            .map(|l| (*l, l.get(&f.state.layer_state.geo)))
            .collect();
        assert!(
            GeoLayer::Cities.get(&f.state.layer_state.geo),
            "Cities ships on, so switching it off is an observable change"
        );

        f.apply(Intent::SetGeoLayer(GeoLayer::Cities, false));

        for (layer, was) in &before {
            let now = layer.get(&f.state.layer_state.geo);
            if *layer == GeoLayer::Cities {
                assert!(!now, "the addressed layer must be off");
            } else {
                assert_eq!(now, *was, "{layer:?} must not have moved");
            }
        }

        // Idempotent, and reversible through the same intent.
        f.apply(Intent::SetGeoLayer(GeoLayer::Cities, false));
        assert!(!GeoLayer::Cities.get(&f.state.layer_state.geo));
        f.apply(Intent::SetGeoLayer(GeoLayer::Cities, true));
        assert!(GeoLayer::Cities.get(&f.state.layer_state.geo));
    }

    /// `PlaceDistancePoint` cycles start → end → start, so a third click
    /// restarts the measurement rather than dragging the old end point.
    #[wasm_bindgen_test]
    fn place_distance_point_cycles_start_end_start() {
        let mut f = Fixture::new();

        f.apply(Intent::PlaceDistancePoint {
            lat: 41.0,
            lon: -93.0,
        });
        assert_eq!(f.state.viz_state.distance_start, Some((41.0, -93.0)));
        assert_eq!(f.state.viz_state.distance_end, None);

        f.apply(Intent::PlaceDistancePoint {
            lat: 42.0,
            lon: -94.0,
        });
        assert_eq!(f.state.viz_state.distance_start, Some((41.0, -93.0)));
        assert_eq!(f.state.viz_state.distance_end, Some((42.0, -94.0)));

        f.apply(Intent::PlaceDistancePoint {
            lat: 43.0,
            lon: -95.0,
        });
        assert_eq!(f.state.viz_state.distance_start, Some((43.0, -95.0)));
        assert_eq!(
            f.state.viz_state.distance_end, None,
            "a finished measurement must restart, not extend"
        );
    }

    /// `ShowAlertOnMap` enables the alert's own overlay class (warnings, not
    /// watches), centers the 2D view on its bbox centroid, and closes the
    /// detail modal.
    #[wasm_bindgen_test]
    fn show_alert_on_map_enables_the_warning_layer_and_centers_on_the_bbox() {
        let mut f = Fixture::new();
        f.state.layer_state.geo.alerts_warnings = false;
        f.state.layer_state.geo.alerts_other = false;
        f.diagnostics
            .alerts
            .alerts
            .push(warning_alert("a1", (-94.0, 41.0, -92.0, 43.0)));
        f.diagnostics.alerts.selected_alert_id = Some("a1".to_string());
        f.diagnostics.alerts.list_modal_open = true;

        f.apply(Intent::ShowAlertOnMap("a1".to_string()));

        assert!(f.state.layer_state.geo.alerts_warnings);
        assert!(
            !f.state.layer_state.geo.alerts_other,
            "a warning must not switch on the watches/advisories layer"
        );
        assert!((f.state.viz_state.center_lat - 42.0).abs() < 1e-9);
        assert!((f.state.viz_state.center_lon + 93.0).abs() < 1e-9);
        assert_eq!(f.diagnostics.alerts.selected_alert_id, None);
        assert!(!f.diagnostics.alerts.list_modal_open);
    }

    /// An unknown alert id is inert — no layer is switched on and the open
    /// modal is left alone.
    #[wasm_bindgen_test]
    fn show_alert_on_map_ignores_an_unknown_id() {
        let mut f = Fixture::new();
        f.state.layer_state.geo.alerts_warnings = false;
        f.state.layer_state.geo.alerts_other = false;
        f.diagnostics.alerts.selected_alert_id = Some("still-open".to_string());

        f.apply(Intent::ShowAlertOnMap("nope".to_string()));

        assert!(!f.state.layer_state.geo.alerts_warnings);
        assert!(!f.state.layer_state.geo.alerts_other);
        assert_eq!(
            f.diagnostics.alerts.selected_alert_id,
            Some("still-open".to_string())
        );
    }

    /// A non-download operation (a realtime chunk) has no queue item to
    /// re-enqueue, so retry is just the status reset — and must not invent a
    /// download.
    #[wasm_bindgen_test]
    fn retry_failed_on_a_non_download_only_resets_the_status() {
        let mut f = Fixture::new();
        let a = f
            .acquisition
            .state
            .create_operation(OperationKind::RealtimeChunk {
                site_id: "KDMX".to_string(),
                chunk_index: 3,
                is_start: false,
                is_end: false,
                scan_timestamp: 1_000,
            });
        f.acquisition.state.mark_active(a);
        f.acquisition.state.mark_failed(a, "boom".to_string());

        f.apply(Intent::RetryFailed(a));

        assert_eq!(f.status(a), OperationStatus::Queued);
        assert!(!f.acquisition.coordinator.download_queue.has_work());
    }

    /// The elevation scope `RetryFailed` re-enqueues with follows the *current*
    /// viz selection, not whatever the original download used.
    #[wasm_bindgen_test]
    fn retry_failed_rescopes_to_the_active_elevation_filter() {
        let mut f = Fixture::new();
        f.state.viz_state.elevation_selection = ElevationSelection::Latest;
        let a = f.acquisition.state.create_operation(download_kind(1_000));
        f.acquisition.state.mark_active(a);
        f.acquisition.state.mark_failed(a, "boom".to_string());

        f.apply(Intent::RetryFailed(a));

        let item = f
            .acquisition
            .coordinator
            .download_queue
            .find_by_scan_start(1_000)
            .expect("retry must re-enqueue");
        assert_eq!(
            item.elevation_filter, None,
            "Latest fetches the whole volume"
        );
    }
}
