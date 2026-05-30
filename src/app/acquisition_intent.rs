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
        // Preconditions: a worker to ingest into; archive mode (live streaming
        // owns its own acquisition, and prefetching the archive "ahead of now"
        // is meaningless); the queue not paused.
        if !self.render.coordinator.has_worker()
            || self.live.mode_state.is_active()
            || self.acquisition.state.is_paused()
        {
            return;
        }

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
        let site_id = self.state.viz_state.site_id.clone();
        let items: Vec<QueueItem> = intents
            .into_iter()
            .map(|i| {
                self.acquisition
                    .state
                    .create_operation(state::OperationKind::ArchiveDownload {
                        site_id: site_id.clone(),
                        file_name: i.file_name.clone(),
                        scan_start: i.scan_start,
                        scan_end: i.scan_end,
                    });
                QueueItem::new(
                    i.date,
                    i.file_name,
                    i.scan_start,
                    i.scan_end,
                    i.elevation_filter,
                )
            })
            .collect();
        self.acquisition.coordinator.download_queue.enqueue(items);

        // Listings may still be in flight for adjacent dates; only mark the
        // view resolved once everything is enqueued and nothing is pending.
        if !listing_pending {
            self.acquisition.prefetch_settle.mark_resolved();
        }
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

    /// Determine which archive scans should be cached for the settled view.
    ///
    /// Returns `(intents, listing_pending)`, where `listing_pending` is true
    /// when a needed date's listing had to be fetched first — the caller keeps
    /// re-evaluating until it arrives.
    fn compute_acquisition_intent(&self, ctx: &egui::Context) -> (Vec<ScanFetchIntent>, bool) {
        let site_id = self.state.viz_state.site_id.clone();
        let pos = self.playback.state.playback_position();

        // Elevation scope: a Fixed cut scopes ingest to that elevation; Latest
        // may render any cut as the cursor advances, so fetch the whole volume.
        let elevation_filter = match &self.state.viz_state.elevation_selection {
            state::ElevationSelection::Fixed {
                elevation_number, ..
            } => Some(*elevation_number),
            state::ElevationSelection::Latest => None,
        };

        // Lookahead window. When playing, scale with speed so fast playback
        // buffers proportionally further ahead; the volume cap is the backstop.
        let speed_mult = self.playback.state.speed.timeline_seconds_per_real_second();
        let lookahead = if self.playback.state.playing {
            crate::PREFETCH_LOOKAHEAD_SECS_PAUSED.max(speed_mult * crate::PREFETCH_PLAY_LEAD_SECS)
        } else {
            crate::PREFETCH_LOOKAHEAD_SECS_PAUSED
        };

        let pos_i64 = pos as i64;
        let win_end_i64 = (pos + lookahead) as i64;
        // Look slightly back so the render target is included when the cursor
        // sits in dead-time just past a scan (and to catch the prior date near
        // a UTC midnight boundary).
        let win_start_i64 = (pos - crate::FALLBACK_SCAN_DURATION_SECS as f64) as i64;

        let mut intents: Vec<ScanFetchIntent> = Vec::new();
        let mut missing_dates: Vec<NaiveDate> = Vec::new();

        for date in dates_spanning(win_start_i64, win_end_i64) {
            match self
                .acquisition
                .coordinator
                .archive_index
                .get(&site_id, &date)
            {
                Some(listing) => {
                    // Current scan + lookahead scans, then the render target
                    // when the cursor sits in dead-time / a gap (dedup'd below).
                    let mut found: Vec<(String, i64, i64)> = listing
                        .scans_intersecting(pos_i64, win_end_i64)
                        .into_iter()
                        .map(|(file, b)| (file.name.clone(), b.start, b.end))
                        .collect();
                    if let Some((file, b)) = listing.scan_at_or_before(pos_i64) {
                        found.push((file.name.clone(), b.start, b.end));
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
        for date in missing_dates {
            if !self
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

        // Collapse the duplicate render-target/covering scan, then drop scans
        // already satisfied (cached for this elevation, or already queued).
        intents.sort_by_key(|i| i.scan_start);
        intents.dedup_by_key(|i| i.scan_start);
        intents.retain(|i| !self.prefetch_already_satisfied(i));

        (intents, listing_pending)
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
