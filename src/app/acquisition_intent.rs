//! Reactive (implicit) prefetch — acquisition as a side effect of navigation.
//!
//! PRODUCT.md §5 specifies that downloading is a *function of what the user
//! looks at*, not a manual chore: the user navigates and acquisition reacts.
//! This module runs, each frame, the pumps that keep the shared download queue
//! and the archive listings in step with the settled playback position (plus a
//! bounded lookahead, scoped by the active filter).
//!
//! Every pump is a thin shell: it assembles read-only inputs, calls the pure
//! reducers in [`crate::core::acquisition`] (window computation, gating,
//! dedup/churn guards, queue-shaping), then executes the described actions in
//! field order — listing fetches, operation creation, and queue appends. All
//! guardrails against runaway fetching (PRODUCT.md §5.1: the settle debounce,
//! the concurrency cap, the session volume cap) are decided in the core.
//!
//! Dedup is layered: the reducers skip scans already cached (per active
//! elevation) or already queued, and the download path itself makes the
//! authoritative per-(elevation) cache-hit decision before any network call.

use crate::core::acquisition::{
    self as acquisition_core, ListingSnapshot, ScanFetchIntent, WindowDayInput,
    WindowIntentActions, WindowPlan,
};
use crate::nexrad::acquisition::archive_index::ArchiveListing;
use crate::nexrad::download_queue::{QueueItem, QueueOrigin};
use crate::{state, WorkbenchApp};
use eframe::egui;

/// Snapshot a cached day listing into the core reducers' input form
/// (file names + inferred scan boundaries, index-aligned).
fn snapshot_listing(listing: &ArchiveListing) -> ListingSnapshot<'_> {
    ListingSnapshot {
        file_names: listing.files.iter().map(|f| f.name.as_str()).collect(),
        boundaries: listing.scan_boundaries(),
    }
}

impl WorkbenchApp {
    /// Reactive archive prefetch. Runs every frame (step 9.5) but is gated by a
    /// settle debounce and an idempotency marker, so it does real work only
    /// when the view has settled on something not yet cached/queued.
    pub(crate) fn pump_implicit_prefetch(&mut self, ctx: &egui::Context) {
        // Preconditions: a worker to ingest into; an archive-positioned
        // playhead (while *attached* to the live edge the stream owns its
        // own acquisition and prefetching "ahead of now" is meaningless — a
        // detached background stream browses the archive and prefetches
        // normally); the queue not paused; the data-saver
        // `autofetch_while_scrubbing` policy on (explicit range selections
        // and the inspector's tap-to-fetch still fetch — the user asked for
        // those — but seeking/scrubbing does not); and no scrub drag currently
        // holding the playhead (requests wait for the drag to settle).
        let playhead_attached =
            self.playback.state.is_pinned() || self.playback.state.is_live_range();
        if !self.render.coordinator.has_worker() {
            return;
        }
        for site in 0..self.sites.list.len() {
            // Per site: an attached playhead hands acquisition to the site's
            // stream only while that stream runs. A site whose stream never
            // connected or gave up still reads the archive, so it shows its
            // latest volume beside the sites that stream.
            let stream_owns = acquisition_core::stream_owns_acquisition(
                playhead_attached,
                self.sites.list[site].live.channel.is_active(),
            );
            if !acquisition_core::reactive_prefetch_allowed(
                stream_owns,
                self.acquisition.state.is_paused(),
                self.state.autofetch_while_scrubbing,
                self.state.pointer_scrub_active,
            ) {
                continue;
            }
            self.pump_implicit_prefetch_for(ctx, site);
        }
    }

    /// [`Self::pump_implicit_prefetch`] for one site, past the shared
    /// preconditions.
    fn pump_implicit_prefetch_for(&mut self, ctx: &egui::Context, site: usize) {
        // Fast path, ahead of the settle debounce: the scan under the
        // playhead right now. A click into a shadow region starts its fetch
        // on this very frame instead of after the 300 ms settle.
        self.pump_anchor_fast_path(site);

        // Phase 1: debounce + volume-cap gate + window plan (pure; mutates
        // only the site's settle state, a field apart from the id it reads).
        let target = &mut self.sites.list[site];
        let env = acquisition_core::ReactivePrefetchEnv {
            auto_fetch_cap_reached: self
                .acquisition
                .coordinator
                .download_queue
                .auto_fetch_cap_reached(),
            now_ms: js_sys::Date::now(),
            debounce_ms: crate::PREFETCH_DEBOUNCE_MS,
            site_id: &target.id.0,
            product_worker_string: self.state.viz_state.product.to_worker_string(),
            fallback_scan_duration_secs: crate::FALLBACK_SCAN_DURATION_SECS,
        };
        let scope = self.state.viz_state.fetch_scope();
        let plan = match acquisition_core::plan_reactive_prefetch(
            &env,
            &self.playback.state,
            &scope,
            &mut target.prefetch_settle,
        ) {
            acquisition_core::ReactivePrefetchPlan::Skip => return,
            acquisition_core::ReactivePrefetchPlan::CapReached { status_message } => {
                // Surface it so an idle canvas isn't mistaken for breakage
                // (PRODUCT.md §7.2).
                self.state.toasts.notice(status_message);
                return;
            }
            acquisition_core::ReactivePrefetchPlan::Window(plan) => plan,
        };

        // Phase 2 on a fresh snapshot (the anchor fast path above may have
        // just enqueued): reduce the settled window to concrete actions,
        // then execute them.
        let actions = self.reduce_window_plan(site, &plan);
        let listing_pending =
            self.execute_window_actions(ctx, site, actions, QueueOrigin::Reactive);

        // Listings may still be in flight for adjacent dates; only mark the
        // view resolved once everything is enqueued and nothing is pending.
        if !listing_pending {
            self.sites.list[site].prefetch_settle.mark_resolved();
        }
    }

    /// The debounce-free remedy for "scrub into a shadow = blank canvas":
    /// enqueue the listed-but-uncached scan under the playhead immediately.
    /// Idempotent and cheap — the satisfied check hits on every subsequent
    /// frame — so it runs every frame ahead of the settle gate. Listings
    /// themselves are left to the debounced pump and the visible-range pump.
    fn pump_anchor_fast_path(&mut self, site: usize) {
        let pos = self.playback.state.playback_position() as i64;
        let Some(date) = chrono::DateTime::from_timestamp(pos, 0).map(|dt| dt.date_naive()) else {
            return;
        };
        let intent = {
            let target = &self.sites.list[site];
            let listing = self
                .acquisition
                .coordinator
                .archive_index
                .get(&target.id.0, &date)
                .map(snapshot_listing);
            let queued_scan_starts = self
                .acquisition
                .coordinator
                .download_queue
                .queued_scan_starts(&target.id);
            let env = acquisition_core::AnchorFastPathEnv {
                auto_fetch_cap_reached: self
                    .acquisition
                    .coordinator
                    .download_queue
                    .auto_fetch_cap_reached(),
                playback_pos: pos,
                date,
                elevation_filter: self.state.viz_state.fetch_scope().elevation_number(),
                listing: listing.as_ref(),
                queued_scan_starts: &queued_scan_starts,
                cache_match_tolerance_secs: crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
                ledger: &target.ledger,
                now_ms: js_sys::Date::now(),
            };
            acquisition_core::decide_anchor_fast_path(&env, &target.scans)
        };
        if let Some(intent) = intent {
            self.enqueue_intents(site, vec![intent], QueueOrigin::Reactive);
        }
    }

    /// Assemble site `site`'s per-date listing/backoff/pending inputs for a
    /// window plan and reduce it to concrete actions. Read-only input
    /// assembly; the caller executes the returned actions.
    fn reduce_window_plan(&self, site: usize, plan: &WindowPlan) -> WindowIntentActions {
        let target = &self.sites.list[site];
        let site_id = &target.id.0;
        let now_ms = js_sys::Date::now();
        let days: Vec<WindowDayInput<'_>> = plan
            .dates
            .iter()
            .map(|&date| WindowDayInput {
                date,
                listing: self
                    .acquisition
                    .coordinator
                    .archive_index
                    .get(site_id, &date)
                    .map(snapshot_listing),
                backoff_until_ms: self
                    .acquisition
                    .listing_backoff
                    .get(&(site_id.clone(), date))
                    .copied(),
                listing_request_pending: self
                    .acquisition
                    .coordinator
                    .download_channel
                    .is_listing_pending(site_id, &date),
            })
            .collect();
        let queued_scan_starts = self
            .acquisition
            .coordinator
            .download_queue
            .queued_scan_starts(&target.id);
        acquisition_core::reduce_window_intents(
            plan,
            &days,
            now_ms,
            &queued_scan_starts,
            &target.scans,
            crate::SCAN_CACHE_MATCH_TOLERANCE_SECS,
            &target.ledger,
        )
    }

    /// Execute a window reduction's described actions in field order: fetch
    /// the missing listings (they populate the index for a later frame), then
    /// enqueue the wanted scans under `origin`. Returns whether a needed
    /// listing is still missing, so the caller can decide completion
    /// (resolve/disarm).
    fn execute_window_actions(
        &mut self,
        ctx: &egui::Context,
        site: usize,
        actions: WindowIntentActions,
        origin: QueueOrigin,
    ) -> bool {
        let site_id = self.sites.list[site].id.0.clone();
        for date in actions.fetch_listings {
            self.acquisition.coordinator.download_channel.fetch_listing(
                ctx.clone(),
                site_id.clone(),
                date,
            );
        }
        self.enqueue_intents(site, actions.enqueue, origin);
        actions.listing_pending
    }

    /// Create a tracked acquisition operation per intent and append the
    /// corresponding items to the shared download queue. The operation id
    /// rides on each queue item so priority dispatch and pruning stay
    /// correlated with the drawer. The next frame's pump_download_queue
    /// dispatches them.
    fn enqueue_intents(&mut self, site: usize, intents: Vec<ScanFetchIntent>, origin: QueueOrigin) {
        if intents.is_empty() {
            return;
        }
        let target = &mut self.sites.list[site];
        let site_id = target.id.0.clone();
        let id = target.id.clone();
        let now_ms = js_sys::Date::now();
        let items: Vec<QueueItem> = intents
            .into_iter()
            .map(|i| {
                target
                    .ledger
                    .note_enqueued(i.scan_start, i.elevation_filter, now_ms);
                let op_id = self.acquisition.state.create_operation(
                    state::OperationKind::ArchiveDownload {
                        site_id: site_id.clone(),
                        file_name: i.file_name.clone(),
                        scan_start: i.scan_start,
                        scan_end: i.scan_end,
                    },
                );
                QueueItem::new(
                    id.clone(),
                    i.date,
                    i.file_name,
                    i.scan_start,
                    i.scan_end,
                    i.elevation_filter,
                )
                .with_operation(op_id)
                .with_origin(origin)
            })
            .collect();
        self.acquisition.coordinator.download_queue.enqueue(items);
    }

    /// Make the timeline the browsing surface: fetch archive listings for
    /// every UTC date the visible window touches, so shadow boundaries
    /// populate wherever the user pans/zooms — no date picker required.
    ///
    /// Bounded by: a max visible span (no listing storms at year zoom), one
    /// new LIST per [`crate::core::acquisition::VISIBLE_LISTING_INTERVAL_MS`],
    /// per-date session caching with a freshness TTL for today (so the shadow
    /// track grows near the live edge), per-(site,date) failure backoff, and
    /// the channel's own pending-listing dedup — all decided in
    /// [`crate::core::acquisition::decide_visible_listing`].
    pub(crate) fn pump_visible_listings(&mut self, ctx: &egui::Context) {
        let span = self.playback.state.view_width_secs();
        let now_ms = js_sys::Date::now();
        if !acquisition_core::visible_listing_pump_due(
            span,
            now_ms,
            self.acquisition.visible_listing_next_ms,
        ) {
            return;
        }

        // Wait for the view to stop moving. The rate limit above only bounds
        // requests per second, it never stops them — so a continuous pan issued
        // one LIST every interval for every date it swept past, none of which
        // the user was looking at by the time it landed. Keyed on the visible
        // day range and the sites on the map, so panning within a day (which
        // needs no new listings) does not reset the timer.
        let view_start = self.playback.state.timeline_view_start;
        let ids: Vec<String> = self.sites.iter().map(|s| s.id.0.clone()).collect();
        let signature = acquisition_core::visible_listing_signature(
            view_start,
            view_start + span,
            &ids.join(","),
        );
        if !self.acquisition.visible_listing_settle.poll(
            signature,
            now_ms,
            crate::PREFETCH_DEBOUNCE_MS,
        ) {
            return;
        }

        let now_secs = self.state.frame_now.secs();
        let today = chrono::DateTime::from_timestamp(now_secs as i64, 0)
            .map(|dt| dt.date_naive())
            .unwrap_or_else(|| chrono::Utc::now().date_naive());
        let dates = acquisition_core::dates_in_range(view_start as i64, (view_start + span) as i64);

        // Every site lists its own days; each may issue one LIST per
        // interval.
        for site_id in ids {
            let days: Vec<acquisition_core::VisibleListingDay> = dates
                .iter()
                .map(|&date| acquisition_core::VisibleListingDay {
                    date,
                    fresh: self
                        .acquisition
                        .coordinator
                        .archive_index
                        .has_fresh(&site_id, &date, now_secs, today),
                    listing_request_pending: self
                        .acquisition
                        .coordinator
                        .download_channel
                        .is_listing_pending(&site_id, &date),
                    backoff_until_ms: self
                        .acquisition
                        .listing_backoff
                        .get(&(site_id.clone(), date))
                        .copied(),
                })
                .collect();

            let actions = acquisition_core::decide_visible_listing(&days, today, now_ms);
            if let Some(date) = actions.fetch {
                self.acquisition.coordinator.download_channel.fetch_listing(
                    ctx.clone(),
                    site_id,
                    date,
                );
            }
            if let Some(next_allowed_ms) = actions.next_allowed_ms {
                // One new LIST per interval — the rest of the span fills in on
                // subsequent frames.
                self.acquisition.visible_listing_next_ms = self
                    .acquisition
                    .visible_listing_next_ms
                    .max(next_allowed_ms);
            }
        }
    }

    /// Apply the duration gate to a range the user just finalized (consumed
    /// from `state.selection_just_finalized`): short spans arm the bulk-fetch
    /// pump immediately; long spans open the confirm modal instead, which arms
    /// the same target on "Download Anyway". Runs once per frame before
    /// `pump_selection_fetch`.
    pub(crate) fn resolve_selection_fetch_gate(&mut self) {
        let Some((start, end)) = self.state.selection_just_finalized.take() else {
            return;
        };
        let now_secs = self.state.frame_now.secs();
        // Six hours on the desktop, two on the phone.
        let threshold = acquisition_core::bulk_confirm_secs(self.chrome.device_layout);
        match acquisition_core::decide_selection_gate(start, end, threshold) {
            acquisition_core::SelectionGate::Arm => {
                self.acquisition.selection_fetch_target =
                    Some(crate::subsystem::acquisition::SelectionFetchTarget {
                        range: (start, end),
                        armed_at_secs: now_secs,
                    });
            }
            acquisition_core::SelectionGate::Confirm => {
                // Priced now, from the listings, so the confirmation says the
                // real size rather than an estimate. The range is fetched for
                // every site on the map, so it prices the sum.
                let live = self.live.mode_state.is_active();
                let (lo, hi) = (start.min(end), start.max(end));
                let costs: Vec<_> = self
                    .sites
                    .iter()
                    .map(|s| {
                        let sizes = self.timeline.activity.scans(&s.id, lo, hi);
                        s.timeline_view(live).range_cost_sized(lo, hi, &sizes)
                    })
                    .collect();
                self.chrome.range_download_modal = Some(
                    acquisition_core::RangeDownloadPrompt::priced((start, end), &costs),
                );
            }
        }
    }

    /// Bulk-fetch the scans in an explicitly selected range (the "selection =
    /// the fetch" contract). Unlike the reactive prefetch, this fetches the
    /// *entire* selected span, not a lookahead window around the cursor.
    ///
    /// Armed via `selection_fetch_target` (by `resolve_selection_fetch_gate` for
    /// short spans, or the confirm modal for long ones). Runs each frame while
    /// armed; the bounded disarm conditions (volume cap, degenerate span, the
    /// listing deadline, and the everything-enqueued normal path) are decided
    /// by [`crate::core::acquisition::plan_selection_fetch`] so it cannot loop
    /// forever. Dedup is the same two-layer guard the reactive pump relies on
    /// (the reducers' already-satisfied check + `download_queue.enqueue`'s own
    /// skip), so re-running while armed never queues a scan twice.
    pub(crate) fn pump_selection_fetch(&mut self, ctx: &egui::Context) {
        let now_secs = self.state.frame_now.secs();
        let today = chrono::DateTime::from_timestamp(now_secs as i64, 0)
            .map(|dt| dt.date_naive())
            .unwrap_or_else(|| chrono::Utc::now().date_naive());
        let env = acquisition_core::SelectionFetchEnv {
            has_worker: self.render.coordinator.has_worker(),
            queue_paused: self.acquisition.state.is_paused(),
            target: self
                .acquisition
                .selection_fetch_target
                .map(|t| (t.range, t.armed_at_secs)),
            auto_fetch_cap_reached: self
                .acquisition
                .coordinator
                .download_queue
                .auto_fetch_cap_reached(),
            now_secs,
            deadline_secs: crate::SELECTION_FETCH_DEADLINE_SECS,
            elevation_filter: self.state.viz_state.fetch_scope().elevation_number(),
            today,
        };
        match acquisition_core::plan_selection_fetch(&env) {
            acquisition_core::SelectionFetchPlan::Skip => {}
            acquisition_core::SelectionFetchPlan::Disarm { status_message } => {
                self.acquisition.selection_fetch_target = None;
                if let Some(message) = status_message {
                    self.state.toasts.notice(message);
                }
            }
            acquisition_core::SelectionFetchPlan::Window(plan) => {
                // A bulk request is explicit, so all missing dates' listings
                // are fetched at once — the per-(site,date) backoff + channel
                // pending-dedup (decided in the reducer) still prevent storms.
                let mut listing_pending = false;
                for site in 0..self.sites.list.len() {
                    let actions = self.reduce_window_plan(site, &plan);
                    listing_pending |=
                        self.execute_window_actions(ctx, site, actions, QueueOrigin::Selection);
                }

                // Disarm: every fetchable date is present and enqueued
                // (normal path).
                if !listing_pending {
                    self.acquisition.selection_fetch_target = None;
                }
            }
        }
    }

    /// Backward backfill for the live lookback replay: while replaying, ensure
    /// the volumes the active pinned loop covers (matching elevation, sized from
    /// the live range's basis) ending at "now" are fetched from the archive, so
    /// the loop has frames. Lazy — it
    /// runs only on a live range — and idempotent via the same dedup as
    /// the forward pump. The live stream itself only fetches the in-progress
    /// volume, so previous volumes must come from the archive.
    pub(crate) fn pump_lookback_backfill(&mut self, ctx: &egui::Context) {
        if !acquisition_core::lookback_backfill_due(
            self.playback.state.is_live_range(),
            self.render.coordinator.has_worker(),
            self.acquisition.state.is_paused(),
            self.acquisition
                .coordinator
                .download_queue
                .auto_fetch_cap_reached(),
            js_sys::Date::now(),
            &mut self.acquisition.lookback_backfill_next_ms,
        ) {
            return;
        }

        // Backfill exactly the window the range covers. The window is
        // resolved from every frame the timeline knows about (listed or
        // cached), so it doesn't depend on what has downloaded so far.
        let now = crate::core::TimeModel::wall_clock_time();
        let basis = self
            .playback
            .state
            .selection
            .range()
            .map(|r| r.basis)
            .unwrap_or_default();
        let (window_start, _) = self.resolve_pinned_window(basis, now);
        let plan = acquisition_core::plan_lookback_backfill(
            window_start,
            now,
            self.state.viz_state.fetch_scope().elevation_number(),
        );
        for site in 0..self.sites.list.len() {
            let actions = self.reduce_window_plan(site, &plan);
            self.execute_window_actions(ctx, site, actions, QueueOrigin::Reactive);
        }
    }
}
