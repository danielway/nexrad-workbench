//! Plain-text serialization of the VCP forecast snapshot for the clipboard
//! button in [`super::vcp_forecast_modal`].
//!
//! Kept separate from the rendering code so the modal stays focused on UI
//! layout and the serialization layout can evolve independently. The format
//! is deliberately compact and column-aligned so it pastes well into chat
//! messages for iterating on the forecasting algorithms.

use super::vcp_forecast_modal::{
    anchor_source_tally, collect_interval_errors_ms, compute_per_bucket_stats, count_statuses,
    fmt_elev, fmt_physics, format_anchor_tally, format_path_tally, format_time,
    scheduler_path_tally, stats_on, total_empty_polls,
};
use crate::state::{
    BucketKey, ChunkArrivalStat, ForecastTimingLabel, SweepStatus, VolumeForecastSnapshot,
};
use std::fmt::Write as _;

pub fn serialize_forecast(
    snap: &VolumeForecastSnapshot,
    arrivals: &[ChunkArrivalStat],
    site_id: &str,
) -> String {
    let mut out = String::new();

    let name = snap.vcp_name.unwrap_or("?");
    let _ = writeln!(
        out,
        "site={} VCP={} ({}) mode={} elevations={}",
        site_id,
        snap.vcp_number,
        name,
        if snap.is_clear_air {
            "clear_air"
        } else {
            "precip"
        },
        snap.expected_elevation_count
    );
    let _ = writeln!(out, "volume_start={}", format_time(snap.volume_start));
    let predicted_dur = snap.predicted_volume_end - snap.volume_start;
    let actual_dur_str = snap
        .actual_volume_end
        .map(|e| format!("{:.1}s", e - snap.volume_start))
        .unwrap_or_else(|| "—".into());
    let drift_str = snap
        .actual_volume_end
        .map(|e| format!("{:+.1}s", e - snap.predicted_volume_end))
        .unwrap_or_else(|| "—".into());
    let _ = writeln!(
        out,
        "duration: pred={:.1}s actual={} drift={}",
        predicted_dur, actual_dur_str, drift_str
    );
    let _ = writeln!(
        out,
        "inter_volume_gap: obs={} pred={} delta={}",
        snap.inter_volume_gap_secs
            .map(|g| format!("{g:+.2}s"))
            .unwrap_or_else(|| "—".into()),
        snap.predicted_inter_volume_gap_secs
            .map(|g| format!("{g:+.2}s"))
            .unwrap_or_else(|| "—".into()),
        match (
            snap.inter_volume_gap_secs,
            snap.predicted_inter_volume_gap_secs,
        ) {
            (Some(o), Some(p)) => format!("{:+.2}s", o - p),
            _ => "—".into(),
        },
    );
    out.push('\n');

    // ── Per-elevation table ─────────────────────────────────────────
    let _ = writeln!(
        out,
        "elv  ang    wf    used  src | pred_dur act_dur Δdur   Δstart | pred_ch act_ch Δch | timing status"
    );
    for s in &snap.sweeps {
        let _ = writeln!(
            out,
            "{:>3}  {:>5.2}  {:<4} {:>5.2} {:<3} | {:>6.1}s {:>6}s {:>5} {:>6} | {:>6} {:>6} {:>3} | {:<6} {}",
            s.elev_number,
            s.elev_angle,
            trim_str(&s.waveform, 4),
            s.azimuth_rate_used,
            s.rate_source.short(),
            s.predicted_duration,
            s.actual_duration()
                .map(|d| format!("{d:.1}"))
                .unwrap_or_else(|| "—".into()),
            s.actual_duration()
                .map(|d| format!("{:+.2}s", d - s.predicted_duration))
                .unwrap_or_else(|| "—".into()),
            s.actual_start
                .map(|a| format!("{:+.2}s", a - s.predicted_start))
                .unwrap_or_else(|| "—".into()),
            s.predicted_chunks
                .map(|c| format!("{c}"))
                .unwrap_or_else(|| "—".into()),
            s.actual_chunks
                .map(|c| format!("{c}"))
                .unwrap_or_else(|| "—".into()),
            match (s.actual_chunks, s.predicted_chunks) {
                (Some(a), Some(p)) => format!("{:+}", a as i32 - p as i32),
                _ => "—".into(),
            },
            match s.timing_source {
                Some(ForecastTimingLabel::Observed) => "Obs",
                Some(ForecastTimingLabel::Anchored) => "Anch",
                Some(ForecastTimingLabel::Estimated) => "Est",
                None => "—",
            },
            match s.status {
                SweepStatus::Complete => "Complete",
                SweepStatus::InProgress { .. } => "InProgress",
                SweepStatus::Future => "Future",
            },
        );
    }
    out.push('\n');

    // ── Summary ─────────────────────────────────────────────────────
    let (complete, in_progress, future) = count_statuses(snap);
    let _ = writeln!(
        out,
        "summary: complete={complete} in_progress={in_progress} future={future}"
    );

    let dur_errs: Vec<f64> = snap
        .sweeps
        .iter()
        .filter_map(|s| s.actual_duration().map(|d| d - s.predicted_duration))
        .collect();
    if let Some((mean, median, max_abs)) = stats_on(&dur_errs) {
        let _ = writeln!(
            out,
            "duration_err: mean={mean:+.2}s median={median:+.2}s max_abs={max_abs:.2}s"
        );
    }

    let chunk_errs: Vec<f64> = snap
        .sweeps
        .iter()
        .filter_map(|s| match (s.actual_chunks, s.predicted_chunks) {
            (Some(a), Some(p)) => Some(a as f64 - p as f64),
            _ => None,
        })
        .collect();
    if let Some((mean, _median, max_abs)) = stats_on(&chunk_errs) {
        let _ = writeln!(out, "chunk_err: mean={mean:+.2} max_abs={max_abs:.0}");
    }

    let total_empty = total_empty_polls(arrivals);
    let any_retry = arrivals.iter().filter(|a| a.empty_polls > 0).count();
    let total_requests = arrivals.len() as u32 + total_empty;
    let waste_pct = if total_requests > 0 {
        100.0 * total_empty as f64 / total_requests as f64
    } else {
        0.0
    };
    let _ = writeln!(
        out,
        "s3_requests: {total_requests} total → {total_empty} wasted ({waste_pct:.1}%)  retries_on={any_retry}/{} chunks",
        arrivals.len()
    );
    let pred_errs: Vec<f64> = arrivals
        .iter()
        .filter_map(|a| a.prediction_error_secs())
        .collect();
    if let Some((mean, median, max_abs)) = stats_on(&pred_errs) {
        let _ = writeln!(
            out,
            "chunk_pred_err: mean={mean:+.2}s median={median:+.2}s max_abs={max_abs:.2}s  (availability-space)"
        );
    }
    let interval_errs_ms = collect_interval_errors_ms(arrivals);
    if let Some((mean, median, max_abs)) = stats_on(&interval_errs_ms) {
        let _ = writeln!(
            out,
            "interval_err: mean={mean:+.0}ms median={median:+.0}ms max_abs={max_abs:.0}ms  (collection-space, n={}; positive = chunk took longer than predicted)",
            interval_errs_ms.len()
        );
    }
    let wait_after_empty_ms: Vec<f64> = arrivals
        .iter()
        .filter_map(|a| a.wait_after_last_empty_ms())
        .collect();
    if let Some((mean, median, max_abs)) = stats_on(&wait_after_empty_ms) {
        let _ = writeln!(
            out,
            "wait_after_last_empty_ms: mean={mean:.0} median={median:.0} max_abs={max_abs:.0}"
        );
    }
    let lag_ms: Vec<f64> = arrivals
        .iter()
        .filter_map(|a| a.availability_lag_ms.map(|m| m as f64))
        .collect();
    if let Some((mean, median, max_abs)) = stats_on(&lag_ms) {
        let _ = writeln!(
            out,
            "availability_lag_ms: mean={mean:.0} median={median:.0} max_abs={max_abs:.0}  (n={}/{})",
            lag_ms.len(),
            arrivals.len()
        );
    }
    let path_tally = scheduler_path_tally(arrivals);
    if path_tally.iter().any(|(_, n)| *n > 0) {
        let _ = writeln!(out, "path: {}", format_path_tally(&path_tally));
    }
    let anchor_tally = anchor_source_tally(arrivals);
    if anchor_tally.iter().any(|(_, n)| *n > 0) {
        let _ = writeln!(out, "anchor: {}", format_anchor_tally(&anchor_tally));
    }
    out.push('\n');

    // ── Per-bucket table ────────────────────────────────────────────
    let bucket_rows = compute_per_bucket_stats(arrivals);
    if !bucket_rows.is_empty() {
        let _ = writeln!(
            out,
            "per_bucket  (bucket = chunk_type|waveform|channel|first_in_sweep)"
        );
        let _ = writeln!(
            out,
            "  bucket           n   med_pred_err  med_lag    n_lag  med_wait_empty"
        );
        for row in &bucket_rows {
            let _ = writeln!(
                out,
                "  {:<16} {:>3}  {:>11}  {:>8}  {:>5}  {:>13}",
                row.bucket.short(),
                row.n,
                row.median_pred_err_ms
                    .map(|m| format!("{m:+.0}ms"))
                    .unwrap_or_else(|| "—".into()),
                row.median_lag_ms
                    .map(|m| format!("{m:+.0}ms"))
                    .unwrap_or_else(|| "—".into()),
                row.n_lag,
                row.median_wait_after_empty_ms
                    .map(|m| format!("{m:.0}ms"))
                    .unwrap_or_else(|| "—".into()),
            );
        }
        out.push('\n');
    }

    // ── Per-chunk arrivals table ───────────────────────────────────
    if !arrivals.is_empty() {
        let _ = writeln!(
            out,
            "chunk_arrivals  (path = hist|phys|legacy|start;  anchor = obs|median|default;  Δint = act_int − pred_wait, collection-space)"
        );
        let _ = writeln!(
            out,
            "  seq  type          elev        empty  bucket            stats_n  path    anchor   wait      pred_err  act_int  pred_wait    Δint     lag_ms    physics"
        );
        let mut prev_elev: Option<u8> = None;
        let mut prev_arrival: Option<&ChunkArrivalStat> = None;
        for a in arrivals {
            if prev_elev.is_some() && a.elevation_number != prev_elev {
                out.push('\n');
            }
            prev_elev = a.elevation_number;
            let act_int = prev_arrival.and_then(|p| a.actual_interval_secs(p));
            let int_err = prev_arrival.and_then(|p| a.interval_error_ms(p));
            let _ = writeln!(
                out,
                "  {:>3}  {:<12}  {:<10}  {:>5}  {:<16}  {:>7}  {:<6}  {:<7}  {:<8}  {:>8}  {:>7}  {:>9}  {:>8}  {:>7}  {}",
                a.sequence,
                a.chunk_type,
                fmt_elev(a),
                a.empty_polls,
                a.bucket_key
                    .as_ref()
                    .map(BucketKey::short)
                    .unwrap_or_else(|| "—".into()),
                if a.stats_n_at_prediction == 0 {
                    "—".into()
                } else {
                    format!("{}", a.stats_n_at_prediction)
                },
                a.scheduler_path.map(|p| p.short()).unwrap_or("—"),
                a.anchor_source.map(|s| s.short()).unwrap_or("—"),
                a.wait_resolution.short(),
                a.prediction_error_secs()
                    .map(|e| format!("{e:+.2}s"))
                    .unwrap_or_else(|| "—".into()),
                act_int
                    .map(|s| format!("{s:.2}s"))
                    .unwrap_or_else(|| "—".into()),
                a.predicted_wait_secs
                    .map(|s| format!("{s:.2}s"))
                    .unwrap_or_else(|| "—".into()),
                int_err
                    .map(|ms| format!("{ms:+.0}ms"))
                    .unwrap_or_else(|| "—".into()),
                a.availability_lag_ms
                    .map(|ms| format!("{ms:+}ms"))
                    .unwrap_or_else(|| "—".into()),
                fmt_physics(a.physics_breakdown.as_ref()),
            );
            prev_arrival = Some(a);
        }
    }

    out
}

fn trim_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        s.chars().take(max_len).collect()
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::state::vcp_forecast::RateSource;
    use crate::state::SweepForecast;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ---- trim_str (private, fully pure, no JS) ----

    #[wasm_bindgen_test]
    fn trim_str_shorter_than_max_passes_through() {
        // 2 chars, max 4 → unchanged.
        assert_eq!(trim_str("CS", 4), "CS".to_string());
    }

    #[wasm_bindgen_test]
    fn trim_str_exact_length_boundary_unchanged() {
        // len == max_len uses the `<=` branch (no truncation).
        assert_eq!(trim_str("ABCD", 4), "ABCD".to_string());
    }

    #[wasm_bindgen_test]
    fn trim_str_longer_truncates_to_max() {
        // 6 chars, max 4 → first 4 retained.
        assert_eq!(trim_str("ABCDEF", 4), "ABCD".to_string());
    }

    #[wasm_bindgen_test]
    fn trim_str_empty_stays_empty() {
        assert_eq!(trim_str("", 4), String::new());
    }

    #[wasm_bindgen_test]
    fn trim_str_counts_chars_not_bytes() {
        // "éé" is 2 chars but 4 bytes; with max 2 it is <= and stays whole.
        let s = "éé";
        assert_eq!(s.len(), 4); // 4 bytes
        assert_eq!(s.chars().count(), 2); // 2 chars
        assert_eq!(trim_str(s, 2), "éé".to_string());
        // With max 1 it truncates to the first single char.
        assert_eq!(trim_str(s, 1), "é".to_string());
    }

    #[wasm_bindgen_test]
    fn trim_str_zero_max_yields_empty() {
        assert_eq!(trim_str("abc", 0), String::new());
    }

    // ---- serialize_forecast: deterministic (non-date) lines ----
    //
    // The `volume_start=` line goes through `format_time` (js_sys::Date) and is
    // deliberately NOT asserted on; every other line tested here is a pure
    // string/number projection of the snapshot fields.

    fn sweep(status: SweepStatus, rate_source: RateSource) -> SweepForecast {
        SweepForecast {
            elev_number: 1,
            elev_angle: 0.5,
            waveform: "CS".to_string(),
            azimuth_rate_used: 20.0,
            rate_source,
            predicted_start: 0.0,
            predicted_duration: 10.0,
            predicted_chunks: None,
            actual_start: None,
            actual_end: None,
            actual_chunks: None,
            timing_source: None,
            status,
        }
    }

    fn snapshot(
        vcp_name: Option<&'static str>,
        is_clear_air: bool,
        actual_volume_end: Option<f64>,
        inter_gap: Option<f64>,
        pred_inter_gap: Option<f64>,
        sweeps: Vec<SweepForecast>,
    ) -> VolumeForecastSnapshot {
        VolumeForecastSnapshot {
            vcp_number: 212,
            vcp_name,
            is_clear_air,
            volume_start: 500.0,
            predicted_volume_end: 800.0,
            actual_volume_end,
            expected_elevation_count: 14,
            sweeps,
            inter_volume_gap_secs: inter_gap,
            predicted_inter_volume_gap_secs: pred_inter_gap,
        }
    }

    /// Find the single line beginning with `prefix` (after leading whitespace
    /// is irrelevant for these header lines, which are flush-left).
    fn line_with<'a>(out: &'a str, prefix: &str) -> Option<&'a str> {
        out.lines().find(|l| l.starts_with(prefix))
    }

    #[wasm_bindgen_test]
    fn header_line_unknown_name_precip_mode() {
        // vcp_name None → "?" ; is_clear_air false → "precip".
        let snap = snapshot(None, false, None, None, None, vec![]);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let header = line_with(&out, "site=").expect("header line");
        assert_eq!(header, "site=KTLX VCP=212 (?) mode=precip elevations=14");
    }

    #[wasm_bindgen_test]
    fn header_line_named_clear_air_mode() {
        // vcp_name Some → printed verbatim ; is_clear_air true → "clear_air".
        let snap = snapshot(Some("VCP-35"), true, None, None, None, vec![]);
        let out = serialize_forecast(&snap, &[], "KFWS");
        let header = line_with(&out, "site=").expect("header line");
        assert_eq!(
            header,
            "site=KFWS VCP=212 (VCP-35) mode=clear_air elevations=14"
        );
    }

    #[wasm_bindgen_test]
    fn duration_line_dashes_when_no_actual_end() {
        // predicted_dur = 800 - 500 = 300.0 ; actual/drift → "—".
        let snap = snapshot(None, false, None, None, None, vec![]);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let dur = line_with(&out, "duration:").expect("duration line");
        assert_eq!(dur, "duration: pred=300.0s actual=— drift=—");
    }

    #[wasm_bindgen_test]
    fn duration_line_actual_and_signed_drift() {
        // actual_volume_end 820 → actual = 820-500 = 320.0s ;
        // drift = 820 - predicted_end(800) = +20.0s (signed, one decimal).
        let snap = snapshot(None, false, Some(820.0), None, None, vec![]);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let dur = line_with(&out, "duration:").expect("duration line");
        assert_eq!(dur, "duration: pred=300.0s actual=320.0s drift=+20.0s");
    }

    #[wasm_bindgen_test]
    fn duration_line_negative_drift_sign() {
        // actual_volume_end 790 → drift = 790 - 800 = -10.0s.
        let snap = snapshot(None, false, Some(790.0), None, None, vec![]);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let dur = line_with(&out, "duration:").expect("duration line");
        assert_eq!(dur, "duration: pred=300.0s actual=290.0s drift=-10.0s");
    }

    #[wasm_bindgen_test]
    fn inter_volume_gap_all_dashes_when_none() {
        let snap = snapshot(None, false, None, None, None, vec![]);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let gap = line_with(&out, "inter_volume_gap:").expect("gap line");
        assert_eq!(gap, "inter_volume_gap: obs=— pred=— delta=—");
    }

    #[wasm_bindgen_test]
    fn inter_volume_gap_signed_values_and_delta() {
        // obs +5.00s, pred +3.00s, delta = 5 - 3 = +2.00s (two decimals, signed).
        let snap = snapshot(None, false, None, Some(5.0), Some(3.0), vec![]);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let gap = line_with(&out, "inter_volume_gap:").expect("gap line");
        assert_eq!(gap, "inter_volume_gap: obs=+5.00s pred=+3.00s delta=+2.00s");
    }

    #[wasm_bindgen_test]
    fn inter_volume_gap_delta_dash_when_one_missing() {
        // obs known, pred missing → delta cannot be computed → "—".
        let snap = snapshot(None, false, None, Some(5.0), None, vec![]);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let gap = line_with(&out, "inter_volume_gap:").expect("gap line");
        assert_eq!(gap, "inter_volume_gap: obs=+5.00s pred=— delta=—");
    }

    #[wasm_bindgen_test]
    fn summary_counts_each_status_class() {
        // One Complete, one Future, one InProgress → 1/1/1.
        let sweeps = vec![
            sweep(SweepStatus::Complete, RateSource::VcpMessage),
            sweep(SweepStatus::Future, RateSource::VcpMessage),
            sweep(
                SweepStatus::InProgress {
                    radials_received: 0,
                    chunks_received: 0,
                    chunks_expected: None,
                },
                RateSource::VcpMessage,
            ),
        ];
        let snap = snapshot(None, false, None, None, None, sweeps);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let summary = line_with(&out, "summary:").expect("summary line");
        assert_eq!(summary, "summary: complete=1 in_progress=1 future=1");
    }

    #[wasm_bindgen_test]
    fn s3_requests_line_zero_when_no_arrivals() {
        // Empty arrivals → 0 total, 0 wasted, 0.0%, 0/0 chunks.
        let snap = snapshot(None, false, None, None, None, vec![]);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let line = line_with(&out, "s3_requests:").expect("s3 line");
        assert_eq!(
            line,
            "s3_requests: 0 total → 0 wasted (0.0%)  retries_on=0/0 chunks"
        );
    }

    #[wasm_bindgen_test]
    fn sweep_row_renders_status_and_rate_source_tokens() {
        // A single Future sweep with the VCP-message rate source: the per-sweep
        // row must carry the rate-source short code "VCP" and status "Future".
        let snap = snapshot(
            None,
            false,
            None,
            None,
            None,
            vec![sweep(SweepStatus::Future, RateSource::VcpMessage)],
        );
        let out = serialize_forecast(&snap, &[], "KTLX");
        // The status token "Future" only appears in the sweep row (not in the
        // header lines), confirming the row was emitted.
        assert!(
            out.contains("Future"),
            "expected sweep row with Future status, got:\n{out}"
        );
        // Rate-source short code for VcpMessage is "VCP".
        assert!(
            out.contains("VCP"),
            "expected rate-source short 'VCP', got:\n{out}"
        );
    }

    #[wasm_bindgen_test]
    fn sweep_row_uses_projection_library_rate_source_code() {
        // ProjectionLibrary short code is "LIB"; Complete status renders.
        let snap = snapshot(
            None,
            false,
            None,
            None,
            None,
            vec![sweep(SweepStatus::Complete, RateSource::ProjectionLibrary)],
        );
        let out = serialize_forecast(&snap, &[], "KTLX");
        assert!(out.contains("LIB"), "expected 'LIB' src code, got:\n{out}");
        assert!(
            out.contains("Complete"),
            "expected Complete status, got:\n{out}"
        );
    }

    #[wasm_bindgen_test]
    fn s3_requests_counts_empty_polls_and_waste_pct() {
        // Two arrivals: one with 1 empty poll, one with 0.
        // total_empty = 1 ; any_retry = 1 ; total_requests = 2 + 1 = 3 ;
        // waste_pct = 100 * 1/3 = 33.3% (one decimal).
        let mut a0 = ChunkArrivalStat::minimal_for_test(1, 10.0);
        a0.empty_polls = 1;
        let a1 = ChunkArrivalStat::minimal_for_test(2, 11.0);
        let arrivals = vec![a0, a1];
        let snap = snapshot(None, false, None, None, None, vec![]);
        let out = serialize_forecast(&snap, &arrivals, "KTLX");
        let line = line_with(&out, "s3_requests:").expect("s3 line");
        assert_eq!(
            line,
            "s3_requests: 3 total → 1 wasted (33.3%)  retries_on=1/2 chunks"
        );
    }

    #[wasm_bindgen_test]
    fn summary_all_future_when_no_progress() {
        let sweeps = vec![
            sweep(SweepStatus::Future, RateSource::MethodBFallback),
            sweep(SweepStatus::Future, RateSource::MethodBFallback),
        ];
        let snap = snapshot(None, false, None, None, None, sweeps);
        let out = serialize_forecast(&snap, &[], "KTLX");
        let summary = line_with(&out, "summary:").expect("summary line");
        assert_eq!(summary, "summary: complete=0 in_progress=0 future=2");
        // Method-B fallback short code is "FB".
        assert!(out.contains("FB"), "expected 'FB' src code, got:\n{out}");
    }
}
