//! Reactive (implicit) prefetch — acquisition as a side effect of navigation.
//!
//! PRODUCT.md §5 specifies that downloading is a *function of what the user
//! looks at*, not a manual chore: the user navigates and acquisition reacts.
//! This module computes, each frame, the set of archive scans that the settled
//! playback position (plus a bounded lookahead, scoped by the active filter)
//! ought to have cached, and enqueues them into the shared download queue.
//!
//! Guardrails against runaway fetching (PRODUCT.md §5.1):
//! - **Debounce** — a prefetch only fires once the view has been stable for
//!   [`crate::PREFETCH_DEBOUNCE_MS`], so transient scrub/zoom positions don't
//!   trigger downloads. During playback the debounce collapses to zero and the
//!   lookahead window scales with speed, so prefetch tracks the cursor.
//! - **Concurrency cap** — inherited from the shared `DownloadQueueManager`.
//! - **Volume cap** — [`crate::nexrad::download_queue::DEFAULT_MAX_AUTO_FETCH_BYTES`]
//!   bounds total bytes fetched per session.
//!
//! Dedup is layered: this module skips scans already cached (per active
//! elevation) or already queued, and the download path itself makes the
//! authoritative per-(elevation) cache-hit decision before any network call.

use crate::nexrad::download_queue::QueueItem;
use crate::{state, WorkbenchApp};
use chrono::NaiveDate;
use eframe::egui;
use std::hash::{Hash, Hasher};

/// One archive scan the reactive pump wants cached, scoped to the active
/// elevation filter (`None` = whole volume, for Latest mode).
struct ScanFetchIntent {
    date: NaiveDate,
    file_name: String,
    scan_start: i64,
    scan_end: i64,
    elevation_filter: Option<u8>,
}

/// Quantum (timeline seconds) for bucketing the playback position in the
/// debounce signature. Small movements within a bucket don't reset the settle;
/// during playback the bucket advances and re-triggers prefetch at a bounded
/// rate regardless of speed.
const PREFETCH_POS_QUANTUM_SECS: f64 = 30.0;

impl WorkbenchApp {
    /// Reactive archive prefetch. Runs every frame (step 9.5) but is gated by a
    /// settle debounce and an idempotency marker, so it does real work only
    /// when the view has settled on something not yet cached/queued.
    pub(crate) fn pump_implicit_prefetch(&mut self, ctx: &egui::Context) {
        // Preconditions: a worker to ingest into; an archive-positioned
        // playhead (while *attached* to the live edge the stream owns its
        // own acquisition and prefetching "ahead of now" is meaningless — a
        // detached background stream browses the archive and prefetches
        // normally); the queue not paused.
        let playhead_attached = self.playback.state.time_model.is_pinned()
            || self.playback.state.time_model.is_lookback();
        if !self.render.coordinator.has_worker()
            || playhead_attached
            || self.acquisition.state.is_paused()
        {
            return;
        }

        // Fast path, ahead of the settle debounce: the scan under the
        // playhead right now. A click into a shadow region starts its fetch
        // on this very frame instead of after the 300 ms settle.
        self.pump_anchor_fast_path();

        let playing_micro = self.playback.state.playing
            && self.playback.state.playback_mode() == state::PlaybackMode::Micro;

        // Debounce: require the view to settle, unless playing (then track the
        // advancing cursor continuously — dedup keeps that idempotent).
        let signature = self.prefetch_signature();
        let now_ms = js_sys::Date::now();
        let settle_ms = if playing_micro {
            0.0
        } else {
            crate::PREFETCH_DEBOUNCE_MS
        };
        if !self
            .acquisition
            .prefetch_settle
            .poll(signature, now_ms, settle_ms)
            || self.acquisition.prefetch_settle.already_resolved()
        {
            return;
        }

        // Stop adding background work once the session volume cap is hit. Mark
        // resolved so we don't recompute until the view moves; surface it so an
        // idle canvas isn't mistaken for breakage (PRODUCT.md §7.2).
        if self
            .acquisition
            .coordinator
            .download_queue
            .auto_fetch_cap_reached()
        {
            self.acquisition.prefetch_settle.mark_resolved();
            self.state.status_message =
                "Auto-fetch limit reached — pausing background prefetch".to_string();
            return;
        }

        // Compute what should be cached for the settled view.
        let (intents, listing_pending) = self.compute_acquisition_intent(ctx);
        if intents.is_empty() {
            // Nothing to do. If no listing is still in flight, this view is
            // fully handled — stop recomputing until it moves.
            if !listing_pending {
                self.acquisition.prefetch_settle.mark_resolved();
            }
            return;
        }

        // Create an acquisition operation per new scan (so prefetch surfaces in
        // the acquisition drawer and completion is tracked), then append to the
        // shared queue. The next frame's pump_download_queue dispatches them.
        self.enqueue_intents(intents);

        // Listings may still be in flight for adjacent dates; only mark the
        // view resolved once everything is enqueued and nothing is pending.
        if !listing_pending {
            self.acquisition.prefetch_settle.mark_resolved();
        }
    }

    /// The debounce-free remedy for "scrub into a shadow = blank canvas": if
    /// the archive scan the playhead would render (at-or-before semantics,
    /// matching the render side) is listed but neither cached for the active
    /// scope nor queued, enqueue it immediately. Idempotent and cheap — the
    /// satisfied check hits on every subsequent frame — so it runs every
    /// frame ahead of the settle gate. Listings themselves are left to the
    /// debounced pump and the visible-range pump.
    fn pump_anchor_fast_path(&mut self) {
        if self
            .acquisition
            .coordinator
            .download_queue
            .auto_fetch_cap_reached()
        {
            return;
        }
        let pos = self.playback.state.playback_position() as i64;
        let Some(date) = chrono::DateTime::from_timestamp(pos, 0).map(|dt| dt.date_naive()) else {
            return;
        };
        let elevation_filter = match &self.state.viz_state.elevation_selection {
            state::ElevationSelection::Fixed {
                elevation_number, ..
            } => Some(*elevation_number),
            state::ElevationSelection::Latest => None,
        };
        let site_id = self.state.viz_state.site_id.clone();
        let intent = {
            let Some(listing) = self
                .acquisition
                .coordinator
                .archive_index
                .get(&site_id, &date)
            else {
                return;
            };
            let Some((file, b)) = listing.scan_at_or_before(pos) else {
                return;
            };
            ScanFetchIntent {
                date,
                file_name: file.name.clone(),
                scan_start: b.start,
                scan_end: b.end,
                elevation_filter,
            }
        };
        if self.prefetch_already_satisfied(&intent) {
            return;
        }
        self.enqueue_intents(vec![intent]);
    }

    /// Create a tracked acquisition operation per intent and append the
    /// corresponding items to the shared download queue. The operation id
    /// rides on each queue item so priority dispatch and pruning stay
    /// correlated with the drawer.
    fn enqueue_intents(&mut self, intents: Vec<ScanFetchIntent>) {
        if intents.is_empty() {
            return;
        }
        let site_id = self.state.viz_state.site_id.clone();
        let items: Vec<QueueItem> = intents
            .into_iter()
            .map(|i| {
                let op_id = self.acquisition.state.create_operation(
                    state::OperationKind::ArchiveDownload {
                        site_id: site_id.clone(),
                        file_name: i.file_name.clone(),
                        scan_start: i.scan_start,
                        scan_end: i.scan_end,
                    },
                );
                QueueItem::new(
                    i.date,
                    i.file_name,
                    i.scan_start,
                    i.scan_end,
                    i.elevation_filter,
                )
                .with_operation(op_id)
            })
            .collect();
        self.acquisition.coordinator.download_queue.enqueue(items);
    }

    /// Hash of the inputs that determine *what* to prefetch: a quantized
    /// playback position, the elevation filter, the product, and the site. A
    /// change resets the settle timer; a stable value lets it fire.
    fn prefetch_signature(&self) -> u64 {
        let pos = self.playback.state.playback_position();
        let bucket = (pos / PREFETCH_POS_QUANTUM_SECS).floor() as i64;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bucket.hash(&mut h);
        match &self.state.viz_state.elevation_selection {
            state::ElevationSelection::Fixed {
                elevation_number, ..
            } => (1u8, *elevation_number).hash(&mut h),
            state::ElevationSelection::Latest => (0u8, 0u8).hash(&mut h),
        }
        self.state.viz_state.product.to_worker_string().hash(&mut h);
        self.state.viz_state.site_id.hash(&mut h);
        h.finish()
    }

    /// Determine which archive scans should be cached for the settled view
    /// (the *forward* lookahead window around the playback cursor).
    ///
    /// Returns `(intents, listing_pending)`, where `listing_pending` is true
    /// when a needed date's listing had to be fetched first — the caller keeps
    /// re-evaluating until it arrives.
    fn compute_acquisition_intent(&self, ctx: &egui::Context) -> (Vec<ScanFetchIntent>, bool) {
        let pos = self.playback.state.playback_position();

        // Elevation scope: a Fixed cut scopes ingest to that elevation; Latest
        // may render any cut as the cursor advances, so fetch the whole volume.
        let elevation_filter = match &self.state.viz_state.elevation_selection {
            state::ElevationSelection::Fixed {
                elevation_number, ..
            } => Some(*elevation_number),
            state::ElevationSelection::Latest => None,
        };

        // Prefetch window: direction-aware, speed-scaled while playing, with
        // a short trail behind the cursor so small backward jogs stay warm.
        // Shared with the queue's prune/priority logic so all three agree on
        // what "near the playhead" means; the volume cap is the backstop.
        let speed_mult = self.playback.state.speed.timeline_seconds_per_real_second();
        let forward = self.playback.state.time_model.direction == state::PlaybackDirection::Forward;
        let (win_start_i64, win_end_i64) = crate::nexrad::download_queue::prefetch_window(
            pos,
            speed_mult,
            self.playback.state.playing,
            forward,
        );

        let pos_i64 = pos as i64;
        // Look slightly back so the listing for the prior date is fetched near a
        // UTC midnight boundary; the render target is the `scan_at_or_before`
        // anchor below.
        let dates_start_i64 =
            win_start_i64.min((pos - crate::FALLBACK_SCAN_DURATION_SECS as f64) as i64);

        self.compute_intents_for_window(
            dates_start_i64,
            win_start_i64,
            win_end_i64,
            Some(pos_i64),
            elevation_filter,
            ctx,
        )
    }

    /// Shared core: which archive scans intersect a window should be cached.
    ///
    /// `dates_start_i64..win_end_i64` bounds the *dates* whose listings we
    /// consult; `intersect_start_i64..win_end_i64` bounds which scans within
    /// those listings we want; `anchor_at_or_before` optionally adds the scan
    /// covering that instant (the forward render target). Fetches any missing
    /// listings, dedups, and drops already-cached/queued scans.
    fn compute_intents_for_window(
        &self,
        dates_start_i64: i64,
        intersect_start_i64: i64,
        win_end_i64: i64,
        anchor_at_or_before: Option<i64>,
        elevation_filter: Option<u8>,
        ctx: &egui::Context,
    ) -> (Vec<ScanFetchIntent>, bool) {
        let site_id = self.state.viz_state.site_id.clone();
        let mut intents: Vec<ScanFetchIntent> = Vec::new();
        let mut missing_dates: Vec<NaiveDate> = Vec::new();

        for date in dates_spanning(dates_start_i64, win_end_i64) {
            match self
                .acquisition
                .coordinator
                .archive_index
                .get(&site_id, &date)
            {
                Some(listing) => {
                    let mut found: Vec<(String, i64, i64)> = listing
                        .scans_intersecting(intersect_start_i64, win_end_i64)
                        .into_iter()
                        .map(|(file, b)| (file.name.clone(), b.start, b.end))
                        .collect();
                    if let Some(anchor) = anchor_at_or_before {
                        if let Some((file, b)) = listing.scan_at_or_before(anchor) {
                            found.push((file.name.clone(), b.start, b.end));
                        }
                    }
                    for (file_name, scan_start, scan_end) in found {
                        intents.push(ScanFetchIntent {
                            date,
                            file_name,
                            scan_start,
                            scan_end,
                            elevation_filter,
                        });
                    }
                }
                None => missing_dates.push(date),
            }
        }

        // Fetch any missing listings (the immutable archive_index borrow above
        // is released here). They populate the index for a later frame.
        let listing_pending = !missing_dates.is_empty();
        let now_ms = js_sys::Date::now();
        for date in missing_dates {
            let backed_off = self
                .acquisition
                .listing_backoff
                .get(&(site_id.clone(), date))
                .is_some_and(|&until| now_ms < until);
            if !backed_off
                && !self
                    .acquisition
                    .coordinator
                    .download_channel
                    .is_listing_pending(&site_id, &date)
            {
                self.acquisition.coordinator.download_channel.fetch_listing(
                    ctx.clone(),
                    site_id.clone(),
                    date,
                );
            }
        }

        // Collapse duplicate scans, then drop those already satisfied (cached
        // for this elevation, or already queued).
        intents.sort_by_key(|i| i.scan_start);
        intents.dedup_by_key(|i| i.scan_start);
        intents.retain(|i| !self.prefetch_already_satisfied(i));

        (intents, listing_pending)
    }

    /// Maximum visible span (seconds) for which the visible-range listing
    /// pump will fetch archive listings. Zoomed out past this (weeks/months),
    /// listing every visible day would be an S3 request storm for shadows
    /// too small to read anyway.
    const VISIBLE_LISTING_MAX_SPAN_SECS: f64 = 4.0 * 86_400.0;

    /// Rate limit between new listing requests issued by the visible pump.
    const VISIBLE_LISTING_INTERVAL_MS: f64 = 400.0;

    /// Make the timeline the browsing surface: fetch archive listings for
    /// every UTC date the visible window touches, so shadow boundaries
    /// populate wherever the user pans/zooms — no date picker required.
    ///
    /// Bounded by: a max visible span (no listing storms at year zoom), one
    /// new LIST per [`Self::VISIBLE_LISTING_INTERVAL_MS`], per-date session
    /// caching with a freshness TTL for today (so the shadow track grows
    /// near the live edge), per-(site,date) failure backoff, and the
    /// channel's own pending-listing dedup.
    pub(crate) fn pump_visible_listings(&mut self, ctx: &egui::Context) {
        let span = self.playback.state.view_width_secs();
        if span <= 0.0 || span > Self::VISIBLE_LISTING_MAX_SPAN_SECS {
            return;
        }
        let now_ms = js_sys::Date::now();
        if now_ms < self.acquisition.visible_listing_next_ms {
            return;
        }

        let now_secs = self.state.frame_now.secs();
        let today = chrono::DateTime::from_timestamp(now_secs as i64, 0)
            .map(|dt| dt.date_naive())
            .unwrap_or_else(|| chrono::Utc::now().date_naive());
        let view_start = self.playback.state.timeline_view_start;
        let site_id = self.state.viz_state.site_id.clone();

        for date in dates_in_range(view_start as i64, (view_start + span) as i64) {
            if date > today {
                continue;
            }
            if self
                .acquisition
                .coordinator
                .archive_index
                .has_fresh(&site_id, &date, now_secs, today)
            {
                continue;
            }
            if self
                .acquisition
                .coordinator
                .download_channel
                .is_listing_pending(&site_id, &date)
            {
                continue;
            }
            if self
                .acquisition
                .listing_backoff
                .get(&(site_id.clone(), date))
                .is_some_and(|&until| now_ms < until)
            {
                continue;
            }
            self.acquisition.coordinator.download_channel.fetch_listing(
                ctx.clone(),
                site_id.clone(),
                date,
            );
            // One new LIST per interval — the rest of the span fills in on
            // subsequent frames.
            self.acquisition.visible_listing_next_ms = now_ms + Self::VISIBLE_LISTING_INTERVAL_MS;
            return;
        }
    }

    /// Backward backfill for the live lookback replay: while replaying, ensure
    /// the last ~[`crate::LOOKBACK_FRAMES`] volumes (matching elevation) ending
    /// at "now" are fetched from the archive, so the loop has frames. Lazy — it
    /// runs only in `LookbackLoop` mode — and idempotent via the same dedup as
    /// the forward pump. The live stream itself only fetches the in-progress
    /// volume, so previous volumes must come from the archive.
    pub(crate) fn pump_lookback_backfill(&mut self, ctx: &egui::Context) {
        if !self.playback.state.time_model.is_lookback()
            || !self.render.coordinator.has_worker()
            || self.acquisition.state.is_paused()
            || self
                .acquisition
                .coordinator
                .download_queue
                .auto_fetch_cap_reached()
        {
            return;
        }

        // Light 1 Hz throttle — the enqueue is idempotent, this just avoids
        // recomputing the window every frame for the whole replay.
        let now_ms = js_sys::Date::now();
        if now_ms < self.acquisition.lookback_backfill_next_ms {
            return;
        }
        self.acquisition.lookback_backfill_next_ms = now_ms + 1000.0;

        let now = crate::state::TimeModel::wall_clock_time();
        let win_end_i64 = now as i64;
        let win_start_i64 = (now - crate::LOOKBACK_SPAN_SECS) as i64;
        let elevation_filter = match &self.state.viz_state.elevation_selection {
            state::ElevationSelection::Fixed {
                elevation_number, ..
            } => Some(*elevation_number),
            state::ElevationSelection::Latest => None,
        };

        let (intents, _listing_pending) = self.compute_intents_for_window(
            win_start_i64,
            win_start_i64,
            win_end_i64,
            None,
            elevation_filter,
            ctx,
        );
        self.enqueue_intents(intents);
    }

    /// Whether a candidate scan is already cached for the active scope or is
    /// already in the download queue — a synchronous, in-memory check. The
    /// download path makes the authoritative IDB-backed decision as a backstop.
    fn prefetch_already_satisfied(&self, intent: &ScanFetchIntent) -> bool {
        if self
            .acquisition
            .coordinator
            .download_queue
            .find_by_scan_start(intent.scan_start)
            .is_some()
        {
            return true;
        }
        self.timeline.scans.scans.iter().any(|s| {
            (s.start_time as i64 - intent.scan_start).abs() < crate::SCAN_CACHE_MATCH_TOLERANCE_SECS
                && match intent.elevation_filter {
                    // Fixed cut: satisfied once that elevation is stored.
                    Some(elev) => s.sweeps.iter().any(|sw| sw.elevation_number == elev),
                    // Whole volume (Latest): treat any cached sweep as enough —
                    // Latest renders from whatever's present, and completing a
                    // partial volume is left to the on-demand download path.
                    None => !s.sweeps.is_empty(),
                }
        })
    }
}

/// The distinct UTC dates a `[start, end]` second-range touches (one, or two
/// across a midnight boundary — the prefetch window is always well under 24h).
fn dates_spanning(start_secs: i64, end_secs: i64) -> Vec<NaiveDate> {
    let mut dates = Vec::new();
    for ts in [start_secs, end_secs] {
        if let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) {
            let date = dt.date_naive();
            if !dates.contains(&date) {
                dates.push(date);
            }
        }
    }
    dates
}

/// Every UTC date a `[start, end]` second-range touches, in order. Unlike
/// [`dates_spanning`] (which only samples the endpoints), this walks day by
/// day so multi-day visible windows enumerate their interior dates too.
fn dates_in_range(start_secs: i64, end_secs: i64) -> Vec<NaiveDate> {
    let (Some(start_dt), Some(end_dt)) = (
        chrono::DateTime::from_timestamp(start_secs, 0),
        chrono::DateTime::from_timestamp(end_secs.max(start_secs), 0),
    ) else {
        return Vec::new();
    };
    let mut dates = Vec::new();
    let mut date = start_dt.date_naive();
    let last = end_dt.date_naive();
    while date <= last {
        dates.push(date);
        let Some(next) = date.succ_opt() else { break };
        date = next;
    }
    dates
}
