//! Real-time NEXRAD streaming channel.
//!
//! Provides a channel-based interface for real-time NEXRAD data streaming
//! from AWS. The public surface is [`RealtimeChannel`]; the actual polling
//! loop and its helpers live in [`streaming`].
//!
//! ## Internal state machine
//!
//! Streaming-loop output flows over a typed [`futures_channel::mpsc`]
//! results channel (loop ⇒ UI). Two coordination fields still live on
//! [`RealtimeState`] behind `Rc<RefCell<_>>` because the loop polls them
//! synchronously inside `interruptible_sleep`:
//!
//! - `stop_requested`: set by `stop()`; the loop checks it on each iteration
//!   and at every sleep break, exiting cleanly when set.
//! - `pending_observations`: drained by the loop each iteration and applied
//!   to the projector. New observations can be enqueued from the UI thread
//!   via `observe()` without ordering constraints.
//! - `filter_epoch`: bumped by `set_filter()` on every change. A sleeping
//!   loop polls the epoch every ~250 ms (`interruptible_sleep`) and wakes
//!   to re-target when it changes, so filter swaps don't have to wait for
//!   the current chunk to arrive.
//!
//! Migrating the remaining three fields to typed control channels is the
//! second slice of S2; gating it on a real select-with-timeout primitive
//! (the loop's `interruptible_sleep` would become a `select!`).

use super::download::NetworkStats;
use super::streaming_filter::StreamingFilter;
use crate::data::facade::DataFacade;
use eframe::egui;
use futures_channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use std::cell::RefCell;
use std::rc::Rc;

mod streaming;
use streaming::streaming_loop;

impl From<&crate::state::ElevationSelection> for StreamingFilter {
    fn from(selection: &crate::state::ElevationSelection) -> Self {
        match selection {
            crate::state::ElevationSelection::Latest => StreamingFilter::All,
            crate::state::ElevationSelection::Fixed {
                elevation_number, ..
            } => StreamingFilter::Elevation(*elevation_number),
        }
    }
}

/// Projected timing & diagnostics for a single future chunk.
///
/// Present iff the parent [`ChunkProjectionInfo`] describes a chunk that
/// hasn't been observed yet — past chunks have `forecast: None`. Carries
/// the diagnostic bundle the projector computed for this chunk so
/// downstream surfaces (per-sweep confidence display, prediction-error
/// attribution, the diagnostics modal) can read attribution data per chunk
/// rather than only for the immediate next download target.
#[derive(Clone, Debug)]
pub struct ChunkForecast {
    /// COLLECTION category: projected Unix-seconds time the radar physically
    /// emits/receives for this chunk.
    pub collection_time_secs: f64,
    /// AVAILABILITY category: projected Unix-seconds time this chunk
    /// becomes available in S3 (`collection_at + lag`).
    pub available_at_secs: f64,
    /// POLL category: projected time the scheduler will fire its first
    /// download poll (`available_at + retry_budget + POLL_BIAS`). The
    /// retry budget is already folded in; the streaming loop sleeps
    /// directly to this target without additional padding.
    pub poll_at_secs: f64,
    /// Physics decomposition for the hop into this chunk (azimuth gap,
    /// inter-sweep transition, inter-volume gap).
    pub physics_breakdown: super::timing::PhysicsBreakdown,
    /// Bucket sample count consulted at projection time. `0` when no
    /// historical samples were available.
    pub stats_n: usize,
    /// Which projector branch supplied the interval: `Blended` when
    /// historical samples contributed, `Physics` otherwise.
    pub scheduler_path: super::timing::SchedulerPath,
    /// The bucket key the lookup hit (or missed). `None` when no
    /// elevation was resolvable (Start chunk).
    pub bucket: Option<super::timing::ChunkCharacteristics>,
}

/// Projected timing and structural info for a single chunk in the volume.
///
/// Combines structural metadata from `ChunkMetadata` (available for every
/// chunk in the volume) with an optional `forecast` ([`ChunkForecast`])
/// that's present iff the chunk is in the future from the streaming loop's
/// anchor. Past chunks carry only structural fields.
#[derive(Clone, Debug)]
pub struct ChunkProjectionInfo {
    /// 1-based sequence number in the volume.
    pub sequence: usize,
    /// Elevation number (1-based), None for the Start chunk.
    pub elevation_number: Option<usize>,
    /// Azimuth rotation rate in degrees/second from the VCP.
    pub azimuth_rate_dps: f64,
    /// 0-based index of this chunk within its sweep.
    pub chunk_index_in_sweep: usize,
    /// Total chunks in this sweep (3 for standard, 6 for super-res).
    pub chunks_in_sweep: usize,
    /// Projected timing & projector diagnostics. `Some` iff this chunk is
    /// in the future (the projector emitted a [`super::timing::ChunkProjection`]
    /// for it); `None` for past chunks.
    pub forecast: Option<ChunkForecast>,
}

/// Result type for realtime streaming events.
///
/// `ChunkReceived` is significantly larger than the other variants because it
/// carries the per-chunk diagnostic bundle (`arrival_stat`) used by the VCP
/// forecast modal. Boxing it would add an allocation per chunk for no gain —
/// these values are produced and consumed within the same frame.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum RealtimeResult {
    /// Iterator initialized, streaming started
    Started { site_id: String },
    /// Chunk received from the stream (UI status update).
    ///
    /// `plan` is the canonical forward-looking projection consumed by the
    /// timeline countdown, the in-progress sweep rendering, the next-scan
    /// ghost, and any caller that wants to know "when does the next chunk
    /// arrive." Replaces the older bag of `time_until_next` +
    /// `projected_volume_end_*` + `chunk_projections` +
    /// `next_volume_chunk_projections` fields that drifted apart and
    /// caused mismatches between the UI countdown and the loop's sleep.
    ChunkReceived {
        chunks_in_volume: u32,
        is_volume_end: bool,
        fetch_latency_ms: f64,
        plan: Option<super::StreamingPlan>,
        /// Arrival diagnostics (empty-poll counts, predicted vs. actual time).
        /// `None` on synthetic emissions such as the resume-from-cache path.
        arrival_stat: Option<crate::state::ChunkArrivalStat>,
    },
    /// Raw chunk data for incremental ingest
    ChunkData {
        data: Vec<u8>,
        chunk_index: u32,
        is_start: bool,
        is_end: bool,
        /// Volume scan start (Unix seconds, sub-second precision). Carries
        /// the provisional value computed in the streaming loop; the
        /// worker uses it as the IDB scan-key timestamp for every chunk
        /// in the volume, so all chunks in one volume must agree on this
        /// value. Sub-second precision matters: the IDB key is built via
        /// `ScanKey::from_secs_f64`, and a truncating `i64` would round
        /// distinct volumes onto the same key when they're within the
        /// same wall-clock second.
        timestamp: f64,
        /// Whether this chunk is the last chunk of its sweep, derived from
        /// the VCP mapper at emission time. The worker accumulator uses this
        /// to flush the in-progress elevation as soon as the last chunk is
        /// ingested rather than waiting for the next elevation's first chunk
        /// — important under filter mode where the next-elevation chunk may
        /// never arrive in this volume. `None` means the projection didn't
        /// resolve (rare; e.g. for the Start chunk).
        is_last_in_sweep: Option<bool>,
    },
    /// Error occurred during streaming
    Error(String),
}

/// Internal state for the realtime streaming channel.
///
/// Shared between the UI thread (via `RealtimeChannel`) and the async
/// `streaming_loop`. Coordinates via the fields documented at the module
/// top: `stop_requested`, `pending_observations`, `pending_filter` /
/// `filter_epoch`. Streaming output flows over a separate typed channel
/// (the loop's `results_tx`) so the UI no longer racially shares the
/// produced-vs-consumed Vec.
#[derive(Default)]
pub(super) struct RealtimeState {
    pub(super) active: bool,
    pub(super) stop_requested: bool,
    /// Projector observations queued from the main thread (worker ingest
    /// results, etc.) for the streaming loop to drain and apply to its
    /// `StreamingState`. See [`super::ProjectorObservation`] for the
    /// vocabulary; adding new observation kinds is mechanical (one
    /// enum variant + one drain match arm).
    pub(super) pending_observations: Vec<super::ProjectorObservation>,
    /// Active filter on the chunk stream. Updated from the UI thread via
    /// `RealtimeChannel::set_filter`; the streaming loop snapshots this on
    /// each iteration and uses it to skip chunks that don't match.
    ///
    /// Filter changes are not routed through `pending_observations`
    /// because they have additional sleep-interruption semantics (see
    /// `filter_epoch` below) and a re-entry path in the loop that other
    /// observations don't need.
    pub(super) pending_filter: StreamingFilter,
    /// Bumped by `set_filter` on every change so a sleeping loop can detect
    /// "the filter just changed" via epoch comparison and wake up to
    /// re-target without polling the filter value itself for equality.
    pub(super) filter_epoch: u64,
}

/// Channel for real-time NEXRAD streaming.
///
/// `results` is a typed [`futures_channel::mpsc`] queue carrying
/// loop-produced events to the UI; the channel pair is replaced on
/// every [`start`](Self::start) so messages from a previously-running
/// loop don't leak into a new session.
pub struct RealtimeChannel {
    state: Rc<RefCell<RealtimeState>>,
    /// Results channel pair. Refilled by `start()`; reads via
    /// `try_recv` borrow the receiver mutably through the `RefCell`.
    results: RefCell<ResultsChannel>,
    stats: NetworkStats,
}

struct ResultsChannel {
    tx: UnboundedSender<RealtimeResult>,
    rx: UnboundedReceiver<RealtimeResult>,
}

impl ResultsChannel {
    fn new() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx }
    }
}

impl Default for RealtimeChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl RealtimeChannel {
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(RealtimeState::default())),
            results: RefCell::new(ResultsChannel::new()),
            stats: NetworkStats::new(),
        }
    }

    pub fn with_stats(stats: NetworkStats) -> Self {
        Self {
            state: Rc::new(RefCell::new(RealtimeState::default())),
            results: RefCell::new(ResultsChannel::new()),
            stats,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state.borrow().active
    }

    pub fn start(&self, ctx: egui::Context, site_id: String, facade: DataFacade) {
        {
            let mut state = self.state.borrow_mut();
            state.active = true;
            state.stop_requested = false;
        }

        // Replace the channel pair on every start so any in-flight
        // sends from a still-winding-down previous loop hit a dropped
        // receiver and disappear, rather than leaking into the new
        // session's result stream.
        *self.results.borrow_mut() = ResultsChannel::new();
        let results_tx = self.results.borrow().tx.clone();

        let state = self.state.clone();
        let stats = self.stats.clone();

        wasm_bindgen_futures::spawn_local(async move {
            streaming_loop(ctx, site_id, state, stats, facade, results_tx).await;
        });
    }

    pub fn stop(&self) {
        let mut state = self.state.borrow_mut();
        state.stop_requested = true;
        state.active = false;
    }

    pub fn try_recv(&self) -> Option<RealtimeResult> {
        self.results.borrow_mut().rx.try_recv().ok()
    }

    /// Enqueue a projector observation to be applied on the next
    /// streaming-loop iteration. Adding new observation kinds is purely
    /// a matter of extending [`super::ProjectorObservation`] and the
    /// drain dispatch — no new state field or new channel method needed.
    pub fn observe(&self, observation: super::ProjectorObservation) {
        self.state
            .borrow_mut()
            .pending_observations
            .push(observation);
    }

    /// Push the latest radial collection time (Unix seconds) parsed from
    /// the chunk that was just ingested. Convenience wrapper over
    /// [`Self::observe`].
    pub fn record_chunk_collection_end_secs(&self, secs: f64) {
        self.observe(super::ProjectorObservation::CollectionEndSecs(secs));
    }

    /// Push an empirical availability lag (S3 upload − ACTUAL chunk
    /// collection time, seconds) for the chunk just ingested. Convenience
    /// wrapper over [`Self::observe`].
    pub fn record_availability_lag_secs(&self, lag_secs: f64) {
        self.observe(super::ProjectorObservation::AvailabilityLagSecs(lag_secs));
    }

    /// Update the active streaming filter. Bumps the filter epoch so a
    /// sleeping `streaming_loop` wakes within ~250 ms and re-targets.
    /// Setting the same value the loop already has is a no-op.
    pub fn set_filter(&self, filter: StreamingFilter) {
        let mut state = self.state.borrow_mut();
        if state.pending_filter == filter {
            return;
        }
        state.pending_filter = filter;
        state.filter_epoch = state.filter_epoch.wrapping_add(1);
    }

    /// Push the user's elevation selection down as a [`StreamingFilter`].
    /// Called once per frame from the UI; the underlying [`Self::set_filter`]
    /// no-ops on equal values, so this is cheap.
    pub fn sync_filter(&self, selection: &crate::state::ElevationSelection) {
        self.set_filter(StreamingFilter::from(selection));
    }

    /// Drain every pending result.
    pub fn poll(&self) -> Vec<RealtimeResult> {
        let mut out = Vec::new();
        while let Some(result) = self.try_recv() {
            out.push(result);
        }
        out
    }
}
