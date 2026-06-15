//! Pure acquisition decisions: prefetch policy, the selection-fetch gate, and
//! the UTC-date spans the prefetch/listing pumps enumerate.
//!
//! The pumps in `app::acquisition_intent` interleave these decisions with I/O
//! (listing fetches, queue enqueues). The *deciding* is pure and lives here so
//! the policy gates and date math are unit-tested without a worker, a queue, or a
//! browser; the shell pumps call them and perform the I/O. The download queue's
//! own state machine (`nexrad::download_queue`) is already pure + tested and
//! stays where it is.

use chrono::NaiveDate;

/// Whether the playhead-driven reactive prefetch (settled window + anchor
/// fast-path) may run this frame. Suppressed while the playhead is attached to
/// the live edge (the stream owns acquisition there), while the queue is
/// manually paused, or when the data-saver `autofetch_while_scrubbing` policy is
/// off.
pub fn reactive_prefetch_allowed(
    playhead_attached: bool,
    queue_paused: bool,
    autofetch_while_scrubbing: bool,
) -> bool {
    !playhead_attached && !queue_paused && autofetch_while_scrubbing
}

/// How a just-finalized timeline selection should be fetched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionGate {
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
pub fn decide_selection_gate(start: f64, end: f64, confirm_threshold: f64) -> SelectionGate {
    if (end - start).abs() <= confirm_threshold {
        SelectionGate::Arm
    } else {
        SelectionGate::Confirm
    }
}

/// The distinct UTC dates a `[start, end]` second-range touches (one, or two
/// across a midnight boundary — the prefetch window is always well under 24h).
pub fn dates_spanning(start_secs: i64, end_secs: i64) -> Vec<NaiveDate> {
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
/// [`dates_spanning`] (which only samples the endpoints), this walks day by day
/// so multi-day visible windows enumerate their interior dates too.
pub fn dates_in_range(start_secs: i64, end_secs: i64) -> Vec<NaiveDate> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── reactive_prefetch_allowed ──

    #[wasm_bindgen_test]
    fn prefetch_runs_when_free_and_autofetch_on() {
        assert!(reactive_prefetch_allowed(false, false, true));
    }

    #[wasm_bindgen_test]
    fn prefetch_suppressed_when_autofetch_off() {
        assert!(!reactive_prefetch_allowed(false, false, false));
    }

    #[wasm_bindgen_test]
    fn prefetch_suppressed_when_attached_or_paused() {
        assert!(!reactive_prefetch_allowed(true, false, true));
        assert!(!reactive_prefetch_allowed(false, true, true));
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
