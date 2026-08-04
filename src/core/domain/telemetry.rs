//! Network telemetry records reported by the service worker.
//!
//! Pure data types only — the listener that accumulates them lives in
//! [`crate::subsystem::network_monitor`].

use crate::core::OperationId;
use std::collections::VecDeque;

/// A single completed network request reported by the service worker.
#[derive(Clone, Debug)]
pub(crate) struct NetworkRequest {
    /// Request URL (truncated for display).
    pub url: String,
    /// HTTP status code (0 if the request failed before a response).
    pub status: u16,
    /// Response body size in bytes (from Content-Length).
    pub bytes: u64,
    /// Duration of the request in milliseconds.
    pub duration_ms: f64,
    /// Whether the response was successful (2xx).
    pub ok: bool,
    /// Timestamp when this metric was received (ms since epoch).
    pub timestamp_ms: f64,
    /// Correlated acquisition operation ID (populated by URL matching in main loop).
    pub operation_id: Option<OperationId>,
}

/// Aggregate network statistics for the session.
#[derive(Clone, Debug, Default)]
pub(crate) struct NetworkAggregate {
    /// Total number of requests intercepted.
    pub total_requests: u32,
    /// Number of failed requests (non-ok or network error).
    pub failed_requests: u32,
    /// Total bytes transferred.
    pub total_bytes: u64,
}

/// How far back the throughput readout looks.
pub(crate) const THROUGHPUT_WINDOW_MS: f64 = 10_000.0;

/// Cap on retained samples, so a burst of small requests can't grow the buffer
/// without bound between prunes.
const THROUGHPUT_MAX_SAMPLES: usize = 128;

/// Floor on the divisor when computing a rate.
///
/// Without it, a single 5 MB sample that landed 3 ms ago would report ~1.6 GB/s.
/// Clamping the span to one second makes a fresh single sample read as "at most
/// this many bytes in the last second", which is the honest reading.
const THROUGHPUT_MIN_SPAN_MS: f64 = 1_000.0;

/// One observation of bytes arriving at a point in time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ThroughputSample {
    pub at_ms: f64,
    pub bytes: u64,
}

/// A rolling window of transfer samples, used for the activity surface's
/// bytes-per-second readout.
///
/// The clock is injected on every call — this type never reads `Date::now()`,
/// so it is unit-testable headlessly like the rest of the core.
#[derive(Default, Clone, Debug)]
pub(crate) struct ThroughputWindow {
    samples: VecDeque<ThroughputSample>,
}

impl ThroughputWindow {
    /// Record a sample and prune anything outside the window or over the cap.
    pub(crate) fn push(&mut self, sample: ThroughputSample, now_ms: f64) {
        self.samples.push_back(sample);
        self.prune(now_ms);
    }

    /// Drop samples older than the window, then enforce the sample cap.
    ///
    /// Called from `push`, and also worth calling on an idle frame so the
    /// readout decays to `None` instead of freezing at the last rate.
    pub(crate) fn prune(&mut self, now_ms: f64) {
        while let Some(front) = self.samples.front() {
            if now_ms - front.at_ms > THROUGHPUT_WINDOW_MS {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        while self.samples.len() > THROUGHPUT_MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    /// Bytes per second across the window, or `None` when there is nothing to
    /// measure. Callers must render `None` as "—", never as `0 B/s`: an empty
    /// window means "not transferring", not "transferring at zero".
    #[allow(dead_code)] // Read by the activity view-model; pinned by tests meanwhile.
    pub(crate) fn rate_bytes_per_sec(&self, now_ms: f64) -> Option<f64> {
        let oldest = self.samples.front()?;
        let total: u64 = self.samples.iter().map(|s| s.bytes).sum();
        let span_ms = (now_ms - oldest.at_ms).max(THROUGHPUT_MIN_SPAN_MS);
        Some(total as f64 / (span_ms / 1000.0))
    }

    /// Number of retained samples (shown in the dev detail rows).
    #[allow(dead_code)] // Read by the activity view-model; pinned by tests meanwhile.
    pub(crate) fn len(&self) -> usize {
        self.samples.len()
    }
}

/// Turn a pair of readings from a cumulative byte counter into a sample.
///
/// The fallback throughput source is `NetworkStats::bytes_transferred()`, a
/// session-cumulative counter shared into async tasks via `Rc<RefCell<_>>`.
/// Returns `None` when the counter did not move, and — importantly — also when
/// it went *backwards*, which would otherwise underflow into a nonsense sample.
pub(crate) fn throughput_delta_sample(
    prev_total: u64,
    total: u64,
    now_ms: f64,
) -> Option<ThroughputSample> {
    if total <= prev_total {
        return None;
    }
    Some(ThroughputSample {
        at_ms: now_ms,
        bytes: total - prev_total,
    })
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn sample(at_ms: f64, bytes: u64) -> ThroughputSample {
        ThroughputSample { at_ms, bytes }
    }

    /// An empty window has no rate at all — the UI must show "—", not "0 B/s".
    #[wasm_bindgen_test]
    fn rate_is_none_when_window_empty() {
        let w = ThroughputWindow::default();
        assert_eq!(w.rate_bytes_per_sec(1_000.0), None);
        assert_eq!(w.len(), 0);
    }

    /// The rate divides by elapsed span, not by sample count: four 1 KB samples
    /// spread over 4 s is 1 KB/s, regardless of how many samples that took.
    #[wasm_bindgen_test]
    fn rate_uses_window_span_not_sample_count() {
        let mut w = ThroughputWindow::default();
        for i in 0..4 {
            w.push(sample(i as f64 * 1_000.0, 1_000), 4_000.0);
        }
        // 4000 bytes over a 4 s span.
        assert_eq!(w.rate_bytes_per_sec(4_000.0), Some(1_000.0));
    }

    /// Samples older than the window are pruned as new ones arrive, so a long
    /// idle period after a big transfer decays the readout instead of pinning
    /// it high.
    #[wasm_bindgen_test]
    fn samples_outside_window_are_pruned_on_push() {
        let mut w = ThroughputWindow::default();
        w.push(sample(0.0, 5_000_000), 0.0);
        assert_eq!(w.len(), 1);
        // A sample 11 s later evicts the first (window is 10 s).
        w.push(sample(11_000.0, 1_000), 11_000.0);
        assert_eq!(w.len(), 1);
        assert_eq!(w.rate_bytes_per_sec(11_000.0), Some(1_000.0));
    }

    /// `prune` alone decays an idle window to empty — the readout must go back
    /// to `None` even when no new samples ever arrive.
    #[wasm_bindgen_test]
    fn prune_alone_empties_an_idle_window() {
        let mut w = ThroughputWindow::default();
        w.push(sample(0.0, 1_000), 0.0);
        w.prune(THROUGHPUT_WINDOW_MS + 1.0);
        assert_eq!(w.len(), 0);
        assert_eq!(w.rate_bytes_per_sec(THROUGHPUT_WINDOW_MS + 1.0), None);
    }

    /// A single large sample that just landed must not report an absurd rate:
    /// the span floor clamps the divisor to one second.
    #[wasm_bindgen_test]
    fn span_floor_prevents_a_single_fresh_sample_reporting_absurd_rate() {
        let mut w = ThroughputWindow::default();
        w.push(sample(1_000.0, 5_000_000), 1_003.0);
        // 3 ms of real span would be ~1.6 GB/s; the floor makes it 5 MB/s.
        assert_eq!(w.rate_bytes_per_sec(1_003.0), Some(5_000_000.0));
    }

    /// The sample cap evicts oldest-first even when everything is inside the
    /// time window.
    #[wasm_bindgen_test]
    fn sample_cap_evicts_oldest() {
        let mut w = ThroughputWindow::default();
        for i in 0..(THROUGHPUT_MAX_SAMPLES + 10) {
            // All at the same instant, so time-pruning never fires.
            w.push(sample(0.0, 1), 0.0);
            let _ = i;
        }
        assert_eq!(w.len(), THROUGHPUT_MAX_SAMPLES);
    }

    /// A cumulative counter that hasn't moved yields no sample.
    #[wasm_bindgen_test]
    fn delta_sample_none_when_total_unchanged() {
        assert_eq!(throughput_delta_sample(500, 500, 1.0), None);
    }

    /// A counter that went backwards (reset, or a stale read) yields no sample
    /// rather than underflowing.
    #[wasm_bindgen_test]
    fn delta_sample_none_on_counter_reset() {
        assert_eq!(throughput_delta_sample(900, 100, 1.0), None);
    }

    /// A forward-moving counter yields exactly the delta, stamped now.
    #[wasm_bindgen_test]
    fn delta_sample_reports_the_increment() {
        assert_eq!(
            throughput_delta_sample(100, 350, 42.0),
            Some(ThroughputSample {
                at_ms: 42.0,
                bytes: 250
            })
        );
    }
}
