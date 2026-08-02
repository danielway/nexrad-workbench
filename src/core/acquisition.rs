//! Pure acquisition decisions: prefetch policy, the selection-fetch gate, the
//! UTC-date spans the prefetch/listing pumps enumerate, and the pump reducers
//! themselves (debounced reactive prefetch, the anchor fast-path, visible-range
//! listings, selection bulk-fetch, lookback backfill).
//!
//! The pumps in `app::acquisition_intent` used to interleave these decisions
//! with I/O (listing fetches, queue enqueues). The *deciding* is pure and lives
//! here so the policy gates, window math, and dedup/churn guards are
//! unit-tested without a worker, a queue, or a browser; the shell pumps
//! assemble inputs, call the reducers, and execute the described actions in
//! field order. The download queue's own state machine
//! (`nexrad::download_queue`) is already pure + tested and stays where it is.
//!
//! The window pumps are **two decision points separated by reads**, following
//! the [`crate::core::render_loop`] pattern: a *plan* reducer computes the
//! window and which dates' listings to consult, the shell snapshots those
//! listings, and [`reduce_window_intents`] turns them into concrete
//! fetch/enqueue actions.

use crate::core::{
    ElevationSelection, LoopBasis, PlaybackDirection, PlaybackMode, PlaybackState, RadarTimeline,
    ScanBoundary,
};
use chrono::NaiveDate;
use std::hash::{Hash, Hasher};

/// Whether the playhead-driven reactive prefetch (settled window + anchor
/// fast-path) may run this frame. Suppressed while the playhead is attached to
/// the live edge (the stream owns acquisition there), while the queue is
/// manually paused, when the data-saver `autofetch_while_scrubbing` policy is
/// off, or while a scrub drag is still in progress.
///
/// The `scrub_in_progress` gate is what makes dragging the playhead across the
/// archive cost one fetch instead of one per scan crossed: the settle debounce
/// alone can't do it, because `pump_anchor_fast_path` deliberately runs ahead
/// of the debounce so a *click* into a shadow region fetches immediately. A
/// press-and-release still fetches on the very next frame; only the sustained
/// drag is held back.
pub(crate) fn reactive_prefetch_allowed(
    playhead_attached: bool,
    queue_paused: bool,
    autofetch_while_scrubbing: bool,
    scrub_in_progress: bool,
) -> bool {
    !playhead_attached && !queue_paused && autofetch_while_scrubbing && !scrub_in_progress
}

/// How a just-finalized timeline selection should be fetched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SelectionGate {
    /// Short span — arm the bulk-fetch pump immediately.
    Arm,
    /// Long span — open the confirm modal first (it arms the same target on
    /// "Download Anyway").
    Confirm,
}

/// Decide whether a selected `[start, end]` range fetches immediately or asks
/// for confirmation first. Spans at or under `confirm_threshold` seconds arm
/// directly; longer ones confirm. Mirrors the duration gate in
/// `resolve_selection_fetch_gate`.
pub(crate) fn decide_selection_gate(start: f64, end: f64, confirm_threshold: f64) -> SelectionGate {
    if (end - start).abs() <= confirm_threshold {
        SelectionGate::Arm
    } else {
        SelectionGate::Confirm
    }
}

/// The distinct UTC dates a `[start, end]` second-range touches (one, or two
/// across a midnight boundary — the prefetch window is always well under 24h).
pub(crate) fn dates_spanning(start_secs: i64, end_secs: i64) -> Vec<NaiveDate> {
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

/// Hard cap on how many dates [`dates_in_range`] enumerates.
///
/// The walk is day-by-day, so an unbounded range allocates one `NaiveDate` per
/// day — and every one becomes a `fetch_listings` intent downstream. Now that
/// the timeline can be zoomed out across decades, a stray multi-year range
/// would mean tens of thousands of S3 LIST requests. This is the backstop; the
/// selection span gate ([`MAX_SELECTION_SPAN_SECS`]) is the front door.
pub(crate) const MAX_ENUMERATED_DATES: usize = 400;

/// Widest time range a loop/selection may cover.
///
/// The user explicitly accepts limits at extreme zooms: a range selection
/// spanning years is not a meaningful request, it is a way to accidentally
/// queue a decade of downloads. Well above any real event (the 6-hour
/// [`SELECTION_BULK_CONFIRM_SECS`] modal still guards everything below it) and
/// well below the point where enumeration gets expensive.
pub(crate) const MAX_SELECTION_SPAN_SECS: f64 = 7.0 * 86_400.0;

/// Whether a `[start, end]` range is short enough to be a selection at all.
///
/// Pure so the guard can be applied identically at the two places that need it:
/// the strip's selection gesture (refuse to create it) and the bulk-fetch
/// planner (refuse to act on one that somehow exists).
pub(crate) fn selection_span_allowed(start: f64, end: f64) -> bool {
    let span = (end - start).abs();
    span.is_finite() && span <= MAX_SELECTION_SPAN_SECS
}

/// Every UTC date a `[start, end]` second-range touches, in order. Unlike
/// [`dates_spanning`] (which only samples the endpoints), this walks day by day
/// so multi-day visible windows enumerate their interior dates too. Bounded by
/// [`MAX_ENUMERATED_DATES`].
pub(crate) fn dates_in_range(start_secs: i64, end_secs: i64) -> Vec<NaiveDate> {
    let (Some(start_dt), Some(end_dt)) = (
        chrono::DateTime::from_timestamp(start_secs, 0),
        chrono::DateTime::from_timestamp(end_secs.max(start_secs), 0),
    ) else {
        return Vec::new();
    };
    let mut dates = Vec::new();
    let mut date = start_dt.date_naive();
    let last = end_dt.date_naive();
    while date <= last && dates.len() < MAX_ENUMERATED_DATES {
        dates.push(date);
        let Some(next) = date.succ_opt() else { break };
        date = next;
    }
    dates
}

// ───────────────────────────────────────────────────────────────────────────
// Listing queries (shared with `nexrad::archive_index`, which delegates here)
// ───────────────────────────────────────────────────────────────────────────

/// Indices of `bounds` whose span `[start, end)` intersects
/// `[range_start, range_end]`. Half-open: a scan starting exactly at
/// `range_end`, or ending exactly at `range_start`, does not intersect.
pub(crate) fn intersecting_indices(
    bounds: &[ScanBoundary],
    range_start: i64,
    range_end: i64,
) -> Vec<usize> {
    bounds
        .iter()
        .enumerate()
        .filter(|(_, b)| b.start < range_end && b.end > range_start)
        .map(|(i, _)| i)
        .collect()
}

/// Index of the most recent boundary that starts at or before `timestamp`.
///
/// This is the scan a playback cursor renders even when it sits in the
/// dead-time after a scan's last sweep or in a gap before the next scan —
/// matching `find_recent_scan`'s "most recent started" semantics on the
/// render side.
pub(crate) fn at_or_before_index(bounds: &[ScanBoundary], timestamp: i64) -> Option<usize> {
    bounds
        .iter()
        .enumerate()
        .filter(|(_, b)| b.start <= timestamp)
        .max_by_key(|(_, b)| b.start)
        .map(|(i, _)| i)
}

// ───────────────────────────────────────────────────────────────────────────
// Prefetch window + dedup guard
// ───────────────────────────────────────────────────────────────────────────

/// The `[start, end]` window (Unix seconds) of scans worth having queued for
/// a playhead at `pos`.
///
/// Paused: degenerate — only the scan under the playhead is wanted (the
/// `anchor_at_or_before` mechanism resolves it; backward jogs are served
/// on-frame by the same at-or-before anchor). Playing: a lead in the playback
/// direction of at least one scan, scaled with speed so fast playback buffers
/// proportionally further — without it every scan boundary during playback
/// would wait on a cold S3 fetch. No trailing prefetch in either state.
pub(crate) fn prefetch_window(
    pos: f64,
    speed_secs_per_sec: f64,
    playing: bool,
    forward: bool,
) -> (i64, i64) {
    if !playing {
        return (pos as i64, pos as i64);
    }
    let lead = (crate::FALLBACK_SCAN_DURATION_SECS as f64)
        .max(speed_secs_per_sec * crate::PREFETCH_PLAY_LEAD_SECS);
    if forward {
        (pos as i64, (pos + lead) as i64)
    } else {
        ((pos - lead) as i64, pos as i64)
    }
}

/// One archive scan a pump wants cached, scoped to the active elevation
/// filter (`None` = whole volume, for Latest mode).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScanFetchIntent {
    pub date: NaiveDate,
    pub file_name: String,
    pub scan_start: i64,
    pub scan_end: i64,
    pub elevation_filter: Option<u8>,
}

/// Shell snapshot of one cached day listing: file names and inferred scan
/// boundaries, index-aligned (`file_names[i]` spans `boundaries[i]`).
pub(crate) struct ListingSnapshot<'a> {
    pub file_names: Vec<&'a str>,
    pub boundaries: Vec<ScanBoundary>,
}

/// Whether a candidate scan is already cached for the active scope or is
/// already in the download queue — a synchronous, in-memory check. The
/// download path makes the authoritative IDB-backed decision as a backstop.
///
/// `queued_scan_starts` is the shell's snapshot of every queue item's
/// `scan_start` (any state — a Done item still suppresses re-enqueue, exactly
/// like the queue's own `find_by_scan_start`).
pub(crate) fn prefetch_already_satisfied(
    intent: &ScanFetchIntent,
    queued_scan_starts: &[i64],
    timeline: &RadarTimeline,
    cache_match_tolerance_secs: i64,
) -> bool {
    if queued_scan_starts.contains(&intent.scan_start) {
        return true;
    }
    timeline.scans.iter().any(|s| {
        (s.start_time as i64 - intent.scan_start).abs() < cache_match_tolerance_secs
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

// ───────────────────────────────────────────────────────────────────────────
// Settle debounce + prefetch signature
// ───────────────────────────────────────────────────────────────────────────

/// Debounce + idempotency state for reactive prefetch.
///
/// Prefetch must not fire while the user is actively scrubbing or zooming —
/// the view has to settle first (PRODUCT.md §5.1). This tracks the last
/// "what should we prefetch" signature and when it last changed; the pump
/// only acts once the signature has been stable for the debounce window
/// (which collapses to zero during playback so prefetch tracks the advancing
/// cursor continuously). `resolved_signature` suppresses redundant
/// re-evaluation once a settled view has been fully handled.
#[derive(Default)]
pub(crate) struct PrefetchSettle {
    last_signature: u64,
    settled_since_ms: Option<f64>,
    resolved_signature: Option<u64>,
}

impl PrefetchSettle {
    /// Record this frame's signature and report whether the view has been
    /// settled for at least `settle_ms`. A changed signature resets the timer
    /// and clears the resolved marker.
    pub(crate) fn poll(&mut self, signature: u64, now_ms: f64, settle_ms: f64) -> bool {
        if signature != self.last_signature {
            self.last_signature = signature;
            self.settled_since_ms = Some(now_ms);
            self.resolved_signature = None;
        }
        self.settled_since_ms
            .is_some_and(|since| now_ms - since >= settle_ms)
    }

    /// Whether the current signature has already been fully handled (nothing
    /// left to enqueue, no listing pending), so re-evaluation can be skipped.
    pub(crate) fn already_resolved(&self) -> bool {
        self.resolved_signature == Some(self.last_signature)
    }

    /// Mark the current signature as fully handled.
    pub(crate) fn mark_resolved(&mut self) {
        self.resolved_signature = Some(self.last_signature);
    }
}

/// Quantum (timeline seconds) for bucketing the playback position in the
/// debounce signature. Small movements within a bucket don't reset the settle;
/// during playback the bucket advances and re-triggers prefetch at a bounded
/// rate regardless of speed.
pub(crate) const PREFETCH_POS_QUANTUM_SECS: f64 = 30.0;

/// Hash of the inputs that determine *what* to prefetch: a quantized
/// playback position, the elevation filter, the product, and the site. A
/// change resets the settle timer; a stable value lets it fire.
pub(crate) fn prefetch_signature(
    pos: f64,
    elevation_selection: &ElevationSelection,
    product_worker_string: &str,
    site_id: &str,
) -> u64 {
    let bucket = (pos / PREFETCH_POS_QUANTUM_SECS).floor() as i64;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bucket.hash(&mut h);
    match elevation_selection {
        ElevationSelection::Fixed {
            elevation_number, ..
        } => (1u8, *elevation_number).hash(&mut h),
        ElevationSelection::Latest => (0u8, 0u8).hash(&mut h),
    }
    product_worker_string.hash(&mut h);
    site_id.hash(&mut h);
    h.finish()
}

/// Hash of the inputs that determine *which listings the visible range needs*:
/// the first and last visible UTC day, plus the site.
///
/// Quantized to whole days on purpose. The listing pump works per date, so
/// panning *within* a day needs no new listings and must not reset the settle;
/// panning *across* days does, and should wait for the view to stop before
/// spending requests on dates being scrolled past.
pub(crate) fn visible_listing_signature(view_start: f64, view_end: f64, site_id: &str) -> u64 {
    let first_day = (view_start / 86_400.0).floor() as i64;
    let last_day = (view_end / 86_400.0).floor() as i64;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    first_day.hash(&mut h);
    last_day.hash(&mut h);
    site_id.hash(&mut h);
    h.finish()
}

// ───────────────────────────────────────────────────────────────────────────
// Anchor fast-path
// ───────────────────────────────────────────────────────────────────────────

/// Read-only inputs for the anchor fast-path decision, shell-assembled every
/// frame.
pub(crate) struct AnchorFastPathEnv<'a> {
    /// `download_queue.auto_fetch_cap_reached()`.
    pub auto_fetch_cap_reached: bool,
    /// `playback_position() as i64`.
    pub playback_pos: i64,
    /// The UTC date of `playback_pos`.
    pub date: NaiveDate,
    /// `elevation_selection.elevation_number()`.
    pub elevation_filter: Option<u8>,
    /// The cached listing for `date`, if any.
    pub listing: Option<&'a ListingSnapshot<'a>>,
    /// Every queue item's `scan_start` (any state).
    pub queued_scan_starts: &'a [i64],
    /// `crate::SCAN_CACHE_MATCH_TOLERANCE_SECS`.
    pub cache_match_tolerance_secs: i64,
}

/// The debounce-free remedy for "scrub into a shadow = blank canvas": if
/// the archive scan the playhead would render (at-or-before semantics,
/// matching the render side) is listed but neither cached for the active
/// scope nor queued, fetch it immediately. Idempotent and cheap — the
/// satisfied check hits on every subsequent frame. Listings themselves are
/// left to the debounced pump and the visible-range pump.
pub(crate) fn decide_anchor_fast_path(
    env: &AnchorFastPathEnv<'_>,
    timeline: &RadarTimeline,
) -> Option<ScanFetchIntent> {
    if env.auto_fetch_cap_reached {
        return None;
    }
    let listing = env.listing?;
    let idx = at_or_before_index(&listing.boundaries, env.playback_pos)?;
    let b = listing.boundaries[idx];
    let intent = ScanFetchIntent {
        date: env.date,
        file_name: listing.file_names[idx].to_string(),
        scan_start: b.start,
        scan_end: b.end,
        elevation_filter: env.elevation_filter,
    };
    if prefetch_already_satisfied(
        &intent,
        env.queued_scan_starts,
        timeline,
        env.cache_match_tolerance_secs,
    ) {
        return None;
    }
    Some(intent)
}

// ───────────────────────────────────────────────────────────────────────────
// Window plans (phase 1) + the shared window→intents reducer (phase 2)
// ───────────────────────────────────────────────────────────────────────────

/// Which archive scans a pump wants: the dates whose listings to consult and
/// the window bounds to reduce them with (phase-2 input for
/// [`reduce_window_intents`]).
///
/// `dates` bounds which listings are consulted; `intersect_start..win_end`
/// bounds which scans within those listings are wanted; `anchor_at_or_before`
/// optionally adds the scan covering that instant (the forward render target).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WindowPlan {
    pub dates: Vec<NaiveDate>,
    pub intersect_start: i64,
    pub win_end: i64,
    pub anchor_at_or_before: Option<i64>,
    pub elevation_filter: Option<u8>,
    /// When set, a missing listing for a date *after* this one (a future date
    /// with no archive) is skipped instead of counted as pending/fetchable.
    pub skip_missing_after: Option<NaiveDate>,
}

/// Shell snapshot of one plan date's listing state.
pub(crate) struct WindowDayInput<'a> {
    pub date: NaiveDate,
    /// The cached listing, or `None` when the archive index has none yet.
    pub listing: Option<ListingSnapshot<'a>>,
    /// Per-(site, date) wall-clock ms before which a failed listing may not
    /// be re-requested.
    pub backoff_until_ms: Option<f64>,
    /// Whether a listing request for this date is already in flight.
    pub listing_request_pending: bool,
}

/// Described effects of a window reduction, executed by the shell in this
/// field order.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct WindowIntentActions {
    /// `download_channel.fetch_listing(ctx, site, date)` per date, in order.
    pub fetch_listings: Vec<NaiveDate>,
    /// Create a tracked acquisition operation per intent, then append the
    /// corresponding items to the shared download queue.
    pub enqueue: Vec<ScanFetchIntent>,
    /// A needed date's listing is still missing — the pump keeps re-evaluating
    /// until it arrives (reactive: don't mark the view resolved; selection:
    /// stay armed).
    pub listing_pending: bool,
}

/// Shared core: which archive scans intersecting the plan's window should be
/// cached. Collects the intersecting (plus anchor) scans from present
/// listings, decides which missing listings to fetch (backoff + pending
/// dedup), then collapses duplicates and drops already-satisfied scans.
pub(crate) fn reduce_window_intents(
    plan: &WindowPlan,
    days: &[WindowDayInput<'_>],
    now_ms: f64,
    queued_scan_starts: &[i64],
    timeline: &RadarTimeline,
    cache_match_tolerance_secs: i64,
) -> WindowIntentActions {
    let mut intents: Vec<ScanFetchIntent> = Vec::new();
    let mut missing_days: Vec<&WindowDayInput<'_>> = Vec::new();

    for day in days {
        match &day.listing {
            Some(listing) => {
                let mut found: Vec<(String, i64, i64)> =
                    intersecting_indices(&listing.boundaries, plan.intersect_start, plan.win_end)
                        .into_iter()
                        .map(|i| {
                            let b = listing.boundaries[i];
                            (listing.file_names[i].to_string(), b.start, b.end)
                        })
                        .collect();
                if let Some(anchor) = plan.anchor_at_or_before {
                    if let Some(i) = at_or_before_index(&listing.boundaries, anchor) {
                        let b = listing.boundaries[i];
                        found.push((listing.file_names[i].to_string(), b.start, b.end));
                    }
                }
                for (file_name, scan_start, scan_end) in found {
                    intents.push(ScanFetchIntent {
                        date: day.date,
                        file_name,
                        scan_start,
                        scan_end,
                        elevation_filter: plan.elevation_filter,
                    });
                }
            }
            // Dates in the future have no archive; don't keep the pump
            // waiting for them (selection sets `skip_missing_after`).
            None => match plan.skip_missing_after {
                Some(today) if day.date > today => {}
                _ => missing_days.push(day),
            },
        }
    }

    let listing_pending = !missing_days.is_empty();
    let mut fetch_listings: Vec<NaiveDate> = Vec::new();
    for day in missing_days {
        let backed_off = day.backoff_until_ms.is_some_and(|until| now_ms < until);
        if !backed_off && !day.listing_request_pending {
            fetch_listings.push(day.date);
        }
    }

    // Collapse duplicate scans, then drop those already satisfied (cached
    // for this elevation, or already queued).
    intents.sort_by_key(|i| i.scan_start);
    intents.dedup_by_key(|i| i.scan_start);
    intents.retain(|i| {
        !prefetch_already_satisfied(i, queued_scan_starts, timeline, cache_match_tolerance_secs)
    });

    WindowIntentActions {
        fetch_listings,
        enqueue: intents,
        listing_pending,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Reactive (implicit) prefetch — the debounced settled-view pump
// ───────────────────────────────────────────────────────────────────────────

/// Read-only frame context for the reactive-prefetch plan, shell-assembled.
pub(crate) struct ReactivePrefetchEnv<'a> {
    /// `download_queue.auto_fetch_cap_reached()`.
    pub auto_fetch_cap_reached: bool,
    /// `js_sys::Date::now()`.
    pub now_ms: f64,
    /// `crate::PREFETCH_DEBOUNCE_MS`.
    pub debounce_ms: f64,
    /// `viz_state.site_id`.
    pub site_id: &'a str,
    /// `viz_state.product.to_worker_string()`.
    pub product_worker_string: &'a str,
    /// `crate::FALLBACK_SCAN_DURATION_SECS`.
    pub fallback_scan_duration_secs: i64,
}

/// What the debounced reactive pump should do this frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReactivePrefetchPlan {
    /// Debounce hasn't settled, or this settled view was already handled.
    Skip,
    /// Session volume cap hit: surface the status so an idle canvas isn't
    /// mistaken for breakage (PRODUCT.md §7.2). The settle was marked
    /// resolved so nothing recomputes until the view moves.
    CapReached { status_message: String },
    /// Consult the dates' listings and reduce the window to intents.
    Window(WindowPlan),
}

/// Decide whether the settled view needs prefetching and, if so, plan the
/// forward lookahead window around the playback cursor. Mutates only the
/// settle state.
///
/// The window is direction-aware, speed-scaled while playing, with a short
/// trail behind the cursor so small backward jogs stay warm. Shared with the
/// queue's prune/priority logic (via [`prefetch_window`]) so all three agree
/// on what "near the playhead" means; the volume cap is the backstop.
pub(crate) fn plan_reactive_prefetch(
    env: &ReactivePrefetchEnv<'_>,
    playback: &PlaybackState,
    elevation_selection: &ElevationSelection,
    settle: &mut PrefetchSettle,
) -> ReactivePrefetchPlan {
    let playing_micro = playback.playing && playback.playback_mode() == PlaybackMode::Micro;

    // Debounce: require the view to settle, unless playing (then track the
    // advancing cursor continuously — dedup keeps that idempotent).
    let signature = prefetch_signature(
        playback.playback_position(),
        elevation_selection,
        env.product_worker_string,
        env.site_id,
    );
    let settle_ms = if playing_micro { 0.0 } else { env.debounce_ms };
    if !settle.poll(signature, env.now_ms, settle_ms) || settle.already_resolved() {
        return ReactivePrefetchPlan::Skip;
    }

    // Stop adding background work once the session volume cap is hit. Mark
    // resolved so we don't recompute until the view moves.
    if env.auto_fetch_cap_reached {
        settle.mark_resolved();
        return ReactivePrefetchPlan::CapReached {
            status_message: "Auto-fetch limit reached — pausing background prefetch".to_string(),
        };
    }

    let pos = playback.playback_position();

    // Elevation scope: a Fixed cut scopes ingest to that elevation; Latest
    // may render any cut as the cursor advances, so fetch the whole volume.
    let elevation_filter = elevation_selection.elevation_number();

    let speed_mult = playback.speed.timeline_seconds_per_real_second();
    let forward = playback.time_model.direction == PlaybackDirection::Forward;
    let (win_start_i64, win_end_i64) = prefetch_window(pos, speed_mult, playback.playing, forward);

    let pos_i64 = pos as i64;
    // Look slightly back so the listing for the prior date is fetched near a
    // UTC midnight boundary; the render target is the `scan_at_or_before`
    // anchor.
    let dates_start_i64 = win_start_i64.min((pos - env.fallback_scan_duration_secs as f64) as i64);

    ReactivePrefetchPlan::Window(WindowPlan {
        dates: dates_spanning(dates_start_i64, win_end_i64),
        intersect_start: win_start_i64,
        win_end: win_end_i64,
        anchor_at_or_before: Some(pos_i64),
        elevation_filter,
        skip_missing_after: None,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Visible-range listing pump
// ───────────────────────────────────────────────────────────────────────────

/// Maximum visible span (seconds) for which the visible-range listing
/// pump will fetch archive listings. Zoomed out past this (weeks/months),
/// listing every visible day would be an S3 request storm for shadows
/// too small to read anyway.
pub(crate) const VISIBLE_LISTING_MAX_SPAN_SECS: f64 = 4.0 * 86_400.0;

/// Rate limit between new listing requests issued by the visible pump.
pub(crate) const VISIBLE_LISTING_INTERVAL_MS: f64 = 400.0;

/// Whether the visible-range listing pump may run this frame: the visible
/// span must be positive and readable (no listing storms at year zoom), and
/// the one-new-LIST rate limit must have elapsed.
pub(crate) fn visible_listing_pump_due(span_secs: f64, now_ms: f64, next_allowed_ms: f64) -> bool {
    span_secs > 0.0 && span_secs <= VISIBLE_LISTING_MAX_SPAN_SECS && now_ms >= next_allowed_ms
}

/// Shell snapshot of one visible date's listing state.
pub(crate) struct VisibleListingDay {
    pub date: NaiveDate,
    /// `archive_index.has_fresh(site, date, now, today)`.
    pub fresh: bool,
    /// Whether a listing request for this date is already in flight.
    pub listing_request_pending: bool,
    /// Per-(site, date) failure backoff deadline (wall-clock ms).
    pub backoff_until_ms: Option<f64>,
}

/// Described effects of the visible-listing decision, executed by the shell
/// in this field order.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct VisibleListingActions {
    /// `download_channel.fetch_listing(ctx, site, date)`.
    pub fetch: Option<NaiveDate>,
    /// New `visible_listing_next_ms` rate-limit deadline.
    pub next_allowed_ms: Option<f64>,
}

/// Pick the first visible date whose listing is worth (re)fetching: not in
/// the future, not fresh, not already in flight, not backed off. One new
/// LIST per [`VISIBLE_LISTING_INTERVAL_MS`] — the rest of the span fills in
/// on subsequent frames.
pub(crate) fn decide_visible_listing(
    days: &[VisibleListingDay],
    today: NaiveDate,
    now_ms: f64,
) -> VisibleListingActions {
    for day in days {
        if day.date > today || day.fresh || day.listing_request_pending {
            continue;
        }
        if day.backoff_until_ms.is_some_and(|until| now_ms < until) {
            continue;
        }
        return VisibleListingActions {
            fetch: Some(day.date),
            next_allowed_ms: Some(now_ms + VISIBLE_LISTING_INTERVAL_MS),
        };
    }
    VisibleListingActions::default()
}

// ───────────────────────────────────────────────────────────────────────────
// Selection bulk-fetch pump
// ───────────────────────────────────────────────────────────────────────────

/// Read-only frame context for the selection-fetch plan, shell-assembled.
pub(crate) struct SelectionFetchEnv {
    /// `render.coordinator.has_worker()`.
    pub has_worker: bool,
    /// `acquisition.state.is_paused()`.
    pub queue_paused: bool,
    /// `((start, end), armed_at_secs)` of the armed target, if any.
    pub target: Option<((f64, f64), f64)>,
    /// `download_queue.auto_fetch_cap_reached()`.
    pub auto_fetch_cap_reached: bool,
    /// `state.frame_now.secs()`.
    pub now_secs: f64,
    /// `crate::SELECTION_FETCH_DEADLINE_SECS`.
    pub deadline_secs: f64,
    /// `elevation_selection.elevation_number()`.
    pub elevation_filter: Option<u8>,
    /// Current UTC date — dates after it have no archive.
    pub today: NaiveDate,
}

/// What the selection bulk-fetch pump should do this frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SelectionFetchPlan {
    /// No worker, queue paused, or nothing armed.
    Skip,
    /// Disarm the target now (volume cap, degenerate span, or the listing
    /// deadline), surfacing `status_message` when set.
    Disarm { status_message: Option<String> },
    /// Fetch the *entire* selected span (day-by-day, so multi-day spans
    /// enumerate interior days), unlike the reactive pump's cursor window.
    Window(WindowPlan),
}

/// Decide the selection pump's bounded disarm conditions and, if still armed,
/// plan the full selected range. Disarm triggers, in order: the session
/// volume cap; a degenerate (<1 s) selection; the
/// [`crate::SELECTION_FETCH_DEADLINE_SECS`] backstop (a listing is stuck).
pub(crate) fn plan_selection_fetch(env: &SelectionFetchEnv) -> SelectionFetchPlan {
    if !env.has_worker || env.queue_paused {
        return SelectionFetchPlan::Skip;
    }
    let Some(((start, end), armed_at_secs)) = env.target else {
        return SelectionFetchPlan::Skip;
    };

    // Disarm: session volume cap reached.
    if env.auto_fetch_cap_reached {
        return SelectionFetchPlan::Disarm {
            status_message: Some(
                "Auto-fetch limit reached — selected range not fully downloaded".to_string(),
            ),
        };
    }

    // Degenerate selection — nothing to do.
    if (end - start).abs() < 1.0 {
        return SelectionFetchPlan::Disarm {
            status_message: None,
        };
    }

    // Disarm: absurdly wide selection. Defence in depth — the strip refuses to
    // create one, but a URL-restored or handle-dragged range at an extreme zoom
    // must never reach the listing enumeration.
    if !selection_span_allowed(start, end) {
        return SelectionFetchPlan::Disarm {
            status_message: Some(format!(
                "Selection too long to download — pick at most {} days",
                (MAX_SELECTION_SPAN_SECS / 86_400.0) as i64
            )),
        };
    }

    // Disarm: a listing is stuck (permanent 404 / network failure). The
    // hard backstop that guarantees termination regardless of outcome.
    if env.now_secs > armed_at_secs + env.deadline_secs {
        return SelectionFetchPlan::Disarm {
            status_message: Some(
                "Couldn't list part of the selected range — download may be incomplete".to_string(),
            ),
        };
    }

    let start_i64 = start as i64;
    let end_i64 = end as i64;
    SelectionFetchPlan::Window(WindowPlan {
        // Walk every UTC date the range touches (day-by-day, so multi-day
        // spans enumerate interior days — `dates_spanning` samples endpoints
        // only).
        dates: dates_in_range(start_i64, end_i64),
        intersect_start: start_i64,
        win_end: end_i64,
        anchor_at_or_before: None,
        elevation_filter: env.elevation_filter,
        skip_missing_after: Some(env.today),
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Lookback backfill pump
// ───────────────────────────────────────────────────────────────────────────

/// Gate + light 1 Hz throttle for the lookback backfill pump. Runs only in
/// `LookbackLoop` mode with a worker, an unpaused queue, and headroom under
/// the volume cap; on a passing frame the throttle deadline advances (the
/// enqueue is idempotent — this just avoids recomputing the window every
/// frame for the whole replay).
pub(crate) fn lookback_backfill_due(
    is_lookback: bool,
    has_worker: bool,
    queue_paused: bool,
    auto_fetch_cap_reached: bool,
    now_ms: f64,
    next_ms: &mut f64,
) -> bool {
    if !is_lookback || !has_worker || queue_paused || auto_fetch_cap_reached {
        return false;
    }
    if now_ms < *next_ms {
        return false;
    }
    *next_ms = now_ms + 1000.0;
    true
}

/// Plan the backfill window the active pinned loop covers, sized from its
/// basis (frame-count or duration). `resolved_start` is the loop window's
/// resolved start; the *start* is widened by the basis fallback span so a
/// frame-count loop still backfills enough archive before its frames are
/// cached (its resolved span collapses to near-zero then).
pub(crate) fn plan_lookback_backfill(
    resolved_start: f64,
    now_secs: f64,
    basis: LoopBasis,
    elevation_filter: Option<u8>,
) -> WindowPlan {
    let win_end_i64 = now_secs as i64;
    let win_start_i64 = resolved_start.min(now_secs - basis.fallback_span_secs()) as i64;
    WindowPlan {
        dates: dates_spanning(win_start_i64, win_end_i64),
        intersect_start: win_start_i64,
        win_end: win_end_i64,
        anchor_at_or_before: None,
        elevation_filter,
        skip_missing_after: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── reactive_prefetch_allowed ──

    #[wasm_bindgen_test]
    fn prefetch_runs_when_free_and_autofetch_on() {
        assert!(reactive_prefetch_allowed(false, false, true, false));
    }

    #[wasm_bindgen_test]
    fn prefetch_suppressed_when_autofetch_off() {
        assert!(!reactive_prefetch_allowed(false, false, false, false));
    }

    #[wasm_bindgen_test]
    fn prefetch_suppressed_when_attached_or_paused() {
        assert!(!reactive_prefetch_allowed(true, false, true, false));
        assert!(!reactive_prefetch_allowed(false, true, true, false));
    }

    // ── decide_selection_gate ──

    #[wasm_bindgen_test]
    fn selection_gate_arms_short_confirms_long() {
        // threshold 6h.
        let threshold = 6.0 * 3600.0;
        assert_eq!(
            decide_selection_gate(0.0, 3600.0, threshold),
            SelectionGate::Arm
        );
        // Exactly at threshold → arm (inclusive `<=`).
        assert_eq!(
            decide_selection_gate(0.0, threshold, threshold),
            SelectionGate::Arm
        );
        // Over threshold → confirm.
        assert_eq!(
            decide_selection_gate(0.0, threshold + 1.0, threshold),
            SelectionGate::Confirm
        );
        // Order-independent (abs).
        assert_eq!(
            decide_selection_gate(threshold + 100.0, 0.0, threshold),
            SelectionGate::Confirm
        );
    }

    // ── date spans ──

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// 2021-01-01 00:00:00 UTC = 1_609_459_200.
    const JAN1: i64 = 1_609_459_200;

    #[wasm_bindgen_test]
    fn dates_spanning_single_and_midnight_cross() {
        // Within one day → one date.
        assert_eq!(dates_spanning(JAN1, JAN1 + 3600), vec![day(2021, 1, 1)]);
        // Crossing midnight → two dates (endpoints only).
        assert_eq!(
            dates_spanning(JAN1 + 23 * 3600, JAN1 + 25 * 3600),
            vec![day(2021, 1, 1), day(2021, 1, 2)]
        );
    }

    #[wasm_bindgen_test]
    fn dates_in_range_enumerates_interior_days() {
        // 3-day span → all 3 dates, in order (interior day included).
        assert_eq!(
            dates_in_range(JAN1, JAN1 + 2 * 86400 + 100),
            vec![day(2021, 1, 1), day(2021, 1, 2), day(2021, 1, 3)]
        );
        // Reversed range clamps end to start → single day, never empty/looping.
        assert_eq!(dates_in_range(JAN1 + 100, JAN1), vec![day(2021, 1, 1)]);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// 2021-01-01 00:00:00 UTC.
    const JAN1: i64 = 1_609_459_200;

    // ── reactive_prefetch_allowed: complete the truth table ──

    #[wasm_bindgen_test]
    fn prefetch_all_false_is_blocked_by_autofetch_off() {
        // Not attached, not paused, but autofetch off → blocked.
        assert!(!reactive_prefetch_allowed(false, false, false, false));
    }

    #[wasm_bindgen_test]
    fn prefetch_blocked_when_attached_even_with_everything_else_permissive() {
        // Attached dominates regardless of paused/autofetch combos.
        assert!(!reactive_prefetch_allowed(true, false, true, false));
        assert!(!reactive_prefetch_allowed(true, true, true, false));
        assert!(!reactive_prefetch_allowed(true, false, false, false));
        assert!(!reactive_prefetch_allowed(true, true, false, false));
    }

    #[wasm_bindgen_test]
    fn prefetch_requires_all_four_conditions() {
        // The single allowing combination is exactly
        // (!attached, !paused, autofetch, !scrubbing).
        assert!(reactive_prefetch_allowed(false, false, true, false));
        // Flipping any one input flips the result to false.
        assert!(!reactive_prefetch_allowed(true, false, true, false));
        assert!(!reactive_prefetch_allowed(false, true, true, false));
        assert!(!reactive_prefetch_allowed(false, false, false, false));
        assert!(!reactive_prefetch_allowed(false, false, true, true));
    }

    #[wasm_bindgen_test]
    fn prefetch_blocked_while_a_scrub_drag_is_in_progress() {
        // The audit case: dragging the playhead across the archive must not
        // fire a fetch per scan crossed. Every other input is permissive here,
        // so the scrub flag is provably the thing doing the blocking.
        assert!(!reactive_prefetch_allowed(false, false, true, true));
        // …and releasing the drag re-allows it on the very next frame.
        assert!(reactive_prefetch_allowed(false, false, true, false));
    }

    // ── decide_selection_gate: edge spans ──

    #[wasm_bindgen_test]
    fn selection_gate_zero_width_span_arms() {
        // Identical endpoints → zero span → always arm (0 <= any non-negative threshold).
        assert_eq!(
            decide_selection_gate(1234.5, 1234.5, 0.0),
            SelectionGate::Arm
        );
        assert_eq!(decide_selection_gate(0.0, 0.0, 3600.0), SelectionGate::Arm);
    }

    #[wasm_bindgen_test]
    fn selection_gate_zero_threshold_confirms_any_positive_span() {
        // With threshold 0, any positive-width span must confirm; exact-zero span arms.
        assert_eq!(decide_selection_gate(0.0, 0.0, 0.0), SelectionGate::Arm);
        assert_eq!(decide_selection_gate(0.0, 1.0, 0.0), SelectionGate::Confirm);
    }

    #[wasm_bindgen_test]
    fn selection_gate_negative_coordinates_use_abs_width() {
        // Span width uses |end - start|; sign of coordinates is irrelevant.
        let threshold = 100.0;
        // width = |-50 - (-90)| = 40 <= 100 → arm
        assert_eq!(
            decide_selection_gate(-90.0, -50.0, threshold),
            SelectionGate::Arm
        );
        // width = |-200 - (-50)| = 150 > 100 → confirm
        assert_eq!(
            decide_selection_gate(-50.0, -200.0, threshold),
            SelectionGate::Confirm
        );
    }

    // ── dates_spanning: dedup and pre-epoch ──

    #[wasm_bindgen_test]
    fn dates_spanning_identical_endpoints_dedup_to_one() {
        // Same timestamp twice → the `contains` guard collapses to a single date.
        assert_eq!(dates_spanning(JAN1, JAN1), vec![day(2021, 1, 1)]);
        // Different seconds, same calendar day → still one date.
        assert_eq!(
            dates_spanning(JAN1 + 60, JAN1 + 12 * 3600),
            vec![day(2021, 1, 1)]
        );
    }

    #[wasm_bindgen_test]
    fn dates_spanning_endpoints_inserted_in_start_then_end_order() {
        // A reversed (end < start) pair across midnight still pushes the start's
        // date first, then the end's — endpoint sampling preserves argument order.
        assert_eq!(
            dates_spanning(JAN1 + 25 * 3600, JAN1 + 23 * 3600),
            vec![day(2021, 1, 2), day(2021, 1, 1)]
        );
    }

    #[wasm_bindgen_test]
    fn dates_spanning_pre_epoch_negative_timestamp() {
        // Negative seconds resolve to 1969 UTC dates.
        assert_eq!(dates_spanning(-86_400, -1), vec![day(1969, 12, 31)]);
    }

    // ── visible-listing settle: the pan request storm ──

    const DAY: f64 = 86_400.0;

    #[wasm_bindgen_test]
    fn panning_within_a_day_does_not_reset_the_listing_settle() {
        // The pump works per date, so moving inside one day needs no new
        // listings and must not keep re-arming the timer.
        let a = visible_listing_signature(DAY * 100.0, DAY * 100.0 + 3600.0, "KDMX");
        let b = visible_listing_signature(DAY * 100.0 + 7200.0, DAY * 100.0 + 10_800.0, "KDMX");
        assert_eq!(a, b);
    }

    #[wasm_bindgen_test]
    fn panning_across_a_day_boundary_resets_it() {
        let a = visible_listing_signature(DAY * 100.0, DAY * 100.0 + 3600.0, "KDMX");
        let b = visible_listing_signature(DAY * 101.0, DAY * 101.0 + 3600.0, "KDMX");
        assert!(a != b);
    }

    #[wasm_bindgen_test]
    fn the_signature_covers_both_edges_and_the_site() {
        let base = visible_listing_signature(DAY * 100.0, DAY * 101.0, "KDMX");
        // Widening the view (zoom out) brings new dates in.
        assert!(base != visible_listing_signature(DAY * 100.0, DAY * 103.0, "KDMX"));
        // A different site needs different listings entirely.
        assert!(base != visible_listing_signature(DAY * 100.0, DAY * 101.0, "KABR"));
    }

    #[wasm_bindgen_test]
    fn a_continuous_pan_never_settles_and_a_stopped_one_does() {
        // The reported bug: panning fired a LIST every rate-limit interval for
        // each date swept past. The rate limit bounds requests per second but
        // never stops them — only the settle does.
        let mut settle = PrefetchSettle::default();
        let settle_ms = 300.0;
        let mut now = 1000.0;

        // Sweeping one day per 100ms: the range keeps changing, so the pump
        // stays shut for the whole gesture.
        for day in 0..20 {
            let start = DAY * (100 + day) as f64;
            let sig = visible_listing_signature(start, start + DAY, "KDMX");
            assert!(
                !settle.poll(sig, now, settle_ms),
                "fired mid-pan at day {day}"
            );
            now += 100.0;
        }

        // Pointer released — the range holds still and the pump opens once the
        // settle elapses.
        let start = DAY * 120.0;
        let sig = visible_listing_signature(start, start + DAY, "KDMX");
        assert!(!settle.poll(sig, now, settle_ms));
        assert!(!settle.poll(sig, now + settle_ms - 1.0, settle_ms));
        assert!(settle.poll(sig, now + settle_ms, settle_ms));
    }

    // ── span guardrails for the wide Archive zooms ──

    #[wasm_bindgen_test]
    fn dates_in_range_is_capped() {
        // The walk allocates one NaiveDate per day, and each becomes a
        // fetch_listings intent. Now that the timeline reaches decades, an
        // unbounded range would mean tens of thousands of S3 LIST requests.
        let thirty_six_years = JAN1 + (36 * 365 + 9) * 86_400;
        let dates = dates_in_range(JAN1, thirty_six_years);
        assert_eq!(dates.len(), MAX_ENUMERATED_DATES);
        // Still ordered and starting where asked.
        assert_eq!(dates[0], day(2021, 1, 1));
        assert!(dates.windows(2).all(|w| w[0] < w[1]));
    }

    #[wasm_bindgen_test]
    fn ordinary_ranges_are_unaffected_by_the_cap() {
        assert_eq!(dates_in_range(JAN1, JAN1 + 2 * 86_400).len(), 3);
    }

    #[wasm_bindgen_test]
    fn selection_span_gate_allows_real_events_and_rejects_years() {
        // A multi-day severe-weather event is a legitimate selection…
        assert!(selection_span_allowed(0.0, 6.0 * 86_400.0));
        assert!(selection_span_allowed(0.0, MAX_SELECTION_SPAN_SECS));
        // …a multi-year drag at Archive zoom is not.
        assert!(!selection_span_allowed(0.0, MAX_SELECTION_SPAN_SECS + 1.0));
        assert!(!selection_span_allowed(0.0, 3.0 * 365.0 * 86_400.0));
        // Direction-agnostic (a drag can go either way).
        assert!(!selection_span_allowed(3.0 * 365.0 * 86_400.0, 0.0));
        // Degenerate inputs don't pass by accident.
        assert!(!selection_span_allowed(0.0, f64::INFINITY));
        assert!(!selection_span_allowed(0.0, f64::NAN));
    }

    #[wasm_bindgen_test]
    fn plan_selection_fetch_disarms_on_an_over_span_selection() {
        // Defence in depth: the strip refuses to create one, but a
        // URL-restored or handle-dragged range must never reach enumeration.
        let env = SelectionFetchEnv {
            has_worker: true,
            queue_paused: false,
            target: Some(((0.0, 3.0 * 365.0 * 86_400.0), 0.0)),
            auto_fetch_cap_reached: false,
            now_secs: 1000.0,
            deadline_secs: 30.0,
            elevation_filter: Some(1),
            today: day(2021, 1, 5),
        };
        match plan_selection_fetch(&env) {
            SelectionFetchPlan::Disarm { status_message } => {
                assert!(status_message.is_some_and(|m| m.contains("too long")));
            }
            other => panic!("expected Disarm, got {other:?}"),
        }
    }

    // ── dates_in_range: single-day, month/year boundaries, continuity ──

    #[wasm_bindgen_test]
    fn dates_in_range_single_second_is_one_date() {
        // end == start → exactly one date, no looping.
        assert_eq!(dates_in_range(JAN1, JAN1), vec![day(2021, 1, 1)]);
        // sub-day range stays one date.
        assert_eq!(dates_in_range(JAN1, JAN1 + 3600), vec![day(2021, 1, 1)]);
    }

    #[wasm_bindgen_test]
    fn dates_in_range_crosses_month_boundary() {
        // 2021-01-31 12:00 → 2021-02-01 12:00 enumerates both month-edge dates.
        let m_start = JAN1 + 30 * 86_400 + 43_200; // 2021-01-31 12:00
        let m_end = JAN1 + 31 * 86_400 + 43_200; // 2021-02-01 12:00
        assert_eq!(
            dates_in_range(m_start, m_end),
            vec![day(2021, 1, 31), day(2021, 2, 1)]
        );
    }

    #[wasm_bindgen_test]
    fn dates_in_range_crosses_year_boundary_continuously() {
        // 2021-12-31 12:00 → 2022-01-02 12:00 walks every interior day across the
        // year rollover with no gaps.
        let y_start = 1_640_952_000; // 2021-12-31 12:00:00 UTC
        let y_end = y_start + 2 * 86_400; // 2022-01-02 12:00:00 UTC
        assert_eq!(
            dates_in_range(y_start, y_end),
            vec![day(2021, 12, 31), day(2022, 1, 1), day(2022, 1, 2)]
        );
    }
}

#[cfg(test)]
mod reducer_tests {
    use super::*;
    use crate::core::{Scan, Sweep, TimelineTier};
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── builders ────────────────────────────────────────────────────────────

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// 2021-01-01 00:00:00 UTC = 1_609_459_200.
    const JAN1: i64 = 1_609_459_200;

    /// Match the shell's `crate::SCAN_CACHE_MATCH_TOLERANCE_SECS` shape.
    const TOL: i64 = 15;

    fn snap(files: &[(&'static str, i64, i64)]) -> ListingSnapshot<'static> {
        ListingSnapshot {
            file_names: files.iter().map(|&(n, _, _)| n).collect(),
            boundaries: files
                .iter()
                .map(|&(_, s, e)| ScanBoundary { start: s, end: e })
                .collect(),
        }
    }

    fn sweep(elev_num: u8, start: f64, end: f64) -> Sweep {
        Sweep {
            start_time: start,
            end_time: end,
            elevation: elev_num as f32 * 0.5,
            elevation_number: elev_num,
            start_azimuth: 0.0,
            radials: Vec::new(),
            cached_products: vec!["reflectivity".to_string()],
        }
    }

    fn cached_scan(key_ts: f64, sweeps: Vec<Sweep>) -> Scan {
        Scan {
            start_time: key_ts,
            end_time: key_ts + 300.0,
            key_timestamp: key_ts,
            vcp: 215,
            vcp_pattern: None,
            sweeps,
            completeness: None,
            cached_sweep_count: None,
            planned_sweep_count: None,
        }
    }

    fn empty_timeline() -> RadarTimeline {
        RadarTimeline { scans: Vec::new() }
    }

    fn playback_at(pos: f64) -> PlaybackState {
        let mut playback = PlaybackState::default();
        playback.set_playback_position(pos);
        playback
    }

    fn reactive_env(now_ms: f64) -> ReactivePrefetchEnv<'static> {
        ReactivePrefetchEnv {
            auto_fetch_cap_reached: false,
            now_ms,
            debounce_ms: 300.0,
            site_id: "KDMX",
            product_worker_string: "reflectivity",
            fallback_scan_duration_secs: 300,
        }
    }

    // ── listing index queries ───────────────────────────────────────────────

    // Half-open window predicate over boundaries, preserving listing order.
    #[wasm_bindgen_test]
    fn intersecting_indices_half_open_and_order() {
        let b = [
            ScanBoundary {
                start: 1000,
                end: 1300,
            },
            ScanBoundary {
                start: 1300,
                end: 1600,
            },
            ScanBoundary {
                start: 1600,
                end: 1900,
            },
        ];
        // range_end == a boundary's start excludes it; range_start == an end
        // excludes the earlier one.
        assert_eq!(intersecting_indices(&b, 1100, 1300), vec![0]);
        assert_eq!(intersecting_indices(&b, 1300, 1500), vec![1]);
        assert_eq!(intersecting_indices(&b, 1200, 1700), vec![0, 1, 2]);
        assert!(intersecting_indices(&b, 2000, 3000).is_empty());
        assert!(intersecting_indices(&[], 0, 10_000).is_empty());
    }

    // Inclusive at-or-before with the last-max tie rule (mirrors the old
    // zip + max_by_key listing walk exactly).
    #[wasm_bindgen_test]
    fn at_or_before_index_inclusive_and_ties() {
        let b = [
            ScanBoundary {
                start: 1000,
                end: 1300,
            },
            ScanBoundary {
                start: 1300,
                end: 1600,
            },
        ];
        assert_eq!(at_or_before_index(&b, 999), None);
        assert_eq!(at_or_before_index(&b, 1000), Some(0)); // inclusive
        assert_eq!(at_or_before_index(&b, 1299), Some(0));
        assert_eq!(at_or_before_index(&b, 1300), Some(1));
        assert_eq!(at_or_before_index(&b, 999_999), Some(1)); // dead time → most recent
        assert_eq!(at_or_before_index(&[], 1000), None);

        // Duplicate starts: max_by_key keeps the LAST maximum.
        let dup = [
            ScanBoundary {
                start: 1000,
                end: 1300,
            },
            ScanBoundary {
                start: 1000,
                end: 1300,
            },
        ];
        assert_eq!(at_or_before_index(&dup, 1500), Some(1));
    }

    // ── prefetch_window (moved from nexrad::download_queue) ────────────────

    #[wasm_bindgen_test]
    fn prefetch_window_paused_is_degenerate_anchor_only() {
        // Paused: no lead, no trail — only the scan under the playhead is
        // wanted (via anchor_at_or_before), regardless of speed/direction.
        assert_eq!(
            prefetch_window(10_000.0, 300.0, false, true),
            (10_000, 10_000)
        );
        assert_eq!(
            prefetch_window(10_000.0, 1_000_000.0, false, false),
            (10_000, 10_000)
        );
    }

    #[wasm_bindgen_test]
    fn prefetch_window_playing_scales_lead_with_speed() {
        // Playing fast forward: lead scales with speed, no trail behind.
        let (s, e) = prefetch_window(10_000.0, 1200.0, true, true);
        assert_eq!(s, 10_000);
        assert_eq!(
            e,
            (10_000.0 + 1200.0 * crate::PREFETCH_PLAY_LEAD_SECS) as i64
        );
    }

    #[wasm_bindgen_test]
    fn prefetch_window_playing_slow_floors_at_one_scan() {
        // Playing but slow: speed*PLAY_LEAD (4 s) < one scan → floor at
        // FALLBACK_SCAN_DURATION_SECS so playback never waits on a cold fetch.
        let (s, e) = prefetch_window(0.0, 1.0, true, true);
        assert_eq!(s, 0);
        assert_eq!(e, crate::FALLBACK_SCAN_DURATION_SECS);
    }

    #[wasm_bindgen_test]
    fn prefetch_window_playing_backward_mirrors_lead() {
        let speed = 1000.0;
        let expected_lead = speed * crate::PREFETCH_PLAY_LEAD_SECS; // 4000 > 300
        let (s, e) = prefetch_window(50_000.0, speed, true, false);
        // Backward: lead extends behind, nothing ahead.
        assert_eq!(s, (50_000.0 - expected_lead) as i64);
        assert_eq!(e, 50_000);
    }

    // ── prefetch_already_satisfied ──────────────────────────────────────────

    fn intent(scan_start: i64, elevation_filter: Option<u8>) -> ScanFetchIntent {
        ScanFetchIntent {
            date: day(2021, 1, 1),
            file_name: format!("f{scan_start}"),
            scan_start,
            scan_end: scan_start + 300,
            elevation_filter,
        }
    }

    #[wasm_bindgen_test]
    fn satisfied_when_scan_start_queued() {
        let tl = empty_timeline();
        assert!(prefetch_already_satisfied(
            &intent(1300, Some(1)),
            &[999, 1300],
            &tl,
            TOL
        ));
        // Only an exact scan_start match counts as queued.
        assert!(!prefetch_already_satisfied(
            &intent(1300, Some(1)),
            &[1299, 1301],
            &tl,
            TOL
        ));
    }

    #[wasm_bindgen_test]
    fn satisfied_fixed_cut_requires_matching_elevation_within_tolerance() {
        let tl = RadarTimeline {
            scans: vec![cached_scan(1305.0, vec![sweep(1, 1305.0, 1315.0)])],
        };
        // Within tolerance (|1305-1300| = 5 < 15) and elevation 1 stored.
        assert!(prefetch_already_satisfied(
            &intent(1300, Some(1)),
            &[],
            &tl,
            TOL
        ));
        // The stored elevation doesn't match the filter → not satisfied.
        assert!(!prefetch_already_satisfied(
            &intent(1300, Some(2)),
            &[],
            &tl,
            TOL
        ));
        // Exactly at tolerance is NOT a match (strict <).
        assert!(!prefetch_already_satisfied(
            &intent(1305 - TOL, Some(1)),
            &[],
            &tl,
            TOL
        ));
    }

    #[wasm_bindgen_test]
    fn satisfied_latest_treats_any_cached_sweep_as_enough() {
        // Whole-volume intent (Latest): any sweep satisfies…
        let tl = RadarTimeline {
            scans: vec![cached_scan(1300.0, vec![sweep(3, 1300.0, 1310.0)])],
        };
        assert!(prefetch_already_satisfied(
            &intent(1300, None),
            &[],
            &tl,
            TOL
        ));
        // …but a sweepless scan record does not.
        let tl = RadarTimeline {
            scans: vec![cached_scan(1300.0, Vec::new())],
        };
        assert!(!prefetch_already_satisfied(
            &intent(1300, None),
            &[],
            &tl,
            TOL
        ));
    }

    // ── prefetch_signature ──────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn signature_stable_within_bucket_changes_across_inputs() {
        let fixed = ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 0.5,
        };
        let base = prefetch_signature(10.0, &fixed, "reflectivity", "KDMX");
        // Same 30 s bucket → same signature (small scrubs don't reset settle).
        assert_eq!(
            base,
            prefetch_signature(29.9, &fixed, "reflectivity", "KDMX")
        );
        // Next bucket → different.
        assert_ne!(
            base,
            prefetch_signature(30.0, &fixed, "reflectivity", "KDMX")
        );
        // Product / site changes → different.
        assert_ne!(base, prefetch_signature(10.0, &fixed, "velocity", "KDMX"));
        assert_ne!(
            base,
            prefetch_signature(10.0, &fixed, "reflectivity", "KABR")
        );
    }

    #[wasm_bindgen_test]
    fn signature_distinguishes_elevation_scope() {
        let fixed1 = ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 0.5,
        };
        let fixed2 = ElevationSelection::Fixed {
            elevation_number: 2,
            angle: 0.9,
        };
        let a = prefetch_signature(10.0, &fixed1, "reflectivity", "KDMX");
        let b = prefetch_signature(10.0, &fixed2, "reflectivity", "KDMX");
        let c = prefetch_signature(10.0, &ElevationSelection::Latest, "reflectivity", "KDMX");
        assert_ne!(a, b);
        assert_ne!(a, c);
        // The angle is NOT part of the signature — only the number.
        let fixed1_other_angle = ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 1.4,
        };
        assert_eq!(
            a,
            prefetch_signature(10.0, &fixed1_other_angle, "reflectivity", "KDMX")
        );
    }

    // ── PrefetchSettle (moved from subsystem::acquisition) ─────────────────

    #[wasm_bindgen_test]
    fn settle_waits_for_stability_then_resets_on_change() {
        let mut s = PrefetchSettle::default();
        // First sighting starts the timer; not settled yet.
        assert!(!s.poll(42, 1000.0, 300.0));
        // Still inside the debounce window.
        assert!(!s.poll(42, 1200.0, 300.0));
        // Past the window → settled.
        assert!(s.poll(42, 1300.0, 300.0));
        // A new signature resets the timer (e.g. the user scrubbed elsewhere).
        assert!(!s.poll(99, 1300.0, 300.0));
        assert!(s.poll(99, 1600.0, 300.0));
    }

    #[wasm_bindgen_test]
    fn settle_zero_window_fires_immediately() {
        // Playback passes settle_ms = 0: a stable signature fires at once.
        let mut s = PrefetchSettle::default();
        assert!(s.poll(7, 500.0, 0.0));
    }

    #[wasm_bindgen_test]
    fn resolved_marker_clears_when_signature_changes() {
        let mut s = PrefetchSettle::default();
        assert!(s.poll(1, 0.0, 0.0));
        assert!(!s.already_resolved());
        s.mark_resolved();
        assert!(s.already_resolved());
        // Moving the view (new signature) clears the resolved marker so the
        // pump re-evaluates.
        s.poll(2, 0.0, 0.0);
        assert!(!s.already_resolved());
    }

    #[wasm_bindgen_test]
    fn default_is_not_resolved() {
        let s = PrefetchSettle::default();
        assert!(!s.already_resolved());
    }

    #[wasm_bindgen_test]
    fn poll_at_exact_settle_boundary_fires() {
        let mut s = PrefetchSettle::default();
        // Use a non-zero signature so the timer starts at now=1000.
        assert!(!s.poll(5, 1000.0, 500.0));
        // Exactly settle_ms later: 1500 - 1000 = 500 >= 500 → settled.
        assert!(s.poll(5, 1500.0, 500.0));
    }

    #[wasm_bindgen_test]
    fn signature_zero_collides_with_default_and_never_starts_timer() {
        // The default last_signature is 0, so the first poll of signature 0 sees
        // "no change", never sets settled_since_ms, and reports unsettled even
        // with a zero window. (Real signatures are hashes, so 0 is vanishingly
        // unlikely — this pins the documented edge.)
        let mut s = PrefetchSettle::default();
        assert!(!s.poll(0, 500.0, 0.0));
        assert!(!s.poll(0, 9999.0, 0.0));
    }

    #[wasm_bindgen_test]
    fn resolved_marker_survives_repolling_same_signature() {
        let mut s = PrefetchSettle::default();
        s.poll(11, 0.0, 0.0);
        s.mark_resolved();
        assert!(s.already_resolved());
        // Re-polling the SAME signature must not clear the resolved marker.
        s.poll(11, 100.0, 0.0);
        assert!(s.already_resolved());
    }

    #[wasm_bindgen_test]
    fn resolved_marker_is_per_signature() {
        let mut s = PrefetchSettle::default();
        s.poll(1, 0.0, 0.0);
        s.mark_resolved();
        // Different signature → not resolved; back to the first → also not
        // resolved (the marker tracks only the latest signature).
        s.poll(2, 0.0, 0.0);
        assert!(!s.already_resolved());
        s.poll(1, 0.0, 0.0);
        assert!(!s.already_resolved());
    }

    // ── decide_anchor_fast_path ─────────────────────────────────────────────

    fn anchor_env<'a>(
        listing: Option<&'a ListingSnapshot<'a>>,
        queued: &'a [i64],
    ) -> AnchorFastPathEnv<'a> {
        AnchorFastPathEnv {
            auto_fetch_cap_reached: false,
            playback_pos: 1500,
            date: day(2021, 1, 1),
            elevation_filter: Some(1),
            listing,
            queued_scan_starts: queued,
            cache_match_tolerance_secs: TOL,
        }
    }

    #[wasm_bindgen_test]
    fn anchor_enqueues_at_or_before_scan_even_in_dead_time() {
        // Playhead at 1500 sits past b's start (1300) — at-or-before picks b,
        // the scan the render side would show.
        let listing = snap(&[("a", 1000, 1300), ("b", 1300, 1600)]);
        let tl = empty_timeline();
        let got =
            decide_anchor_fast_path(&anchor_env(Some(&listing), &[]), &tl).expect("anchor intent");
        assert_eq!(got.file_name, "b");
        assert_eq!(got.scan_start, 1300);
        assert_eq!(got.scan_end, 1600);
        assert_eq!(got.date, day(2021, 1, 1));
        assert_eq!(got.elevation_filter, Some(1));
    }

    #[wasm_bindgen_test]
    fn anchor_cap_reached_short_circuits() {
        let listing = snap(&[("a", 1000, 1300)]);
        let tl = empty_timeline();
        let mut env = anchor_env(Some(&listing), &[]);
        env.auto_fetch_cap_reached = true;
        assert_eq!(decide_anchor_fast_path(&env, &tl), None);
    }

    #[wasm_bindgen_test]
    fn anchor_needs_a_listing_and_a_started_scan() {
        let tl = empty_timeline();
        // No listing cached for the playhead's date → nothing (listings are
        // left to the debounced/visible pumps).
        assert_eq!(decide_anchor_fast_path(&anchor_env(None, &[]), &tl), None);
        // Listed, but the playhead is before the first scan ever started.
        let listing = snap(&[("a", 2000, 2300)]);
        assert_eq!(
            decide_anchor_fast_path(&anchor_env(Some(&listing), &[]), &tl),
            None
        );
    }

    #[wasm_bindgen_test]
    fn anchor_suppressed_when_queued_or_cached() {
        let listing = snap(&[("b", 1300, 1600)]);
        // Already queued → None.
        let tl = empty_timeline();
        assert_eq!(
            decide_anchor_fast_path(&anchor_env(Some(&listing), &[1300]), &tl),
            None
        );
        // Cached at the active elevation → None.
        let tl = RadarTimeline {
            scans: vec![cached_scan(1300.0, vec![sweep(1, 1300.0, 1310.0)])],
        };
        assert_eq!(
            decide_anchor_fast_path(&anchor_env(Some(&listing), &[]), &tl),
            None
        );
        // Cached only at a different elevation → still wanted.
        let tl = RadarTimeline {
            scans: vec![cached_scan(1300.0, vec![sweep(2, 1300.0, 1310.0)])],
        };
        assert!(decide_anchor_fast_path(&anchor_env(Some(&listing), &[]), &tl).is_some());
    }

    // ── plan_reactive_prefetch ──────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn reactive_skips_until_settled_then_plans_the_cursor_window() {
        let playback = playback_at(10_000.0);
        let sel = ElevationSelection::default(); // Fixed elev 1
        let mut settle = PrefetchSettle::default();

        // First sighting starts the debounce → Skip.
        assert_eq!(
            plan_reactive_prefetch(&reactive_env(1000.0), &playback, &sel, &mut settle),
            ReactivePrefetchPlan::Skip
        );
        // Settled 300 ms later → the paused forward window around the cursor.
        let plan = match plan_reactive_prefetch(&reactive_env(1300.0), &playback, &sel, &mut settle)
        {
            ReactivePrefetchPlan::Window(w) => w,
            other => panic!("expected Window, got {other:?}"),
        };
        // Paused: degenerate window at the cursor; the anchor is the fetch.
        assert_eq!(plan.intersect_start, 10_000);
        assert_eq!(plan.win_end, 10_000);
        assert_eq!(plan.anchor_at_or_before, Some(10_000));
        assert_eq!(plan.elevation_filter, Some(1));
        assert_eq!(plan.skip_missing_after, None);
        assert_eq!(plan.dates, vec![day(1970, 1, 1)]);

        // Not yet marked resolved → the settled view keeps re-evaluating…
        assert!(matches!(
            plan_reactive_prefetch(&reactive_env(1300.0), &playback, &sel, &mut settle),
            ReactivePrefetchPlan::Window(_)
        ));
        // …until the shell marks it handled.
        settle.mark_resolved();
        assert_eq!(
            plan_reactive_prefetch(&reactive_env(1300.0), &playback, &sel, &mut settle),
            ReactivePrefetchPlan::Skip
        );
    }

    #[wasm_bindgen_test]
    fn reactive_playing_micro_collapses_the_debounce() {
        let mut playback = playback_at(10_000.0);
        playback.playing = true;
        playback.timeline_tier = TimelineTier::Micro;
        let sel = ElevationSelection::default();
        let mut settle = PrefetchSettle::default();

        // Playing in micro: settle_ms = 0, so the very first frame plans.
        assert!(matches!(
            plan_reactive_prefetch(&reactive_env(1000.0), &playback, &sel, &mut settle),
            ReactivePrefetchPlan::Window(_)
        ));

        // Playing in MACRO keeps the debounce (frame jumps aren't cursor
        // tracking) → first sighting skips.
        let mut playback = playback_at(10_000.0);
        playback.playing = true;
        playback.timeline_tier = TimelineTier::Macro;
        let mut settle = PrefetchSettle::default();
        assert_eq!(
            plan_reactive_prefetch(&reactive_env(1000.0), &playback, &sel, &mut settle),
            ReactivePrefetchPlan::Skip
        );
    }

    #[wasm_bindgen_test]
    fn reactive_cap_reached_reports_status_and_resolves() {
        let playback = playback_at(10_000.0);
        let sel = ElevationSelection::default();
        let mut settle = PrefetchSettle::default();
        let _ = plan_reactive_prefetch(&reactive_env(1000.0), &playback, &sel, &mut settle);

        let mut env = reactive_env(1300.0);
        env.auto_fetch_cap_reached = true;
        assert_eq!(
            plan_reactive_prefetch(&env, &playback, &sel, &mut settle),
            ReactivePrefetchPlan::CapReached {
                status_message: "Auto-fetch limit reached — pausing background prefetch"
                    .to_string()
            }
        );
        // Marked resolved: the capped view is not recomputed until it moves.
        assert_eq!(
            plan_reactive_prefetch(&env, &playback, &sel, &mut settle),
            ReactivePrefetchPlan::Skip
        );
    }

    #[wasm_bindgen_test]
    fn reactive_window_spans_midnight_for_the_prior_dates_listing() {
        // Cursor 100 s after a UTC midnight: the anchor scan may start on the
        // prior date, so the date span reaches one scan back and both dates'
        // listings are consulted even though the window itself is degenerate.
        let playback = playback_at((JAN1 + 100) as f64);
        let sel = ElevationSelection::Latest;
        let mut settle = PrefetchSettle::default();
        let _ = plan_reactive_prefetch(&reactive_env(0.0), &playback, &sel, &mut settle);
        let plan = match plan_reactive_prefetch(&reactive_env(300.0), &playback, &sel, &mut settle)
        {
            ReactivePrefetchPlan::Window(w) => w,
            other => panic!("expected Window, got {other:?}"),
        };
        assert_eq!(plan.dates, vec![day(2020, 12, 31), day(2021, 1, 1)]);
        assert_eq!(plan.intersect_start, JAN1 + 100);
        assert_eq!(plan.win_end, JAN1 + 100);
        // Latest fetches the whole volume.
        assert_eq!(plan.elevation_filter, None);
    }

    // ── reduce_window_intents ───────────────────────────────────────────────

    fn present_day(
        date: NaiveDate,
        files: &'static [(&'static str, i64, i64)],
    ) -> WindowDayInput<'static> {
        WindowDayInput {
            date,
            listing: Some(snap(files)),
            backoff_until_ms: None,
            listing_request_pending: false,
        }
    }

    fn missing_day(date: NaiveDate) -> WindowDayInput<'static> {
        WindowDayInput {
            date,
            listing: None,
            backoff_until_ms: None,
            listing_request_pending: false,
        }
    }

    fn window(intersect_start: i64, win_end: i64) -> WindowPlan {
        WindowPlan {
            dates: Vec::new(), // the shell derives `days` from these; unused here
            intersect_start,
            win_end,
            anchor_at_or_before: None,
            elevation_filter: Some(2),
            skip_missing_after: None,
        }
    }

    const D1_FILES: &[(&str, i64, i64)] =
        &[("a", 1000, 1300), ("b", 1300, 1600), ("c", 1600, 1900)];

    #[wasm_bindgen_test]
    fn window_collects_intersecting_scans_and_dedups_the_anchor() {
        let mut plan = window(1350, 1700);
        plan.anchor_at_or_before = Some(1360); // → b, which also intersects
        let days = vec![present_day(day(2021, 1, 1), D1_FILES)];
        let a = reduce_window_intents(&plan, &days, 0.0, &[], &empty_timeline(), TOL);

        // b + c intersect; the anchor's duplicate b collapses; sorted by start.
        let got: Vec<(&str, i64, i64)> = a
            .enqueue
            .iter()
            .map(|i| (i.file_name.as_str(), i.scan_start, i.scan_end))
            .collect();
        assert_eq!(got, vec![("b", 1300, 1600), ("c", 1600, 1900)]);
        assert!(a.enqueue.iter().all(|i| i.elevation_filter == Some(2)));
        assert!(a.enqueue.iter().all(|i| i.date == day(2021, 1, 1)));
        assert!(!a.listing_pending);
        assert!(a.fetch_listings.is_empty());
    }

    #[wasm_bindgen_test]
    fn window_anchor_reaches_behind_an_empty_window() {
        // The window itself intersects nothing, but the at-or-before anchor
        // still wants the scan under the cursor (the shadow-click case).
        let mut plan = window(2000, 2100);
        plan.anchor_at_or_before = Some(1999);
        let days = vec![present_day(day(2021, 1, 1), D1_FILES)];
        let a = reduce_window_intents(&plan, &days, 0.0, &[], &empty_timeline(), TOL);
        let got: Vec<&str> = a.enqueue.iter().map(|i| i.file_name.as_str()).collect();
        assert_eq!(got, vec!["c"]);
    }

    #[wasm_bindgen_test]
    fn window_missing_listing_is_pending_and_fetched_once_allowed() {
        let plan = window(1000, 2000);
        let now_ms = 5000.0;

        // Plain missing date → pending + fetched.
        let days = vec![missing_day(day(2021, 1, 1))];
        let a = reduce_window_intents(&plan, &days, now_ms, &[], &empty_timeline(), TOL);
        assert!(a.listing_pending);
        assert_eq!(a.fetch_listings, vec![day(2021, 1, 1)]);
        assert!(a.enqueue.is_empty());

        // Backed off (now < until) → still pending, but no fetch.
        let mut d = missing_day(day(2021, 1, 1));
        d.backoff_until_ms = Some(now_ms + 1.0);
        let a = reduce_window_intents(&plan, &[d], now_ms, &[], &empty_timeline(), TOL);
        assert!(a.listing_pending);
        assert!(a.fetch_listings.is_empty());

        // Backoff expired exactly at now (now < until is false) → fetched.
        let mut d = missing_day(day(2021, 1, 1));
        d.backoff_until_ms = Some(now_ms);
        let a = reduce_window_intents(&plan, &[d], now_ms, &[], &empty_timeline(), TOL);
        assert_eq!(a.fetch_listings, vec![day(2021, 1, 1)]);

        // A listing request already in flight → pending, no duplicate fetch.
        let mut d = missing_day(day(2021, 1, 1));
        d.listing_request_pending = true;
        let a = reduce_window_intents(&plan, &[d], now_ms, &[], &empty_timeline(), TOL);
        assert!(a.listing_pending);
        assert!(a.fetch_listings.is_empty());
    }

    #[wasm_bindgen_test]
    fn window_future_missing_dates_skipped_only_with_the_selection_gate() {
        let today = day(2021, 1, 1);
        let mut plan = window(1000, 2000);

        // Reactive/lookback plans (no gate): even a future date counts.
        let days = vec![missing_day(day(2021, 1, 2))];
        let a = reduce_window_intents(&plan, &days, 0.0, &[], &empty_timeline(), TOL);
        assert!(a.listing_pending);

        // Selection plans gate on today: the future date is ignored…
        plan.skip_missing_after = Some(today);
        let a = reduce_window_intents(&plan, &days, 0.0, &[], &empty_timeline(), TOL);
        assert!(!a.listing_pending);
        assert!(a.fetch_listings.is_empty());

        // …but today itself (date <= today) still counts as fetchable.
        let days = vec![missing_day(today)];
        let a = reduce_window_intents(&plan, &days, 0.0, &[], &empty_timeline(), TOL);
        assert!(a.listing_pending);
        assert_eq!(a.fetch_listings, vec![today]);
    }

    #[wasm_bindgen_test]
    fn window_drops_already_satisfied_scans() {
        let plan = window(1000, 2000);
        let days = vec![present_day(day(2021, 1, 1), D1_FILES)];

        // b is already queued; c is cached at the active elevation (2).
        let tl = RadarTimeline {
            scans: vec![cached_scan(1600.0, vec![sweep(2, 1600.0, 1610.0)])],
        };
        let a = reduce_window_intents(&plan, &days, 0.0, &[1300], &tl, TOL);
        let got: Vec<&str> = a.enqueue.iter().map(|i| i.file_name.as_str()).collect();
        assert_eq!(got, vec!["a"]);
    }

    #[wasm_bindgen_test]
    fn window_sorts_across_days_by_scan_start() {
        // Two days' listings contribute out of order — the queue-shaping sort
        // orders the enqueue by scan_start.
        let plan = window(0, 10_000);
        let days = vec![
            present_day(day(2021, 1, 2), &[("late", 5000, 5300)]),
            present_day(day(2021, 1, 1), &[("early", 1000, 1300)]),
        ];
        let a = reduce_window_intents(&plan, &days, 0.0, &[], &empty_timeline(), TOL);
        let got: Vec<&str> = a.enqueue.iter().map(|i| i.file_name.as_str()).collect();
        assert_eq!(got, vec!["early", "late"]);
    }

    // ── visible-range listing pump ──────────────────────────────────────────

    #[wasm_bindgen_test]
    fn visible_pump_due_gates_span_and_rate() {
        // Degenerate or oversized spans never list.
        assert!(!visible_listing_pump_due(0.0, 1000.0, 0.0));
        assert!(!visible_listing_pump_due(-5.0, 1000.0, 0.0));
        assert!(!visible_listing_pump_due(
            VISIBLE_LISTING_MAX_SPAN_SECS + 1.0,
            1000.0,
            0.0
        ));
        // Exactly at the max span is allowed (inclusive <=).
        assert!(visible_listing_pump_due(
            VISIBLE_LISTING_MAX_SPAN_SECS,
            1000.0,
            0.0
        ));
        // Rate limit: strictly before the deadline blocks; at it, runs.
        assert!(!visible_listing_pump_due(3600.0, 999.0, 1000.0));
        assert!(visible_listing_pump_due(3600.0, 1000.0, 1000.0));
    }

    fn visible_day(date: NaiveDate) -> VisibleListingDay {
        VisibleListingDay {
            date,
            fresh: false,
            listing_request_pending: false,
            backoff_until_ms: None,
        }
    }

    #[wasm_bindgen_test]
    fn visible_listing_picks_first_eligible_and_sets_the_rate_deadline() {
        let today = day(2021, 1, 10);
        let now_ms = 10_000.0;
        let mut future = visible_day(day(2021, 1, 11));
        let mut fresh = visible_day(day(2021, 1, 5));
        fresh.fresh = true;
        let mut pending = visible_day(day(2021, 1, 6));
        pending.listing_request_pending = true;
        let mut backed_off = visible_day(day(2021, 1, 7));
        backed_off.backoff_until_ms = Some(now_ms + 1.0);
        let eligible = visible_day(day(2021, 1, 8));
        let also_eligible = visible_day(day(2021, 1, 9));
        future.fresh = false;

        let days = vec![future, fresh, pending, backed_off, eligible, also_eligible];
        let a = decide_visible_listing(&days, today, now_ms);
        // First eligible wins; ONE new LIST per interval.
        assert_eq!(a.fetch, Some(day(2021, 1, 8)));
        assert_eq!(
            a.next_allowed_ms,
            Some(now_ms + VISIBLE_LISTING_INTERVAL_MS)
        );

        // An expired backoff (now >= until) is eligible again.
        let mut expired = visible_day(day(2021, 1, 7));
        expired.backoff_until_ms = Some(now_ms);
        let a = decide_visible_listing(&[expired], today, now_ms);
        assert_eq!(a.fetch, Some(day(2021, 1, 7)));
    }

    #[wasm_bindgen_test]
    fn visible_listing_nothing_eligible_leaves_the_deadline_alone() {
        let today = day(2021, 1, 10);
        let mut fresh = visible_day(day(2021, 1, 5));
        fresh.fresh = true;
        let future = visible_day(day(2021, 1, 11));
        let a = decide_visible_listing(&[fresh, future], today, 10_000.0);
        assert_eq!(a, VisibleListingActions::default());
    }

    // ── plan_selection_fetch ────────────────────────────────────────────────

    fn selection_env(target: Option<((f64, f64), f64)>) -> SelectionFetchEnv {
        SelectionFetchEnv {
            has_worker: true,
            queue_paused: false,
            target,
            auto_fetch_cap_reached: false,
            now_secs: 1000.0,
            deadline_secs: 30.0,
            elevation_filter: Some(1),
            today: day(2021, 1, 5),
        }
    }

    #[wasm_bindgen_test]
    fn selection_skips_when_gated_or_unarmed() {
        // Nothing armed.
        assert_eq!(
            plan_selection_fetch(&selection_env(None)),
            SelectionFetchPlan::Skip
        );
        // No worker / paused queue skip even when armed.
        let armed = Some(((0.0, 3600.0), 1000.0));
        let mut env = selection_env(armed);
        env.has_worker = false;
        assert_eq!(plan_selection_fetch(&env), SelectionFetchPlan::Skip);
        let mut env = selection_env(armed);
        env.queue_paused = true;
        assert_eq!(plan_selection_fetch(&env), SelectionFetchPlan::Skip);
    }

    #[wasm_bindgen_test]
    fn selection_cap_disarms_with_the_exact_status() {
        let mut env = selection_env(Some(((0.0, 3600.0), 1000.0)));
        env.auto_fetch_cap_reached = true;
        assert_eq!(
            plan_selection_fetch(&env),
            SelectionFetchPlan::Disarm {
                status_message: Some(
                    "Auto-fetch limit reached — selected range not fully downloaded".to_string()
                )
            }
        );
    }

    #[wasm_bindgen_test]
    fn selection_degenerate_span_disarms_silently() {
        let env = selection_env(Some(((500.0, 500.5), 1000.0)));
        assert_eq!(
            plan_selection_fetch(&env),
            SelectionFetchPlan::Disarm {
                status_message: None
            }
        );
        // A span of exactly 1 s is NOT degenerate (strict < 1.0) → it plans.
        let env = selection_env(Some((((JAN1) as f64, (JAN1 + 1) as f64), 1000.0)));
        assert!(matches!(
            plan_selection_fetch(&env),
            SelectionFetchPlan::Window(_)
        ));
    }

    #[wasm_bindgen_test]
    fn selection_deadline_disarms_with_the_exact_status() {
        // now (1000) > armed (960) + deadline (30) → stuck-listing backstop.
        let env = selection_env(Some(((JAN1 as f64, (JAN1 + 3600) as f64), 960.0)));
        assert_eq!(
            plan_selection_fetch(&env),
            SelectionFetchPlan::Disarm {
                status_message: Some(
                    "Couldn't list part of the selected range — download may be incomplete"
                        .to_string()
                )
            }
        );
        // Exactly at the deadline (now == armed + deadline) still runs.
        let env = selection_env(Some(((JAN1 as f64, (JAN1 + 3600) as f64), 970.0)));
        assert!(matches!(
            plan_selection_fetch(&env),
            SelectionFetchPlan::Window(_)
        ));
    }

    #[wasm_bindgen_test]
    fn selection_window_walks_every_day_with_the_today_gate() {
        // A 2.5-day selection enumerates interior days (dates_in_range, not
        // endpoint sampling) and carries the today gate for missing listings.
        let start = (JAN1 + 100) as f64;
        let end = (JAN1 + 2 * 86_400 + 200) as f64;
        let env = selection_env(Some(((start, end), 1000.0)));
        let plan = match plan_selection_fetch(&env) {
            SelectionFetchPlan::Window(w) => w,
            other => panic!("expected Window, got {other:?}"),
        };
        assert_eq!(
            plan.dates,
            vec![day(2021, 1, 1), day(2021, 1, 2), day(2021, 1, 3)]
        );
        assert_eq!(plan.intersect_start, JAN1 + 100);
        assert_eq!(plan.win_end, JAN1 + 2 * 86_400 + 200);
        assert_eq!(plan.anchor_at_or_before, None);
        assert_eq!(plan.elevation_filter, Some(1));
        assert_eq!(plan.skip_missing_after, Some(day(2021, 1, 5)));
    }

    // ── lookback backfill ───────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn lookback_due_gates_then_throttles_at_one_hz() {
        let mut next_ms = 0.0;

        // Each gate blocks without touching the throttle deadline.
        assert!(!lookback_backfill_due(
            false,
            true,
            false,
            false,
            5000.0,
            &mut next_ms
        ));
        assert!(!lookback_backfill_due(
            true,
            false,
            false,
            false,
            5000.0,
            &mut next_ms
        ));
        assert!(!lookback_backfill_due(
            true,
            true,
            true,
            false,
            5000.0,
            &mut next_ms
        ));
        assert!(!lookback_backfill_due(
            true,
            true,
            false,
            true,
            5000.0,
            &mut next_ms
        ));
        assert_eq!(next_ms, 0.0);

        // Passing advances the deadline 1 s…
        assert!(lookback_backfill_due(
            true,
            true,
            false,
            false,
            5000.0,
            &mut next_ms
        ));
        assert_eq!(next_ms, 6000.0);
        // …which throttles the next 999 ms…
        assert!(!lookback_backfill_due(
            true,
            true,
            false,
            false,
            5999.0,
            &mut next_ms
        ));
        assert_eq!(next_ms, 6000.0);
        // …and reopens exactly at it.
        assert!(lookback_backfill_due(
            true,
            true,
            false,
            false,
            6000.0,
            &mut next_ms
        ));
        assert_eq!(next_ms, 7000.0);
    }

    #[wasm_bindgen_test]
    fn lookback_plan_widens_a_collapsed_frame_count_window() {
        // Frame-count loop before its frames are cached: the resolved span
        // collapses to near-zero, so the start widens by the basis fallback.
        let now = (JAN1 + 50_000) as f64;
        let basis = LoopBasis::FrameCount(6);
        let plan = plan_lookback_backfill(now - 10.0, now, basis, Some(1));
        let expected_start = (now - basis.fallback_span_secs()) as i64;
        assert_eq!(plan.intersect_start, expected_start);
        assert_eq!(plan.win_end, now as i64);
        assert_eq!(plan.anchor_at_or_before, None);
        assert_eq!(plan.elevation_filter, Some(1));
        assert_eq!(plan.skip_missing_after, None);
        assert_eq!(plan.dates, dates_spanning(expected_start, now as i64));
    }

    #[wasm_bindgen_test]
    fn lookback_plan_keeps_an_earlier_resolved_start() {
        // A resolved window already wider than the fallback is kept as-is.
        let now = (JAN1 + 50_000) as f64;
        let basis = LoopBasis::Duration(1800.0);
        let resolved_start = now - 5000.0; // wider than the 1800 s fallback
        let plan = plan_lookback_backfill(resolved_start, now, basis, None);
        assert_eq!(plan.intersect_start, resolved_start as i64);
        assert_eq!(plan.win_end, now as i64);
        assert_eq!(plan.elevation_filter, None);
    }
}
