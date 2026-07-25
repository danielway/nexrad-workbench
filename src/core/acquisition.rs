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
pub(crate) fn reactive_prefetch_allowed(
    playhead_attached: bool,
    queue_paused: bool,
    autofetch_while_scrubbing: bool,
) -> bool {
    !playhead_attached && !queue_paused && autofetch_while_scrubbing
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

/// Every UTC date a `[start, end]` second-range touches, in order. Unlike
/// [`dates_spanning`] (which only samples the endpoints), this walks day by day
/// so multi-day visible windows enumerate their interior dates too.
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
        assert!(!reactive_prefetch_allowed(false, false, false));
    }

    #[wasm_bindgen_test]
    fn prefetch_blocked_when_attached_even_with_everything_else_permissive() {
        // Attached dominates regardless of paused/autofetch combos.
        assert!(!reactive_prefetch_allowed(true, false, true));
        assert!(!reactive_prefetch_allowed(true, true, true));
        assert!(!reactive_prefetch_allowed(true, false, false));
        assert!(!reactive_prefetch_allowed(true, true, false));
    }

    #[wasm_bindgen_test]
    fn prefetch_requires_all_three_conditions() {
        // The single allowing combination is exactly (!attached, !paused, autofetch).
        assert!(reactive_prefetch_allowed(false, false, true));
        // Flipping any one input flips the result to false.
        assert!(!reactive_prefetch_allowed(true, false, true));
        assert!(!reactive_prefetch_allowed(false, true, true));
        assert!(!reactive_prefetch_allowed(false, false, false));
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
