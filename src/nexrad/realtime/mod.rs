//! Real-time NEXRAD streaming channel.
//!
//! Provides a channel-based interface for real-time NEXRAD data streaming
//! from AWS. The public surface is [`RealtimeChannel`]; the actual polling
//! loop and its helpers live in [`streaming`].
//!
//! ## Cross-thread coordination
//!
//! Three typed [`futures_channel::mpsc`] queues + one shared `Cell<bool>`
//! carry all traffic between the UI thread and the async `streaming_loop`:
//!
//! - **results** (loop → UI): every [`RealtimeResult`] the loop produces.
//! - **observations** (UI → loop): projector hints
//!   ([`super::ProjectorObservation`]) the UI gathers from worker results
//!   and forwards via [`RealtimeChannel::observe`].
//! - **control** (UI → loop): stop signal + filter changes, drained on
//!   every iteration and inside the sleep loop so a filter swap doesn't
//!   have to wait for the current chunk to arrive.
//! - **active flag** (`Rc<Cell<bool>>`): set true by `start()`, flipped
//!   false by the loop on exit. The UI reads it via [`is_active`](Self::is_active).
//!
//! All three channel pairs are replaced on every `start()` so messages
//! from a still-winding-down previous loop don't leak into the new
//! session. The loop maintains the previously-shared filter/epoch state
//! as local variables; nothing about per-stream coordination requires
//! `Rc<RefCell<_>>` anymore.

use super::download::NetworkStats;
use super::streaming_filter::StreamingFilter;
use crate::data::facade::DataFacade;
use eframe::egui;
use futures_channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use std::cell::{Cell, RefCell};
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

/// Control messages flowing from the UI thread into the streaming loop.
///
/// The loop drains the control channel on every iteration and inside its
/// sleep loop, so a filter change or stop signal interrupts a long wait
/// without polling shared state.
pub(super) enum ControlMessage {
    /// Tear down the loop; the UI clears the active flag separately.
    Stop,
    /// Re-target chunk filtering. The loop replaces its `active_filter`
    /// only when the value actually changes (mirrors the old
    /// `pending_filter == filter` no-op check).
    SetFilter(StreamingFilter),
}

/// Channel for real-time NEXRAD streaming.
///
/// Three typed [`futures_channel::mpsc`] queues carry messages across
/// the thread boundary:
/// - `results` (loop → UI): every [`RealtimeResult`] the loop produces.
/// - `observations` (UI → loop): projector hints (collection-end times,
///   availability lags) the UI gathers from worker results.
/// - `control` (UI → loop): stop + filter-change messages.
///
/// All three channel pairs are replaced on every [`start`](Self::start)
/// so messages from a previously-running loop don't leak across sessions.
/// `active` is a `Cell<bool>` set by `start()` and cleared by the loop on
/// exit; the UI reads it via [`is_active`](Self::is_active).
pub struct RealtimeChannel {
    active: Rc<Cell<bool>>,
    /// Results channel pair. Refilled by `start()`; reads via
    /// `try_recv` borrow the receiver mutably through the `RefCell`.
    results: RefCell<ResultsChannel>,
    /// Observations channel pair. The sender is cloned out to the UI for
    /// every [`observe`](Self::observe) call; the receiver is taken by
    /// the streaming loop when `start()` is called.
    observations: RefCell<ObservationsChannel>,
    /// Control channel pair. The sender is held by `RealtimeChannel`;
    /// the receiver is taken by the streaming loop when `start()` is
    /// called.
    control: RefCell<ControlChannel>,
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

struct ObservationsChannel {
    tx: UnboundedSender<super::ProjectorObservation>,
    /// `Option` so `start()` can `take()` the receiver and hand it to
    /// the streaming loop; reset to `Some` on every fresh channel pair.
    rx: Option<UnboundedReceiver<super::ProjectorObservation>>,
}

impl ObservationsChannel {
    fn new() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx: Some(rx) }
    }
}

impl Default for ObservationsChannel {
    fn default() -> Self {
        Self::new()
    }
}

struct ControlChannel {
    tx: UnboundedSender<ControlMessage>,
    /// `Option` so `start()` can `take()` the receiver and hand it to
    /// the streaming loop; reset to `Some` on every fresh channel pair.
    rx: Option<UnboundedReceiver<ControlMessage>>,
}

impl ControlChannel {
    fn new() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx: Some(rx) }
    }
}

impl Default for ControlChannel {
    fn default() -> Self {
        Self::new()
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
            active: Rc::new(Cell::new(false)),
            results: RefCell::new(ResultsChannel::new()),
            observations: RefCell::new(ObservationsChannel::new()),
            control: RefCell::new(ControlChannel::new()),
            stats: NetworkStats::new(),
        }
    }

    pub fn with_stats(stats: NetworkStats) -> Self {
        Self {
            active: Rc::new(Cell::new(false)),
            results: RefCell::new(ResultsChannel::new()),
            observations: RefCell::new(ObservationsChannel::new()),
            control: RefCell::new(ControlChannel::new()),
            stats,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.get()
    }

    pub fn start(&self, ctx: egui::Context, site_id: String, facade: DataFacade) {
        self.active.set(true);

        // Replace all channel pairs on every start so any in-flight
        // sends from a still-winding-down previous loop hit a dropped
        // receiver and disappear, rather than leaking into the new
        // session.
        *self.results.borrow_mut() = ResultsChannel::new();
        *self.observations.borrow_mut() = ObservationsChannel::new();
        *self.control.borrow_mut() = ControlChannel::new();
        let results_tx = self.results.borrow().tx.clone();
        // `take()` is safe for both because we just installed fresh
        // pairs above — rx is always Some right after construction.
        let observations_rx = self
            .observations
            .borrow_mut()
            .rx
            .take()
            .expect("freshly-built ObservationsChannel always has rx");
        let control_rx = self
            .control
            .borrow_mut()
            .rx
            .take()
            .expect("freshly-built ControlChannel always has rx");

        let active = self.active.clone();
        let stats = self.stats.clone();

        wasm_bindgen_futures::spawn_local(async move {
            streaming_loop(
                ctx,
                site_id,
                active,
                stats,
                facade,
                results_tx,
                observations_rx,
                control_rx,
            )
            .await;
        });
    }

    pub fn stop(&self) {
        // Send a Stop message; the loop will exit cleanly the next time
        // it drains the control channel. Also clear `active` eagerly so
        // `is_active()` reflects the user's intent immediately — the
        // loop's own end-of-life clear is a defense-in-depth backstop.
        let _ = self
            .control
            .borrow()
            .tx
            .unbounded_send(ControlMessage::Stop);
        self.active.set(false);
    }

    pub fn try_recv(&self) -> Option<RealtimeResult> {
        self.results.borrow_mut().rx.try_recv().ok()
    }

    /// Enqueue a projector observation to be applied on the next
    /// streaming-loop iteration. Adding new observation kinds is purely
    /// a matter of extending [`super::ProjectorObservation`] and the
    /// drain dispatch — no new state field or new channel method needed.
    pub fn observe(&self, observation: super::ProjectorObservation) {
        // Drop the result silently; if the loop has finished and
        // closed the receiver, late observations have no consumer.
        let _ = self.observations.borrow().tx.unbounded_send(observation);
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

    /// Update the active streaming filter. The loop's de-dupe check
    /// drops the message if the value didn't actually change, so calling
    /// this every frame from the UI is cheap.
    pub fn set_filter(&self, filter: StreamingFilter) {
        let _ = self
            .control
            .borrow()
            .tx
            .unbounded_send(ControlMessage::SetFilter(filter));
    }

    /// Push the user's elevation selection down as a [`StreamingFilter`].
    /// Called once per frame from the UI; the loop's de-dupe makes this
    /// cheap.
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
