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
    /// What the canvas should show changed in a way the render dedup can't
    /// see (Frames ↔ Rays swaps a live partial for the last whole sweep);
    /// drop the dedup so the next resolve re-requests.
    pub fresh_render: bool,
}

/// The slice of [`WorkbenchApp`] that the state-only intents mutate.
///
/// Deliberately narrow: everything a state-only handler can reach is listed
/// here, and everything listed here is reachable without a browser. That is
/// what lets [`dispatch_state_only`] be driven from a headless test fixture.
/// The GPU, the worker pool, the persistence manager and the modals are
/// absent because no state-only arm touches them; the timeline inventory is
/// lent read-only, for stepping.
pub(crate) struct DispatchState<'a> {
    pub state: &'a mut state::AppState,
    pub playback: &'a mut subsystem::Playback,
    pub acquisition: &'a mut subsystem::Acquisition,
    pub live: &'a mut subsystem::Live,
    pub diagnostics: &'a mut subsystem::Diagnostics,
    pub chrome: &'a mut subsystem::Chrome,
    /// Read-only: the frames a step can land on.
    pub timeline: &'a subsystem::Timeline,
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
    // A playhead or selection intent takes over from a calendar landing
    // still waiting for its day's listing.
    s.chrome.go_to_day_landing =
        crate::core::calendar::landing_after(s.chrome.go_to_day_landing.take(), &intent);
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
        Intent::StopLooping => s.transport(crate::core::transport::reduce_stop_looping),
        Intent::SelectRange {
            start,
            end,
            to_live,
        } => {
            let now = s.state.frame_now.secs();
            let pause = s.state.pause_stream_while_reviewing;
            if s.live
                .select_range(&mut s.playback.state, start, end, to_live, now, pause)
            {
                // Arm the bulk fetch for the new range (selection = the fetch).
                s.state.selection_just_finalized = s.playback.state.selection_range();
            }
        }
        Intent::SelectFixedRange { start, end } => {
            let now = s.state.frame_now.secs();
            let pause = s.state.pause_stream_while_reviewing;
            if s.live
                .select_fixed_range(&mut s.playback.state, start, end, now, pause)
            {
                s.state.selection_just_finalized = s.playback.state.selection_range();
            }
        }

        Intent::Seek { ts, gesture } => {
            s.transport(|env, slices| crate::core::transport::reduce_seek(env, slices, ts, gesture))
        }
        Intent::Step { dir, unit } => s.step(dir, unit),
        Intent::SetLoopPoint(edge) => {
            let selected = s.transport_with(|env, slices| {
                crate::core::transport::reduce_loop_point(env, slices, edge)
            });
            if selected {
                // Arm the bulk fetch for the new range (selection = the fetch).
                s.state.selection_just_finalized = s.playback.state.selection_range();
            }
        }
        Intent::SetLoopStyle(style) => {
            crate::core::transport::set_loop_style(&mut s.playback.state, style)
        }
        Intent::SetSpeed(speed) => {
            crate::core::transport::reduce_set_speed(&mut s.playback.state, speed)
        }
        Intent::StepSpeed { faster } => {
            crate::core::transport::reduce_step_speed(&mut s.playback.state, faster)
        }
        Intent::SaveSelectionAsEvent => {
            if s.playback.state.selection_range().is_some() {
                s.chrome.event_modal_open = true;
                s.chrome.event_modal_editing_id = None;
            }
        }

        // ---- Loop presets -----------------------------------------
        Intent::ClearLoop => s.playback.state.clear_selection(),
        Intent::SetPlayMode(mode) => {
            s.playback.state.set_play_mode(mode);
            outcome.fresh_render = true;
        }

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
        Intent::ToggleMapTool(tool) => {
            let viz = &mut s.state.viz_state;
            let t = crate::core::panels::toggle_map_tool(
                viz.data_probe_enabled,
                viz.distance_tool_active,
                tool,
            );
            viz.data_probe_enabled = t.probe;
            viz.distance_tool_active = t.distance;
            if t.clear_points {
                viz.distance_start = None;
                viz.distance_end = None;
            }
        }
        Intent::SetGeoLayer(layer, on) => layer.set(&mut s.state.layer_state.geo, on),

        // ---- Top bar ------------------------------------------------
        Intent::TopBar(intent) => s.top_bar(intent),

        // ---- Dialogs (one-instrument §12) ---------------------------
        // Clear cache and reset are destructive: the request opens the
        // confirmation, whose button sends the `*Now` intent to the shell.
        Intent::ClearCache => s.dialog(crate::core::dialogs::DialogIntent::RequestConfirm(
            crate::core::dialogs::Confirm::ClearCache,
        )),
        Intent::ResetSettings => {
            s.chrome.reset_forgets_key_and_site = false;
            s.dialog(crate::core::dialogs::DialogIntent::RequestConfirm(
                crate::core::dialogs::Confirm::ResetSettings,
            ));
        }
        Intent::SetResetForgetsKeyAndSite(on) => s.chrome.reset_forgets_key_and_site = on,
        Intent::SetDeveloperMode(on) => {
            s.state.dev_mode = on;
            s.state.dev_mode_saved = on;
        }
        Intent::Dialog(intent) => s.dialog(intent),
        Intent::LoopFromScan(scan_start) => s.loop_from_scan(scan_start),

        // ---- Phone strip ------------------------------------------
        intent @ Intent::SetPhoneStripZoom(_) => {
            s.chrome.phone_strip_zoom =
                crate::core::phone_strip::strip_zoom_after(s.chrome.phone_strip_zoom, &intent);
        }

        // ---- Calendar sheet ---------------------------------------
        Intent::OpenGoToDay => {
            s.chrome.go_to_day = Some(crate::core::calendar::CalendarSheet::default());
            super::dialogs::sync_dialogs(s.chrome, s.diagnostics);
        }
        Intent::Calendar(nav) => {
            let current = crate::core::calendar::latest_month(s.state.frame_now.secs());
            if let Some(sheet) = s.chrome.go_to_day.as_mut() {
                crate::core::calendar::reduce_nav(sheet, nav, current);
            }
        }
        intent @ Intent::GoToDay(day) => s.go_to_day(day, &intent),
        Intent::GoToTime(ts) => s.go_to_time(ts),

        // ---- The phone strip's long-press sheet -------------------
        Intent::OpenPressSheet(press) => {
            s.chrome.press_sheet = Some(press);
            super::dialogs::sync_dialogs(s.chrome, s.diagnostics);
        }
        Intent::OpenChipSheet => {
            s.chrome.chip_sheet_open = true;
            super::dialogs::sync_dialogs(s.chrome, s.diagnostics);
        }
        Intent::PressSheetRow(row) => {
            let press = s.chrome.press_sheet?;
            let facts = crate::core::phone_range::RangeFacts::of(
                &s.playback.state,
                s.state.frame_now.secs(),
            );
            // A disabled row does nothing and the sheet stays.
            if let Ok(next) = crate::core::phone_range::row_intent(row, &press, &facts) {
                s.chrome.press_sheet = None;
                super::dialogs::sync_dialogs(s.chrome, s.diagnostics);
                if let Some(shell) = dispatch_state_only(next, s, outcome) {
                    s.state.push_command(shell);
                }
            }
        }
        Intent::InspectScan(scan_start) => {
            if scan_start.is_finite() {
                s.chrome.scan_inspector = Some(scan_start);
                super::dialogs::sync_dialogs(s.chrome, s.diagnostics);
            }
        }

        // ---- Needs the shell --------------------------------------
        // Listed explicitly (not `_`) so a new intent forces a decision
        // about which half of the partition owns it. The reason each one
        // is here:
        //   SelectSite ........ tears down GPU/render/stream state
        //   OpenExternalUrl ... web_sys window.open
        //   LocateMeForSite ... browser geolocation
        //   SubmitZip ......... network geocode lookup
        //   ClearCacheNow ..... IDB clear + GPU texture wipe
        //   ResetSettingsNow .. localStorage reset + reload
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
        | Intent::ClearCacheNow
        | Intent::ResetSettingsNow
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
    pub(super) fn transport(
        &mut self,
        reduce: impl FnOnce(
            &crate::core::transport::TransportEnv,
            crate::core::transport::TransportSlices<'_>,
        ) -> crate::core::transport::TransportActions,
    ) {
        self.transport_with(|env, slices| (reduce(env, slices), ()));
    }

    /// [`Self::transport`] for a reducer that also returns a value.
    fn transport_with<R>(
        &mut self,
        reduce: impl FnOnce(
            &crate::core::transport::TransportEnv,
            crate::core::transport::TransportSlices<'_>,
        ) -> (crate::core::transport::TransportActions, R),
    ) -> R {
        let (actions, out) = reduce(
            &crate::core::transport::TransportEnv {
                now_secs: self.state.frame_now.secs(),
                pause_stream_while_reviewing: self.state.pause_stream_while_reviewing,
                frame_secs: self.playback.state.median_frame_spacing(),
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
        out
    }

    /// Apply a top-bar intent. Every arm is a plain state write; the one
    /// decision (what a tilt pick does to the selection) is
    /// [`crate::core::top_bar::apply_tilt_pick`].
    fn top_bar(&mut self, intent: crate::core::top_bar::TopBarIntent) {
        use crate::core::top_bar::{apply_tilt_pick, TopBarIntent};
        use crate::geo::ViewMode;
        match intent {
            TopBarIntent::OpenSitePicker => self.chrome.site_modal_open = true,
            TopBarIntent::SetProduct(product) => {
                // The product picks the cut inside a fixed angle's row.
                let entries = self.state.current_elevation_list(
                    &self.playback.state,
                    &self.timeline.scans,
                    self.live
                        .radar_model
                        .volume
                        .as_ref()
                        .and_then(|v| v.vcp_pattern.as_ref()),
                );
                let viz = &mut self.state.viz_state;
                if let Some(pick) = crate::core::ladder::repick_for_product(
                    &viz.elevation_selection,
                    &entries,
                    product,
                ) {
                    apply_tilt_pick(
                        &mut viz.elevation_selection,
                        &mut viz.last_fixed_selection,
                        pick,
                    );
                }
                viz.product = product;
            }
            TopBarIntent::SetTilt(pick) => {
                let viz = &mut self.state.viz_state;
                apply_tilt_pick(
                    &mut viz.elevation_selection,
                    &mut viz.last_fixed_selection,
                    pick,
                );
            }
            TopBarIntent::ToggleTimeZone => {
                self.state.use_local_time = !self.state.use_local_time;
            }
            TopBarIntent::SetView(ViewMode::Flat2D) => self.state.viz_state.switch_to_2d(),
            TopBarIntent::SetView(ViewMode::Globe3D) => self.state.viz_state.switch_to_3d_view(),
            TopBarIntent::ToggleShortcutsHelp => {
                self.chrome.shortcuts_help_visible = !self.chrome.shortcuts_help_visible;
            }
            TopBarIntent::OpenSettings => {
                self.chrome.rail_tab = crate::core::panels::RailTab::Settings;
                self.chrome.right_sidebar_visible = true;
            }
            TopBarIntent::ToggleRightPanel => {
                self.chrome.right_sidebar_visible = !self.chrome.right_sidebar_visible;
            }
            TopBarIntent::DismissToast => self.state.toasts.dismiss(),
        }
    }

    /// Step through the frames of the selected tilt (any tilt for Latest), or
    /// through whole volumes for a volume step and in volumetric 3D, where a
    /// frame is a scan.
    fn step(
        &mut self,
        dir: crate::core::transport::StepDir,
        unit: crate::core::transport::StepUnit,
    ) {
        let viz = &self.state.viz_state;
        let volumetric = viz.volume_3d_active();
        let tilt = crate::core::tilt::TiltFilter::from(&viz.elevation_selection);
        let timeline = self.timeline;
        let index = crate::core::transport::ScanFrames {
            scans: &timeline.scans,
            tilt,
            volumes: volumetric || unit == crate::core::transport::StepUnit::Volume,
        };
        self.transport(|env, slices| crate::core::transport::reduce_step(env, slices, &index, dir));
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
            self.state
                .toasts
                .notice("Can't fetch yet — still listing the archive for that time");
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

    /// The active elevation filter for fetch scoping (see
    /// [`crate::core::acquisition::fetch_scope`]).
    fn active_elevation_filter(&self) -> Option<u8> {
        self.state.viz_state.fetch_scope().elevation_number()
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
                timeline: &self.timeline,
            };
            dispatch_state_only(cmd, &mut bundle, outcome)
        };
        if std::mem::take(&mut outcome.fresh_render) {
            self.render.coordinator.force_fresh_render();
        }
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
            Intent::ClearCacheNow => self.handle_clear_cache(ctx),
            Intent::ResetSettingsNow => self.handle_reset_settings(),
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
            | Intent::StopLooping
            | Intent::SelectRange { .. }
            | Intent::SelectFixedRange { .. }
            | Intent::Seek { .. }
            | Intent::Step { .. }
            | Intent::SetLoopPoint(_)
            | Intent::SetLoopStyle(_)
            | Intent::SetSpeed(_)
            | Intent::StepSpeed { .. }
            | Intent::SaveSelectionAsEvent
            | Intent::SetPlayMode(_)
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
            | Intent::ToggleMapTool(_)
            | Intent::SetGeoLayer(..)
            | Intent::TopBar(_)
            | Intent::ClearCache
            | Intent::ResetSettings
            | Intent::SetResetForgetsKeyAndSite(_)
            | Intent::SetDeveloperMode(_)
            | Intent::Dialog(_)
            | Intent::LoopFromScan(_)
            | Intent::SetPhoneStripZoom(_)
            | Intent::OpenGoToDay
            | Intent::Calendar(_)
            | Intent::GoToDay(_)
            | Intent::GoToTime(_)
            | Intent::OpenPressSheet(_)
            | Intent::PressSheetRow(_)
            | Intent::OpenChipSheet
            | Intent::InspectScan(_) => {
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
        // Every pick — list, search, zip, location or map — completes the
        // first visit and ends "Pick on map" (restoring the Sites layer).
        self.chrome.site_required = false;
        if let Some(was_on) = self.chrome.site_map_pick.take() {
            self.state.layer_state.geo.nexrad_sites = was_on;
        }
        crate::core::site_picker::push_recent(&mut self.state.recent_sites, site_id);
        self.state.preferred_site = Some(site_id.to_string());
        self.chrome.site_modal_open = false;
        if site_id == self.state.viz_state.site_id {
            // Picking the current site is allowed (a first visit can keep the
            // default site); nothing to retarget, but a deferred boot tether
            // still starts.
            if std::mem::take(&mut self.state.start_live_on_site_select) {
                self.state.push_command(crate::core::Intent::StartLive);
            }
            return;
        }
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
            self.state.push_command(crate::core::Intent::ClearCacheNow);
        }
    }

    fn handle_reset_settings(&mut self) {
        let current = crate::core::Preferences {
            mping_api_key: self.diagnostics.mping.api_key.clone(),
            preferred_site: self.state.preferred_site.clone(),
            ..Default::default()
        };
        let plan = crate::core::prefs::reset_plan(&current, self.chrome.reset_forgets_key_and_site);
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            log::error!("Unable to access localStorage while resetting settings");
            return;
        };
        for key in &plan.remove_keys {
            if let Err(e) = storage.remove_item(key) {
                log::error!("Failed to remove setting {key}: {:?}", e);
                return;
            }
        }
        if let Some(kept) = &plan.write {
            kept.save();
        }
        // Reload with a clean URL so the link cannot re-apply what was just
        // cleared (developer flag, mode, tilt).
        let location = window.location();
        let path = location.pathname().unwrap_or_else(|_| "/".to_string());
        let _ = location.replace(&path);
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
    use crate::core::top_bar::{TiltPick, TopBarIntent};
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
        timeline: crate::subsystem::Timeline,
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
                timeline: crate::subsystem::Timeline::default(),
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
                timeline: &self.timeline,
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

        /// The toast this frame would show, if any.
        fn toast(&mut self) -> Option<String> {
            self.state.toasts.tick(0.0);
            self.state.toasts.view(0.0).map(|t| t.text)
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
            Intent::StopLooping,
            Intent::SelectRange {
                start: 100.0,
                end: 400.0,
                to_live: false,
            },
            Intent::SetPlayMode(crate::core::PlaybackMode::Micro),
            Intent::Seek {
                ts: 150.0,
                gesture: crate::core::transport::SeekGesture::Press,
            },
            Intent::Step {
                dir: crate::core::transport::StepDir::Back,
                unit: crate::core::transport::StepUnit::Frame,
            },
            Intent::SetLoopPoint(crate::core::transport::LoopEdge::In),
            Intent::SetLoopStyle(crate::core::LoopMode::Once),
            Intent::SetSpeed(crate::core::PlaybackSpeed::Double),
            Intent::StepSpeed { faster: true },
            Intent::SaveSelectionAsEvent,
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
            Intent::ToggleMapTool(crate::core::panels::MapTool::Probe),
            Intent::SetGeoLayer(GeoLayer::Cities, false),
            Intent::TopBar(TopBarIntent::ToggleTimeZone),
            Intent::ClearCache,
            Intent::ResetSettings,
            Intent::SetResetForgetsKeyAndSite(true),
            Intent::SetDeveloperMode(true),
            Intent::Dialog(crate::core::dialogs::DialogIntent::Escape),
            Intent::LoopFromScan(1_000.0),
            Intent::SetPhoneStripZoom(crate::core::phone_strip::StripZoom::Day),
            Intent::OpenGoToDay,
            Intent::Calendar(crate::core::calendar::CalendarNav::Years),
            Intent::GoToDay(crate::core::calendar::CalendarDay::of(
                chrono::NaiveDate::from_ymd_opt(2024, 5, 6).unwrap(),
                &crate::core::calendar::FixedZone(0),
            )),
            Intent::GoToTime(1_000.0),
            Intent::OpenPressSheet(crate::core::phone_range::PressSheet {
                ts: 1_000.0,
                scan_start: None,
                volume: None,
                newest: false,
                anchor_volume: None,
            }),
            Intent::PressSheetRow(crate::core::phone_range::PressRow::Inspect),
            Intent::OpenChipSheet,
            Intent::SelectFixedRange {
                start: 100.0,
                end: 400.0,
            },
            Intent::InspectScan(1_000.0),
        ];
        assert_eq!(state_only.len(), 42);
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
            Intent::ClearCacheNow,
            Intent::ResetSettingsNow,
            Intent::CheckEviction,
            Intent::RefreshTimeline {
                auto_position: true,
            },
            Intent::StartLive,
            Intent::GoLive,
            Intent::ReturnToLive,
            Intent::ApplyLoopPreset(LoopPreset::LastFrames(6)),
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
        assert!(f.toast().is_some_and(|t| t.contains("still listing")));
    }

    // ---- top bar ------------------------------------------------------

    /// Each top-bar intent lands on the state it names — written to fail if
    /// two arms were swapped (both panel toggles, both views).
    #[wasm_bindgen_test]
    fn top_bar_intents_write_the_state_they_name() {
        let mut f = Fixture::new();
        let local = f.state.use_local_time;
        f.apply(Intent::TopBar(TopBarIntent::ToggleTimeZone));
        assert_eq!(f.state.use_local_time, !local);

        f.apply(Intent::TopBar(TopBarIntent::OpenSitePicker));
        assert!(f.chrome.site_modal_open);

        f.apply(Intent::TopBar(TopBarIntent::SetProduct(
            crate::core::RadarProduct::Velocity,
        )));
        assert!(f.state.viz_state.product == crate::core::RadarProduct::Velocity);

        f.state.viz_state.elevation_selection = ElevationSelection::Fixed {
            elevation_number: 3,
            angle: 1.3,
        };
        f.apply(Intent::TopBar(TopBarIntent::SetTilt(TiltPick::Latest)));
        assert!(f.state.viz_state.elevation_selection == ElevationSelection::Latest);
        assert_eq!(f.state.viz_state.last_fixed_selection, Some((3, 1.3)));

        let right = f.chrome.right_sidebar_visible;
        f.apply(Intent::TopBar(TopBarIntent::ToggleRightPanel));
        assert_eq!(f.chrome.right_sidebar_visible, !right);
        f.apply(Intent::TopBar(TopBarIntent::OpenSettings));
        assert!(f.chrome.right_sidebar_visible);
        assert_eq!(f.chrome.rail_tab, crate::core::panels::RailTab::Settings);

        let help = f.chrome.shortcuts_help_visible;
        f.apply(Intent::TopBar(TopBarIntent::ToggleShortcutsHelp));
        assert_eq!(f.chrome.shortcuts_help_visible, !help);

        f.apply(Intent::TopBar(TopBarIntent::SetView(
            crate::geo::ViewMode::Globe3D,
        )));
        assert_eq!(f.state.viz_state.view_mode(), crate::geo::ViewMode::Globe3D);
        f.apply(Intent::TopBar(TopBarIntent::SetView(
            crate::geo::ViewMode::Flat2D,
        )));
        assert_eq!(f.state.viz_state.view_mode(), crate::geo::ViewMode::Flat2D);
    }

    // ---- dialogs ------------------------------------------------------

    fn drain(f: &mut Fixture) -> Vec<Intent> {
        f.state.drain_commands()
    }

    /// Escape closes only the top entry: the alert detail first, then the
    /// list under it.
    #[wasm_bindgen_test]
    fn escape_closes_the_top_dialog_only() {
        use crate::core::dialogs::DialogIntent;
        let mut f = Fixture::new();
        f.diagnostics.alerts.list_modal_open = true;
        crate::app::dialogs::sync_dialogs(&mut f.chrome, &f.diagnostics);
        f.diagnostics.alerts.selected_alert_id = Some("a1".into());
        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert!(f.diagnostics.alerts.selected_alert_id.is_none());
        assert!(f.diagnostics.alerts.list_modal_open);
        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert!(!f.diagnostics.alerts.list_modal_open);
        assert!(f.chrome.dialogs.is_empty());
    }

    /// The first-visit site picker survives Escape and its close mark; a
    /// pick clears the requirement.
    #[wasm_bindgen_test]
    fn the_required_site_picker_cannot_be_dismissed() {
        use crate::core::dialogs::{DialogId, DialogIntent};
        let mut f = Fixture::new();
        f.chrome.site_modal_open = true;
        f.chrome.site_required = true;
        f.apply(Intent::Dialog(DialogIntent::Escape));
        f.apply(Intent::Dialog(DialogIntent::Close(DialogId::SitePicker)));
        assert!(f.chrome.site_modal_open);
        f.chrome.site_required = false;
        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert!(!f.chrome.site_modal_open);
    }

    /// Clear cache asks first; confirming sends the shell intent, cancelling
    /// sends nothing.
    #[wasm_bindgen_test]
    fn clear_cache_goes_through_the_confirmation() {
        use crate::core::dialogs::{Confirm, DialogId, DialogIntent};
        let mut f = Fixture::new();
        f.apply(Intent::ClearCache);
        assert_eq!(f.chrome.confirm, Some(Confirm::ClearCache));
        assert_eq!(f.chrome.dialogs.top(), Some(DialogId::Confirm));
        assert!(drain(&mut f).is_empty());
        f.apply(Intent::Dialog(DialogIntent::Confirm));
        assert_eq!(f.chrome.confirm, None);
        assert_eq!(drain(&mut f), vec![Intent::ClearCacheNow]);

        f.apply(Intent::ResetSettings);
        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert_eq!(f.chrome.confirm, None);
        assert!(drain(&mut f).is_empty());
    }

    /// The Reset button's intent opens the shared confirmation.
    #[wasm_bindgen_test]
    fn reset_settings_opens_the_confirmation() {
        let mut f = Fixture::new();
        f.apply(Intent::ResetSettings);
        assert_eq!(
            f.chrome.confirm,
            Some(crate::core::dialogs::Confirm::ResetSettings)
        );
    }

    /// Deleting an event confirms over the event dialog; Escape returns to
    /// the dialog, confirming deletes and closes both.
    #[wasm_bindgen_test]
    fn delete_event_confirms_over_the_event_dialog() {
        use crate::core::dialogs::{Confirm, DialogId, DialogIntent};
        let mut f = Fixture::new();
        f.state
            .saved_events
            .add("Derecho".into(), "KDMX".into(), 1.0, 2.0);
        let id = f.state.saved_events.events[0].id;
        f.chrome.event_modal_open = true;
        f.chrome.event_modal_editing_id = Some(id);
        f.apply(Intent::Dialog(DialogIntent::RequestConfirm(
            Confirm::DeleteEvent(id),
        )));
        assert_eq!(f.chrome.dialogs.top(), Some(DialogId::Confirm));
        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert!(f.chrome.event_modal_open);
        assert_eq!(f.state.saved_events.events.len(), 1);

        f.apply(Intent::Dialog(DialogIntent::RequestConfirm(
            Confirm::DeleteEvent(id),
        )));
        f.apply(Intent::Dialog(DialogIntent::Confirm));
        for intent in drain(&mut f) {
            f.apply(intent);
        }
        assert!(f.state.saved_events.events.is_empty());
        assert!(!f.chrome.event_modal_open);
    }

    /// Saving validates in the core and closes the dialog; an invalid save
    /// leaves it open.
    #[wasm_bindgen_test]
    fn save_event_validates_and_closes() {
        use crate::core::dialogs::DialogIntent;
        let mut f = Fixture::new();
        f.chrome.event_modal_open = true;
        let save = |name: &str, start: f64, end: f64| {
            Intent::Dialog(DialogIntent::SaveEvent {
                editing: None,
                name: name.into(),
                site_id: "KDMX".into(),
                start,
                end,
            })
        };
        f.apply(save("x", 5.0, 1.0));
        assert!(f.chrome.event_modal_open);
        assert!(f.state.saved_events.events.is_empty());
        f.apply(save(" Derecho ", 1.0, 5.0));
        assert!(!f.chrome.event_modal_open);
        assert_eq!(f.state.saved_events.events[0].name, "Derecho");
    }

    /// Pick on map turns the Sites layer on and closes the picker; Escape
    /// restores the layer.
    #[wasm_bindgen_test]
    fn pick_on_map_restores_the_sites_layer_on_cancel() {
        use crate::core::dialogs::{DialogId, DialogIntent};
        let mut f = Fixture::new();
        f.chrome.site_modal_open = true;
        f.state.layer_state.geo.nexrad_sites = false;
        f.apply(Intent::Dialog(DialogIntent::PickSiteOnMap));
        assert!(f.state.layer_state.geo.nexrad_sites);
        assert!(!f.chrome.site_modal_open);
        assert_eq!(f.chrome.dialogs.top(), Some(DialogId::PickSiteOnMap));
        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert!(!f.state.layer_state.geo.nexrad_sites);
        assert!(f.chrome.dialogs.is_empty());
    }

    /// Loop from a scan more than 7 days back fails with a toast and keeps
    /// the inspector open.
    #[wasm_bindgen_test]
    fn loop_from_an_old_scan_says_why_and_keeps_the_inspector() {
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(20.0 * 86_400.0);
        f.chrome.scan_inspector = Some(1_000.0);
        f.apply(Intent::LoopFromScan(1_000.0));
        assert_eq!(f.chrome.scan_inspector, Some(1_000.0));
        assert!(f.toast().is_some_and(|t| t.contains("7 days")));
    }

    /// Loop from a recent scan makes the range to now, puts the playhead at
    /// the scan and closes the inspector.
    #[wasm_bindgen_test]
    fn loop_from_a_recent_scan_loops_to_now_and_closes_the_inspector() {
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(10_000.0);
        f.chrome.scan_inspector = Some(7_000.0);
        f.apply(Intent::LoopFromScan(7_000.0));
        assert_eq!(f.chrome.scan_inspector, None);
        assert_eq!(
            f.playback.state.selection_range(),
            Some((7_000.0, 10_000.0))
        );
        assert_eq!(f.playback.state.playback_position(), 7_000.0);
    }

    #[wasm_bindgen_test]
    fn dismiss_toast_clears_the_toast_on_screen() {
        let mut f = Fixture::new();
        f.state.toasts.notice("Forecast copied");
        assert_eq!(f.toast().as_deref(), Some("Forecast copied"));
        f.apply(Intent::TopBar(TopBarIntent::DismissToast));
        assert_eq!(f.state.toasts.view(0.0), None);
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
        assert!(!f.playback.state.is_pinned());
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
        assert!(!f.playback.state.is_pinned());
        assert!((f.playback.state.playback_position() - 5_000.0).abs() < 1e-9);
        // The cap turning to GO LIVE says it; no toast (one-instrument §10).
        assert_eq!(f.toast(), None);
    }

    /// The `StopLive` payload reaches the reducer: `InPlace` keeps the playhead
    /// where it was; neither placement says anything.
    #[wasm_bindgen_test]
    fn stop_live_in_place_keeps_the_playhead_and_stays_silent() {
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(5_000.0);
        f.live.mode_state.phase = LivePhase::Streaming;
        f.playback.state.enter_pinned_live(4_000.0);

        f.apply(Intent::StopLive(LiveStopPlacement::InPlace));

        assert!((f.playback.state.playback_position() - 4_000.0).abs() < 1e-9);
        assert_eq!(f.toast(), None);
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

    /// `ClearLoop` drops the range and the bounds it produced.
    #[wasm_bindgen_test]
    fn clear_loop_drops_the_selection() {
        let mut f = Fixture::new();
        assert!(f.playback.state.select_range(100.0, 200.0, false));
        assert_eq!(f.playback.state.selection_range(), Some((100.0, 200.0)));

        f.apply(Intent::ClearLoop);

        assert_eq!(f.playback.state.selection_range(), None);
        assert_eq!(
            f.playback.state.selection,
            crate::core::Selection::Point { live: false }
        );
        assert_eq!(f.playback.state.playback_bounds(), None);
    }

    /// `SelectRange` loops the span and arms its bulk fetch; `StopLooping`
    /// then drops back to a fixed point where the playhead is.
    #[wasm_bindgen_test]
    fn select_range_then_stop_looping() {
        let mut f = Fixture::new();
        f.apply(Intent::SelectRange {
            start: 400.0,
            end: 100.0,
            to_live: false,
        });
        assert_eq!(f.playback.state.playback_bounds(), Some((100.0, 400.0)));
        assert_eq!(f.state.selection_just_finalized, Some((100.0, 400.0)));

        f.apply(Intent::StopLooping);

        assert_eq!(f.playback.state.playback_bounds(), None);
        assert_eq!(
            f.playback.state.selection,
            crate::core::Selection::Point { live: false }
        );
    }

    /// "Save as event" opens the event dialog only when there is a range to
    /// save.
    #[wasm_bindgen_test]
    fn save_selection_as_event_needs_a_range() {
        let mut f = Fixture::new();
        f.apply(Intent::SaveSelectionAsEvent);
        assert!(!f.chrome.event_modal_open);

        assert!(f.playback.state.select_range(100.0, 400.0, false));
        f.chrome.event_modal_editing_id = Some(7);
        f.apply(Intent::SaveSelectionAsEvent);
        assert!(f.chrome.event_modal_open);
        assert_eq!(f.chrome.event_modal_editing_id, None, "a new event");
    }

    /// I then O at two playhead positions loops the span between them and
    /// arms its fetch; the loop style sticks to the range.
    #[wasm_bindgen_test]
    fn in_out_and_loop_style_reach_the_transport() {
        use crate::core::transport::{LoopEdge, SeekGesture};
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(10_000.0);
        f.apply(Intent::Seek {
            ts: 1_000.0,
            gesture: SeekGesture::Press,
        });
        f.apply(Intent::SetLoopPoint(LoopEdge::In));
        assert_eq!(f.state.selection_just_finalized, None);
        f.apply(Intent::Seek {
            ts: 2_000.0,
            gesture: SeekGesture::Press,
        });
        f.apply(Intent::SetLoopPoint(LoopEdge::Out));
        assert_eq!(f.playback.state.playback_bounds(), Some((1_000.0, 2_000.0)));
        assert_eq!(f.state.selection_just_finalized, Some((1_000.0, 2_000.0)));

        f.apply(Intent::SetLoopStyle(crate::core::LoopMode::PingPong));
        assert_eq!(
            f.playback.state.time_model.loop_mode,
            crate::core::LoopMode::PingPong
        );
    }

    /// A step with no frames known still moves (a gap is never a dead end),
    /// and pauses.
    #[wasm_bindgen_test]
    fn step_moves_through_a_gap_and_pauses() {
        use crate::core::transport::{SeekGesture, StepDir, StepUnit};
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(10_000.0);
        f.apply(Intent::Seek {
            ts: 5_000.0,
            gesture: SeekGesture::Press,
        });
        f.playback.state.playing = true;
        f.apply(Intent::Step {
            dir: StepDir::Back,
            unit: StepUnit::Frame,
        });
        assert!(f.playback.state.playback_position() < 5_000.0);
        assert!(!f.playback.state.playing);
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

    /// Choosing a mode sticks through any zoom, and the speed always stays
    /// one the chosen mode can express (an fps in Frames).
    #[wasm_bindgen_test]
    fn set_play_mode_survives_zoom_and_keeps_speed_valid() {
        use crate::core::PlaybackMode;
        let mut f = Fixture::new();
        f.apply(Intent::SetPlayMode(PlaybackMode::Micro));
        assert_eq!(f.playback.state.playback_mode(), PlaybackMode::Micro);
        // The canvas source can change (live partial ↔ last whole sweep).
        assert!(f.outcome.fresh_render);
        for zoom in [0.01, 0.5, 2.0] {
            f.playback.state.set_timeline_zoom(zoom, 1000.0);
            assert_eq!(f.playback.state.playback_mode(), PlaybackMode::Micro);
        }
        f.playback.state.speed = crate::core::PlaybackSpeed::Realtime;
        f.apply(Intent::SetPlayMode(PlaybackMode::Macro));
        assert_eq!(f.playback.state.playback_mode(), PlaybackMode::Macro);
        assert!(f.playback.state.speed.macro_frames_per_second().is_some());
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

    /// `SetPhoneStripZoom` moves the phone strip's stop and nothing else: the
    /// selection and the playhead stay where they were.
    #[wasm_bindgen_test]
    fn phone_strip_zoom_changes_the_stop_not_the_selection() {
        use crate::core::phone_strip::StripZoom;
        let mut f = Fixture::new();
        assert_eq!(f.chrome.phone_strip_zoom, StripZoom::Cuts);
        f.apply(Intent::SelectRange {
            start: 100.0,
            end: 400.0,
            to_live: false,
        });
        let range = f.playback.state.selection_range();
        let pos = f.playback.state.playback_position();
        assert!(range.is_some());

        f.apply(Intent::SetPhoneStripZoom(StripZoom::Day));

        assert_eq!(f.chrome.phone_strip_zoom, StripZoom::Day);
        assert_eq!(f.playback.state.selection_range(), range);
        assert_eq!(f.playback.state.playback_position(), pos);
    }

    // ---- calendar sheet -----------------------------------------------

    /// The calendar sheet opens from its intent, is an entry on the dialog
    /// stack (so Escape closes it), and moves between its views by intent.
    #[wasm_bindgen_test]
    fn the_calendar_sheet_opens_navigates_and_closes_on_escape() {
        use crate::core::calendar::{CalendarMode, CalendarNav, YearMonth};
        use crate::core::dialogs::{DialogId, DialogIntent};
        let mut f = Fixture::new();
        // 2026-10-17.
        f.state.frame_now = FrameNow(1_792_238_400.0);
        f.apply(Intent::OpenGoToDay);
        assert_eq!(f.chrome.go_to_day.map(|s| s.month), Some(None));
        assert!(f.chrome.dialogs.contains(DialogId::GoToDay));

        f.apply(Intent::Calendar(CalendarNav::Year(2013)));
        let sheet = f.chrome.go_to_day.unwrap();
        assert_eq!(sheet.mode, CalendarMode::Year);
        assert_eq!(sheet.month.map(|m| m.year), Some(2013));
        f.apply(Intent::Calendar(CalendarNav::Month(YearMonth::new(
            2013, 5,
        ))));
        assert_eq!(
            f.chrome.go_to_day.unwrap().month,
            Some(YearMonth::new(2013, 5))
        );

        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert_eq!(f.chrome.go_to_day, None);
        // Navigation with the sheet closed does nothing.
        f.apply(Intent::Calendar(CalendarNav::Years));
        assert_eq!(f.chrome.go_to_day, None);
    }

    /// A tapped day: the 3 H stop, the playhead detached and paused at the
    /// day's midday while its listing is out, the sheet closed, and a
    /// landing left to settle. A later seek takes over from the landing.
    #[wasm_bindgen_test]
    fn go_to_day_lands_at_midday_pauses_and_zooms_to_volumes() {
        use crate::core::calendar::{CalendarDay, FixedZone};
        use crate::core::phone_strip::StripZoom;
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(1_792_238_400.0);
        f.state.viz_state.site_id = "KTLX".to_string();
        f.playback.state.playing = true;
        f.apply(Intent::OpenGoToDay);
        let day = CalendarDay::of(
            chrono::NaiveDate::from_ymd_opt(2024, 5, 6).unwrap(),
            &FixedZone(-5 * 3600),
        );
        f.apply(Intent::GoToDay(day));
        assert_eq!(f.chrome.phone_strip_zoom, StripZoom::Volumes);
        assert_eq!(f.chrome.go_to_day, None, "the sheet closes");
        assert!(!f.playback.state.playing, "paused on the day");
        assert_eq!(f.playback.state.playback_position(), day.midday());
        let landing = f.chrome.go_to_day_landing.clone().expect("waits");
        assert_eq!(landing.landed_at, day.midday());

        f.apply(Intent::Seek {
            ts: day.midday() + 600.0,
            gesture: crate::core::transport::SeekGesture::Press,
        });
        assert_eq!(f.chrome.go_to_day_landing, None, "the user took over");
    }

    /// On the desktop the picked day fills the strip and the overview frames
    /// it, as a click on an overview bar does.
    #[wasm_bindgen_test]
    fn go_to_day_shows_the_day_on_the_desktop_strip() {
        use crate::core::calendar::{CalendarDay, FixedZone};
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(1_792_238_400.0);
        f.state.viz_state.site_id = "KTLX".to_string();
        f.playback.state.timeline_width_px = 1200.0;
        f.apply(Intent::OpenGoToDay);
        let day = CalendarDay::of(
            chrono::NaiveDate::from_ymd_opt(2024, 5, 6).unwrap(),
            &FixedZone(-5 * 3600),
        );
        f.apply(Intent::GoToDay(day));
        let ps = &f.playback.state;
        assert!((ps.timeline_view_start - day.start).abs() < 1e-6);
        assert!((ps.view_width_secs() - 86_400.0).abs() < 1.0);
        assert!(ps.overview.start <= day.start && ps.overview.end() >= day.end);
    }

    /// A time typed in the calendar: the playhead pauses on it, the overview
    /// frames it at 48 h, the calendar closes, and a pending landing is
    /// dropped.
    #[wasm_bindgen_test]
    fn go_to_time_pauses_on_the_time_and_frames_the_overview() {
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(1_792_238_400.0);
        f.state.viz_state.site_id = "KTLX".to_string();
        f.playback.state.timeline_width_px = 1200.0;
        f.playback.state.playing = true;
        f.apply(Intent::OpenGoToDay);
        let ts = 1_728_516_600.0; // 2024-10-09 23:30 UTC
        f.apply(Intent::GoToTime(ts));
        let ps = &f.playback.state;
        assert_eq!(ps.playback_position(), ts);
        assert!(!ps.playing);
        let start = ps.timeline_view_start;
        assert!(start <= ts && ts <= start + ps.view_width_secs());
        assert!((ps.overview.start + ps.overview.span / 2.0 - ts).abs() < 1.0);
        assert_eq!(f.chrome.go_to_day, None);
    }

    /// A day picked while streaming stops the stream: the archive day offers
    /// GO LIVE, not REJOIN.
    #[wasm_bindgen_test]
    fn go_to_day_while_streaming_stops_the_stream() {
        use crate::core::calendar::{CalendarDay, FixedZone};
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(1_792_238_400.0);
        f.state.viz_state.site_id = "KTLX".to_string();
        f.live.mode_state.phase = LivePhase::Streaming;
        f.playback.state.enter_pinned_live(1_792_238_000.0);
        let day = CalendarDay::of(
            chrono::NaiveDate::from_ymd_opt(2024, 5, 6).unwrap(),
            &FixedZone(0),
        );
        f.apply(Intent::GoToDay(day));
        assert!(!f.live.mode_state.is_active(), "the stream stopped");
        assert!(!f.playback.state.is_pinned());
        assert!(!f.playback.state.playing);
        assert_eq!(f.playback.state.playback_position(), day.midday());
    }

    /// A day already listed lands on its stormiest volume at once.
    #[wasm_bindgen_test]
    fn go_to_a_listed_day_lands_on_its_stormiest_volume() {
        use crate::core::calendar::{CalendarDay, FixedZone};
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(1_792_238_400.0);
        f.state.viz_state.site_id = "KTLX".to_string();
        let date = chrono::NaiveDate::from_ymd_opt(2024, 5, 6).unwrap();
        let day = CalendarDay::of(date, &FixedZone(0));
        f.timeline.activity.insert_many([crate::data::DayActivity {
            site: crate::data::SiteId::new("KTLX"),
            day_start: day.start as i64,
            volumes: 3,
            mean_bytes: 6_000_000,
            format: "V06".into(),
            fetched_at_ms: (day.start as i64 + 2 * 86_400) * 1000,
            has_scans: true,
            scan_secs: vec![600, 7_200, 30_000],
            scan_bytes: vec![3_000_000, 12_000_000, 3_000_000],
        }]);
        f.apply(Intent::GoToDay(day));
        // Just inside the volume's listed start, as the strip lands it.
        assert_eq!(f.playback.state.playback_position(), day.start + 7_210.0);
        let landing = f.chrome.go_to_day_landing.clone().expect("waits");
        assert_eq!(
            landing.volume.map(|v| v.start),
            Some(day.start + 7_200.0),
            "for the volume"
        );
    }
    // ---- the long-press sheet -----------------------------------------

    fn press_at(volume_start: f64, scan: Option<f64>) -> crate::core::phone_range::PressSheet {
        crate::core::phone_range::PressSheet {
            ts: volume_start + 100.0,
            scan_start: scan,
            volume: Some((volume_start, volume_start + 420.0)),
            newest: false,
            anchor_volume: None,
        }
    }

    /// The long-press sheet opens on the Esc stack and closes on Escape.
    #[wasm_bindgen_test]
    fn the_press_sheet_opens_on_the_stack_and_closes_on_escape() {
        use crate::core::dialogs::{DialogId, DialogIntent};
        let mut f = Fixture::new();
        f.apply(Intent::OpenPressSheet(press_at(5_000.0, Some(5_000.0))));
        assert!(f.chrome.dialogs.contains(DialogId::PressSheet));
        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert_eq!(f.chrome.press_sheet, None);
        assert!(f.chrome.dialogs.is_empty());
    }

    /// Inspect opens the inspector on the scan under the finger and closes
    /// the sheet; a disabled row leaves the sheet open and changes nothing.
    #[wasm_bindgen_test]
    fn inspect_opens_the_inspector_and_a_disabled_row_does_nothing() {
        use crate::core::phone_range::PressRow;
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(10_000.0);
        f.apply(Intent::OpenPressSheet(press_at(5_000.0, None)));
        f.apply(Intent::PressSheetRow(PressRow::Inspect));
        assert!(f.chrome.press_sheet.is_some(), "no scan: disabled");
        assert_eq!(f.chrome.scan_inspector, None);
        f.apply(Intent::PressSheetRow(PressRow::LoopToHere));
        assert!(f.chrome.press_sheet.is_some(), "no loop: disabled");

        f.apply(Intent::OpenPressSheet(press_at(5_000.0, Some(4_980.0))));
        f.apply(Intent::PressSheetRow(PressRow::Inspect));
        assert_eq!(f.chrome.press_sheet, None);
        assert_eq!(f.chrome.scan_inspector, Some(4_980.0));
    }

    /// Loop from here makes a range from the pressed volume to now; Loop to
    /// here then fixes its end at another volume's end, paused.
    #[wasm_bindgen_test]
    fn loop_from_here_then_to_here_makes_a_fixed_range() {
        use crate::core::phone_range::PressRow;
        let mut f = Fixture::new();
        f.state.frame_now = FrameNow(100_000.0);
        f.apply(Intent::OpenPressSheet(press_at(90_000.0, None)));
        f.apply(Intent::PressSheetRow(PressRow::LoopFromHere));
        assert_eq!(f.chrome.press_sheet, None);
        assert_eq!(
            f.playback.state.playback_bounds().map(|(s, _)| s),
            Some(90_000.0)
        );
        // A loop pinned to now: Loop to here fixes its end.
        f.playback.state.enter_pinned_live(100_000.0);
        f.playback
            .state
            .enter_live_range(Some(90_000.0), crate::core::LoopBasis::FrameCount(6));
        // The live tick slides its window over the frames.
        f.playback.state.slide_live_range(90_000.0, 100_000.0);
        assert!(f.playback.state.is_live_range());
        f.apply(Intent::OpenPressSheet(press_at(95_000.0, None)));
        f.apply(Intent::PressSheetRow(PressRow::LoopToHere));
        assert_eq!(
            f.playback.state.playback_bounds(),
            Some((90_000.0, 95_420.0))
        );
        assert!(!f.playback.state.is_live_range());
        // A parked start (the desktop's I key) pairs too.
        f.apply(Intent::ClearLoop);
        f.playback.state.drag = Some(crate::core::TimeSelection {
            a: 90_000.0,
            b: 90_000.0,
            in_progress: false,
        });
        f.apply(Intent::OpenPressSheet(press_at(95_000.0, None)));
        f.apply(Intent::PressSheetRow(PressRow::LoopToHere));
        assert_eq!(f.chrome.press_sheet, None);
        assert_eq!(
            f.playback.state.playback_bounds(),
            Some((90_000.0, 95_420.0))
        );
        assert!(!f.playback.state.playing, "a new range starts paused");
    }

    // ---- the chip sheet ------------------------------------------------

    /// A tap on the phone's chip opens its sheet on the Esc stack.
    #[wasm_bindgen_test]
    fn the_chip_sheet_opens_on_the_stack_and_closes_on_escape() {
        use crate::core::dialogs::{DialogId, DialogIntent};
        let mut f = Fixture::new();
        f.apply(Intent::OpenChipSheet);
        assert!(f.chrome.chip_sheet_open);
        assert!(f.chrome.dialogs.contains(DialogId::ChipSheet));
        f.apply(Intent::Dialog(DialogIntent::Escape));
        assert!(!f.chrome.chip_sheet_open);
        assert!(f.chrome.dialogs.is_empty());
    }
}
