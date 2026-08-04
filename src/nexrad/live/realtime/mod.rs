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
//! - **observations** (UI → loop): projection hints
//!   ([`ProjectorObservation`]) the UI gathers from worker results
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

use crate::core::StreamingFilter;
use crate::data::facade::MainThreadStore;
use crate::nexrad::acquisition::download::NetworkStats;
use eframe::egui;
use futures_channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

mod streaming;
use streaming::streaming_loop;

impl From<&crate::core::ElevationSelection> for StreamingFilter {
    fn from(selection: &crate::core::ElevationSelection) -> Self {
        match selection {
            crate::core::ElevationSelection::Latest => StreamingFilter::All,
            crate::core::ElevationSelection::Fixed {
                elevation_number, ..
            } => StreamingFilter::Elevation(*elevation_number),
        }
    }
}

/// Result type for realtime streaming events.
///
/// `ChunkReceived` is significantly larger than the other variants because it
/// carries the per-chunk diagnostic bundle (`arrival_stat`) used by the VCP
/// forecast modal. Boxing it would add an allocation per chunk for no gain —
/// these values are produced and consumed within the same frame.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum RealtimeResult {
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
        plan: Option<crate::core::StreamingPlan>,
        /// Arrival diagnostics (empty-poll counts, predicted vs. actual time).
        /// `None` on synthetic emissions such as the resume-from-cache path.
        arrival_stat: Option<crate::core::ChunkArrivalStat>,
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

/// Single-source-of-truth input vocabulary for the projection engine's
/// observation channel.
///
/// Observations originate on the main thread (worker ingest results, UI
/// signals) and are enqueued via [`RealtimeChannel::observe`] for the
/// streaming loop to drain and apply to the shared projection engine. Adding
/// a new observation kind is one enum variant + one match arm in the drain
/// dispatch — no new pending-field-on-state and no new method on the
/// channel needed.
///
/// Filter changes are NOT a `ProjectorObservation`: they have additional
/// sleep-interruption semantics (`filter_epoch`) and a separate
/// re-entry path in the loop, so they keep their own dedicated channel.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ProjectorObservation {
    /// ACTUAL category: collection-end time of the most recently
    /// ingested chunk (Unix seconds, sub-second precision). Anchors
    /// projected COLLECTION times for future chunks.
    CollectionEndSecs(f64),
    /// Empirical S3 upload − ACTUAL chunk collection time (seconds) for
    /// the chunk just ingested. Folded into `ChunkTimingStats` so future
    /// projections use a median lag rather than a default.
    AvailabilityLagSecs(f64),
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
pub(crate) struct RealtimeChannel {
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
    tx: UnboundedSender<ProjectorObservation>,
    /// `Option` so `start()` can `take()` the receiver and hand it to
    /// the streaming loop; reset to `Some` on every fresh channel pair.
    rx: Option<UnboundedReceiver<ProjectorObservation>>,
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
    pub(crate) fn new() -> Self {
        Self {
            active: Rc::new(Cell::new(false)),
            results: RefCell::new(ResultsChannel::new()),
            observations: RefCell::new(ObservationsChannel::new()),
            control: RefCell::new(ControlChannel::new()),
            stats: NetworkStats::new(),
        }
    }

    pub(crate) fn with_stats(stats: NetworkStats) -> Self {
        Self {
            active: Rc::new(Cell::new(false)),
            results: RefCell::new(ResultsChannel::new()),
            observations: RefCell::new(ObservationsChannel::new()),
            control: RefCell::new(ControlChannel::new()),
            stats,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.get()
    }

    pub(crate) fn start(
        &self,
        ctx: egui::Context,
        site_id: String,
        facade: MainThreadStore,
        engine: crate::core::projection::SharedProjectionEngine,
    ) {
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
                engine,
            )
            .await;
        });
    }

    pub(crate) fn stop(&self) {
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

    pub(crate) fn try_recv(&self) -> Option<RealtimeResult> {
        self.results.borrow_mut().rx.try_recv().ok()
    }

    /// Enqueue a projector observation to be applied on the next
    /// streaming-loop iteration. Adding new observation kinds is purely
    /// a matter of extending [`ProjectorObservation`] and the
    /// drain dispatch — no new state field or new channel method needed.
    pub(crate) fn observe(&self, observation: ProjectorObservation) {
        // Drop the result silently; if the loop has finished and
        // closed the receiver, late observations have no consumer.
        let _ = self.observations.borrow().tx.unbounded_send(observation);
    }

    /// Push the latest radial collection time (Unix seconds) parsed from
    /// the chunk that was just ingested. Convenience wrapper over
    /// [`Self::observe`].
    pub(crate) fn record_chunk_collection_end_secs(&self, secs: f64) {
        self.observe(ProjectorObservation::CollectionEndSecs(secs));
    }

    /// Push an empirical availability lag (S3 upload − ACTUAL chunk
    /// collection time, seconds) for the chunk just ingested. Convenience
    /// wrapper over [`Self::observe`].
    pub(crate) fn record_availability_lag_secs(&self, lag_secs: f64) {
        self.observe(ProjectorObservation::AvailabilityLagSecs(lag_secs));
    }

    /// Update the active streaming filter. The loop's de-dupe check
    /// drops the message if the value didn't actually change, so calling
    /// this every frame from the UI is cheap.
    pub(crate) fn set_filter(&self, filter: StreamingFilter) {
        let _ = self
            .control
            .borrow()
            .tx
            .unbounded_send(ControlMessage::SetFilter(filter));
    }

    /// Push the user's elevation selection down as a [`StreamingFilter`].
    /// Called once per frame from the UI; the loop's de-dupe makes this
    /// cheap.
    pub(crate) fn sync_filter(&self, selection: &crate::core::ElevationSelection) {
        self.set_filter(StreamingFilter::from(selection));
    }

    /// Drain every pending result.
    pub(crate) fn poll(&self) -> Vec<RealtimeResult> {
        let mut out = Vec::new();
        while let Some(result) = self.try_recv() {
            out.push(result);
        }
        out
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // --- From<&ElevationSelection> for StreamingFilter ---

    #[wasm_bindgen_test]
    fn filter_from_latest_is_all() {
        let sel = crate::core::ElevationSelection::Latest;
        let filter = StreamingFilter::from(&sel);
        assert_eq!(filter, StreamingFilter::All);
    }

    #[wasm_bindgen_test]
    fn filter_from_fixed_maps_to_elevation_number() {
        let sel = crate::core::ElevationSelection::Fixed {
            elevation_number: 3,
            angle: 1.5,
        };
        let filter = StreamingFilter::from(&sel);
        assert_eq!(filter, StreamingFilter::Elevation(3));
    }

    #[wasm_bindgen_test]
    fn filter_from_fixed_preserves_distinct_numbers() {
        let sel1 = crate::core::ElevationSelection::Fixed {
            elevation_number: 1,
            angle: 0.5,
        };
        let sel7 = crate::core::ElevationSelection::Fixed {
            elevation_number: 7,
            angle: 4.0,
        };
        assert_eq!(StreamingFilter::from(&sel1), StreamingFilter::Elevation(1));
        assert_eq!(StreamingFilter::from(&sel7), StreamingFilter::Elevation(7));
        assert_ne!(StreamingFilter::from(&sel1), StreamingFilter::from(&sel7));
    }

    #[wasm_bindgen_test]
    fn filter_from_default_selection_is_elevation_one() {
        // ElevationSelection::default() is Fixed { 1, 0.5 }.
        let sel = crate::core::ElevationSelection::default();
        assert_eq!(StreamingFilter::from(&sel), StreamingFilter::Elevation(1));
    }

    // --- RealtimeChannel construction / is_active ---

    #[wasm_bindgen_test]
    fn new_channel_is_inactive() {
        let ch = RealtimeChannel::new();
        assert!(!ch.is_active());
    }

    #[wasm_bindgen_test]
    fn default_channel_is_inactive() {
        let ch = RealtimeChannel::default();
        assert!(!ch.is_active());
    }

    #[wasm_bindgen_test]
    fn with_stats_channel_is_inactive() {
        let stats = NetworkStats::new();
        let ch = RealtimeChannel::with_stats(stats);
        assert!(!ch.is_active());
    }

    // --- try_recv / poll on a fresh channel ---

    #[wasm_bindgen_test]
    fn try_recv_on_fresh_channel_is_none() {
        let ch = RealtimeChannel::new();
        assert!(ch.try_recv().is_none());
    }

    #[wasm_bindgen_test]
    fn poll_on_fresh_channel_is_empty() {
        let ch = RealtimeChannel::new();
        assert!(ch.poll().is_empty());
    }

    #[wasm_bindgen_test]
    fn poll_is_idempotent_while_empty() {
        let ch = RealtimeChannel::new();
        assert_eq!(ch.poll().len(), 0);
        assert_eq!(ch.poll().len(), 0);
        assert!(ch.try_recv().is_none());
    }

    // --- stop() semantics ---

    #[wasm_bindgen_test]
    fn stop_clears_active_flag() {
        let ch = RealtimeChannel::new();
        // active starts false; stop() should keep/force it false.
        ch.stop();
        assert!(!ch.is_active());
    }

    // --- observation / control sends are non-panicking and side-effect-free
    //     w.r.t. the active flag and the results channel ---

    #[wasm_bindgen_test]
    fn observe_does_not_touch_active_or_results() {
        let ch = RealtimeChannel::new();
        ch.observe(ProjectorObservation::CollectionEndSecs(123.5));
        assert!(!ch.is_active());
        // Observation goes to the observations channel, never to results.
        assert!(ch.try_recv().is_none());
    }

    #[wasm_bindgen_test]
    fn record_helpers_do_not_panic_or_emit_results() {
        let ch = RealtimeChannel::new();
        ch.record_chunk_collection_end_secs(42.0);
        ch.record_availability_lag_secs(2.25);
        assert!(ch.try_recv().is_none());
        assert!(!ch.is_active());
    }

    #[wasm_bindgen_test]
    fn set_and_sync_filter_do_not_emit_results() {
        let ch = RealtimeChannel::new();
        ch.set_filter(StreamingFilter::Elevation(2));
        ch.set_filter(StreamingFilter::All);
        let sel = crate::core::ElevationSelection::Fixed {
            elevation_number: 4,
            angle: 2.4,
        };
        ch.sync_filter(&sel);
        // Filter changes flow over the control channel, not results.
        assert!(ch.try_recv().is_none());
        assert!(!ch.is_active());
    }
}
