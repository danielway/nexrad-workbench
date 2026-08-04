//! Loop-local coordination state for [`super::streaming_loop`].
//!
//! Holds the mirror of the UI-driven control state ([`LoopState`] +
//! [`drain_control`]), the sleep primitive that watches for stop /
//! filter-change signals ([`interruptible_sleep`] + [`SleepOutcome`]), and the
//! network-stats delta tracker ([`StatsTracker`]). Nothing here touches the
//! network or the projection engine — this is the loop's own bookkeeping.

use crate::core::StreamingFilter;
use crate::net::retry::sleep_ms;
use crate::nexrad::acquisition::download::NetworkStats;
use crate::nexrad::live::realtime::ControlMessage;
use crate::nexrad::live::streaming_state::StreamingState;
use eframe::egui;
use futures_channel::mpsc::UnboundedReceiver;

/// Loop-local mirror of the coordination state that used to live on
/// `RealtimeState`. Updated by [`drain_control`] from incoming
/// [`ControlMessage`]s.
pub(super) struct LoopState {
    /// Set true when the UI sends [`ControlMessage::Stop`]; the loop
    /// checks this at every iteration / sleep boundary.
    pub(super) stop_requested: bool,
    /// Currently applied chunk filter. Bumped by [`ControlMessage::SetFilter`]
    /// when the value actually changes.
    pub(super) active_filter: StreamingFilter,
    /// Local counter that increments every time `active_filter` changes.
    /// Used by [`interruptible_sleep`] to signal a sleep-aborting filter
    /// swap to the main loop.
    pub(super) filter_epoch: u64,
}

impl LoopState {
    pub(super) fn new() -> Self {
        Self {
            stop_requested: false,
            active_filter: StreamingFilter::All,
            filter_epoch: 0,
        }
    }
}

/// Drain every pending control message into `loop_state`. Returns
/// `true` if `active_filter` changed.
pub(super) fn drain_control(
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
) -> bool {
    let mut filter_changed = false;
    while let Ok(msg) = control_rx.try_recv() {
        match msg {
            ControlMessage::Stop => loop_state.stop_requested = true,
            ControlMessage::SetFilter(new_filter) => {
                if loop_state.active_filter != new_filter {
                    loop_state.active_filter = new_filter;
                    loop_state.filter_epoch = loop_state.filter_epoch.wrapping_add(1);
                    filter_changed = true;
                }
            }
        }
    }
    filter_changed
}

/// Outcome of `interruptible_sleep`. `Stopped` means the user requested stop;
/// `FilterChanged` means the active filter changed mid-sleep so the caller
/// should re-evaluate before continuing; `Completed` is the normal "slept the
/// full duration" path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SleepOutcome {
    Completed,
    Stopped,
    FilterChanged,
}

/// Sleep in increments, draining the control channel and watching for
/// stop + filter-change signals between increments. Returns the reason
/// the sleep ended.
///
/// `wake_epoch` is the `filter_epoch` value the caller observed when it
/// decided how long to sleep — if `drain_control` bumps it past that
/// value mid-sleep, the filter has been mutated and the caller should
/// re-evaluate.
pub(super) async fn interruptible_sleep(
    loop_state: &mut LoopState,
    control_rx: &mut UnboundedReceiver<ControlMessage>,
    ctx: &egui::Context,
    total_ms: u32,
    wake_epoch: u64,
) -> SleepOutcome {
    const INCREMENT: u32 = 250;
    let mut remaining = total_ms;

    while remaining > 0 {
        drain_control(loop_state, control_rx);
        if loop_state.stop_requested {
            return SleepOutcome::Stopped;
        }
        if loop_state.filter_epoch != wake_epoch {
            return SleepOutcome::FilterChanged;
        }

        ctx.request_repaint();

        let sleep_time = INCREMENT.min(remaining);
        sleep_ms(sleep_time).await;
        remaining = remaining.saturating_sub(INCREMENT);
    }

    drain_control(loop_state, control_rx);
    if loop_state.stop_requested {
        SleepOutcome::Stopped
    } else if loop_state.filter_epoch != wake_epoch {
        SleepOutcome::FilterChanged
    } else {
        SleepOutcome::Completed
    }
}

pub(super) struct StatsTracker {
    last_requests: usize,
    last_bytes: u64,
}

impl StatsTracker {
    pub(super) fn new(state: &StreamingState) -> Self {
        Self {
            last_requests: state.requests_made(),
            last_bytes: state.bytes_downloaded(),
        }
    }

    pub(super) fn update(&mut self, stats: &NetworkStats, state: &StreamingState) {
        let new_requests = state.requests_made().saturating_sub(self.last_requests);
        let new_bytes = state.bytes_downloaded().saturating_sub(self.last_bytes);

        for _ in 0..new_requests {
            stats.request_started();
            stats.request_completed(0);
        }
        if new_bytes > 0 {
            *stats.total_bytes.borrow_mut() += new_bytes;
        }

        self.last_requests = state.requests_made();
        self.last_bytes = state.bytes_downloaded();
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── LoopState / drain_control state machine ────────────────────────────
    //
    // `drain_control` is the loop's control-channel state machine: it folds
    // every queued `ControlMessage` into a `LoopState` and reports whether the
    // active filter changed. The existing `mod tests` covers only the pure
    // probe/cache helpers, never this. We drive it through an in-process
    // unbounded channel (no async, no browser): keeping the sender alive means
    // `try_recv` returns `Err(Empty)` on drain-out, exactly the production path.

    #[wasm_bindgen_test]
    fn loop_state_starts_unstopped_all_filter() {
        let ls = LoopState::new();
        assert!(!ls.stop_requested);
        assert!(ls.active_filter == StreamingFilter::All);
        assert_eq!(ls.filter_epoch, 0);
    }

    #[wasm_bindgen_test]
    fn drain_control_empty_channel_is_noop() {
        let (_tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(!changed);
        assert!(!ls.stop_requested);
        assert!(ls.active_filter == StreamingFilter::All);
        assert_eq!(ls.filter_epoch, 0);
    }

    #[wasm_bindgen_test]
    fn drain_control_stop_sets_flag_without_filter_change() {
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::Stop);
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        // Stop is not a filter change.
        assert!(!changed);
        assert!(ls.stop_requested);
        // Filter + epoch untouched by a Stop.
        assert!(ls.active_filter == StreamingFilter::All);
        assert_eq!(ls.filter_epoch, 0);
    }

    #[wasm_bindgen_test]
    fn drain_control_filter_change_bumps_epoch_and_reports_change() {
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(3)));
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.active_filter == StreamingFilter::Elevation(3));
        // One real change → epoch 0 → 1.
        assert_eq!(ls.filter_epoch, 1);
        assert!(!ls.stop_requested);
    }

    #[wasm_bindgen_test]
    fn drain_control_redundant_filter_is_noop() {
        // Setting the filter to its current value must NOT bump the epoch or
        // report a change (mirrors the old `pending_filter == filter` guard).
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::All));
        let mut ls = LoopState::new(); // already All
        let changed = drain_control(&mut ls, &mut rx);
        assert!(!changed);
        assert_eq!(ls.filter_epoch, 0);
        assert!(ls.active_filter == StreamingFilter::All);
    }

    #[wasm_bindgen_test]
    fn drain_control_coalesces_multiple_distinct_changes() {
        // Two distinct changes queued before a single drain → both applied,
        // epoch counts each real transition, final value is the last one.
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(1)));
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(2)));
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.active_filter == StreamingFilter::Elevation(2));
        // 0 → 1 (to Elevation(1)) → 2 (to Elevation(2)).
        assert_eq!(ls.filter_epoch, 2);
    }

    #[wasm_bindgen_test]
    fn drain_control_duplicate_change_only_bumps_once() {
        // Same target sent twice: first transition counts, the second is a
        // no-op against the now-current value.
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(5)));
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(5)));
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.active_filter == StreamingFilter::Elevation(5));
        assert_eq!(ls.filter_epoch, 1);
    }

    #[wasm_bindgen_test]
    fn drain_control_change_back_to_all_is_a_real_change() {
        // Starting from Elevation, a SetFilter(All) is a genuine transition.
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::All));
        let mut ls = LoopState::new();
        ls.active_filter = StreamingFilter::Elevation(4);
        // Pretend we'd already advanced the epoch once before this drain.
        ls.filter_epoch = 1;
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.active_filter == StreamingFilter::All);
        assert_eq!(ls.filter_epoch, 2);
    }

    #[wasm_bindgen_test]
    fn drain_control_applies_stop_and_filter_together() {
        // A filter change AND a stop queued in the same drain: both land. The
        // change is reported and the stop flag is set.
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<ControlMessage>();
        let _ = tx.unbounded_send(ControlMessage::SetFilter(StreamingFilter::Elevation(7)));
        let _ = tx.unbounded_send(ControlMessage::Stop);
        let mut ls = LoopState::new();
        let changed = drain_control(&mut ls, &mut rx);
        assert!(changed);
        assert!(ls.stop_requested);
        assert!(ls.active_filter == StreamingFilter::Elevation(7));
        assert_eq!(ls.filter_epoch, 1);
    }
}
