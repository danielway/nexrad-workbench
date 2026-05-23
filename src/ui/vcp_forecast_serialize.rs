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
use crate::state::{BucketKey, ChunkArrivalStat, SweepStatus, SweepTiming, VolumeForecastSnapshot};
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
                Some(SweepTiming::Observed) => "Obs",
                Some(SweepTiming::Anchored) => "Anch",
                Some(SweepTiming::Estimated) => "Est",
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
            "  seq  type          elev        empty  bucket            stats_n  path    anchor   pred_err  act_int  pred_wait    Δint     lag_ms    physics"
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
                "  {:>3}  {:<12}  {:<10}  {:>5}  {:<16}  {:>7}  {:<6}  {:<7}  {:>8}  {:>7}  {:>9}  {:>8}  {:>7}  {}",
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
