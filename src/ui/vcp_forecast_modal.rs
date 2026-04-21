//! VCP forecast diagnostics modal.
//!
//! Shows the current (or most recently completed) live volume's forecast
//! snapshot side-by-side with observed actuals, and provides a copy-to-
//! clipboard button so the plain-text output can be pasted into a chat
//! message for iterating on the forecasting algorithms.

use super::colors::ui as ui_colors;
use crate::state::{
    AppState, ChunkArrivalStat, RateSource, SweepForecast, SweepStatus, SweepTiming,
    VolumeForecastSnapshot,
};
use eframe::egui::{self, RichText, Vec2};
use std::fmt::Write as _;

pub fn render_vcp_forecast_modal(ctx: &egui::Context, state: &mut AppState) {
    if !state.vcp_forecast_open {
        return;
    }

    if super::modal_helper::modal_backdrop(ctx, "vcp_forecast_backdrop", 160) {
        state.vcp_forecast_open = false;
        return;
    }

    let dark = state.is_dark;
    let (snap_opt, arrivals) = {
        let live = &state.live_mode_state;
        if live.current_volume_forecast.is_some() {
            (
                live.current_volume_forecast.clone(),
                live.chunk_arrivals.clone(),
            )
        } else if live.last_volume_forecast.is_some() {
            (
                live.last_volume_forecast.clone(),
                live.last_chunk_arrivals.clone(),
            )
        } else {
            (None, Vec::new())
        }
    };

    egui::Window::new("VCP forecast diagnostics")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(860.0, 560.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| match snap_opt {
            None => {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("No live volume yet.")
                        .size(13.0)
                        .color(ui_colors::label(dark)),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Start live mode and wait for a VCP message to arrive; \
                         this modal will then show predicted vs. observed per-elevation stats.",
                    )
                    .size(11.0)
                    .color(ui_colors::label(dark)),
                );
                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    state.vcp_forecast_open = false;
                }
            }
            Some(snap) => {
                render_snapshot(ui, ctx, &snap, &arrivals, dark, state);
            }
        });
}

fn render_snapshot(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    snap: &VolumeForecastSnapshot,
    arrivals: &[ChunkArrivalStat],
    dark: bool,
    state: &mut AppState,
) {
    let label_color = ui_colors::label(dark);
    let value_color = ui_colors::value(dark);
    let heading_color = ui_colors::ACTIVE;

    egui::ScrollArea::vertical()
        .max_height(480.0)
        .show(ui, |ui| {
            // ── Volume metadata ─────────────────────────────────────────
            ui.label(
                RichText::new("Volume")
                    .size(12.0)
                    .strong()
                    .color(heading_color),
            );
            ui.indent("vol_section", |ui| {
                let name = snap.vcp_name.unwrap_or("?");
                kv(
                    ui,
                    "VCP",
                    &format!("{} ({})", snap.vcp_number, name),
                    label_color,
                    value_color,
                );
                kv(
                    ui,
                    "Mode",
                    if snap.is_clear_air {
                        "clear air"
                    } else {
                        "precip"
                    },
                    label_color,
                    value_color,
                );
                kv(
                    ui,
                    "Elevations",
                    &format!("{}", snap.expected_elevation_count),
                    label_color,
                    value_color,
                );
                kv(
                    ui,
                    "Volume start",
                    &format_time(snap.volume_start),
                    label_color,
                    value_color,
                );
                let predicted_dur = snap.predicted_volume_end - snap.volume_start;
                kv(
                    ui,
                    "Predicted end",
                    &format!(
                        "{} (+{:.1}s)",
                        format_time(snap.predicted_volume_end),
                        predicted_dur
                    ),
                    label_color,
                    value_color,
                );
                match snap.actual_volume_end {
                    Some(end) => {
                        let drift = end - snap.predicted_volume_end;
                        kv(
                            ui,
                            "Actual end",
                            &format!(
                                "{} (+{:.1}s, drift {:+.1}s)",
                                format_time(end),
                                end - snap.volume_start,
                                drift
                            ),
                            label_color,
                            value_color,
                        );
                    }
                    None => kv(ui, "Actual end", "—", label_color, value_color),
                }
                kv(
                    ui,
                    "Projections at start",
                    if snap.chunk_projections_available_at_start {
                        "yes"
                    } else {
                        "no"
                    },
                    label_color,
                    value_color,
                );
                let (m_a, m_b, m_lib) = rate_source_tally(snap);
                kv(
                    ui,
                    "Rate sources",
                    &format!("VCP={m_a}  fallback={m_b}  library={m_lib}"),
                    label_color,
                    value_color,
                );
                let gap_obs = snap
                    .inter_volume_gap_secs
                    .map(|g| format!("{g:+.2}s"))
                    .unwrap_or_else(|| "—".into());
                let gap_pred = snap
                    .predicted_inter_volume_gap_secs
                    .map(|g| format!("{g:+.2}s"))
                    .unwrap_or_else(|| "—".into());
                let gap_delta = match (
                    snap.inter_volume_gap_secs,
                    snap.predicted_inter_volume_gap_secs,
                ) {
                    (Some(o), Some(p)) => format!("{:+.2}s", o - p),
                    _ => "—".into(),
                };
                kv(
                    ui,
                    "Inter-volume gap (obs / pred / Δ)",
                    &format!("{gap_obs} / {gap_pred} / {gap_delta}"),
                    label_color,
                    value_color,
                );
                if let Some(prev_end) = snap.previous_volume_end {
                    kv(
                        ui,
                        "  prev volume end",
                        &format_time(prev_end),
                        label_color,
                        value_color,
                    );
                }
            });

            ui.separator();

            // ── Per-elevation table ─────────────────────────────────────
            ui.label(
                RichText::new("Per-elevation")
                    .size(12.0)
                    .strong()
                    .color(heading_color),
            );

            egui::Grid::new("vcp_forecast_grid")
                .striped(true)
                .min_col_width(44.0)
                .spacing(Vec2::new(10.0, 4.0))
                .show(ui, |ui| {
                    for header in [
                        "elv", "ang", "wf", "prf", "S", "M", "B", "vcp_r", "fb_r", "used", "src",
                        "pred_dur", "act_dur", "Δdur", "pred_ch", "act_ch", "Δch", "Δstart",
                        "obs_rate", "timing", "status",
                    ] {
                        ui.label(
                            RichText::new(header)
                                .size(10.0)
                                .strong()
                                .color(heading_color),
                        );
                    }
                    ui.end_row();

                    for s in &snap.sweeps {
                        grid_row(ui, s, snap.volume_start, label_color, value_color);
                    }
                });

            ui.separator();

            // ── Summary ─────────────────────────────────────────────────
            ui.label(
                RichText::new("Summary")
                    .size(12.0)
                    .strong()
                    .color(heading_color),
            );
            ui.indent("summary", |ui| {
                let (complete, in_progress, future) = count_statuses(snap);
                kv(
                    ui,
                    "Counts",
                    &format!("complete={complete} in_progress={in_progress} future={future}"),
                    label_color,
                    value_color,
                );
                let dur_errs: Vec<f64> = snap
                    .sweeps
                    .iter()
                    .filter_map(|s| s.actual_duration().map(|d| d - s.predicted_duration))
                    .collect();
                if let Some((mean, median, max_abs)) = stats_on(&dur_errs) {
                    kv(
                        ui,
                        "Duration error (actual - predicted)",
                        &format!("mean {mean:+.2}s  median {median:+.2}s  max|{max_abs:.2}s|"),
                        label_color,
                        value_color,
                    );
                } else {
                    kv(
                        ui,
                        "Duration error",
                        "— (no complete sweeps yet)",
                        label_color,
                        value_color,
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
                    kv(
                        ui,
                        "Chunk count error",
                        &format!("mean {mean:+.2}  max|{max_abs:.0}|"),
                        label_color,
                        value_color,
                    );
                } else {
                    kv(ui, "Chunk count error", "—", label_color, value_color);
                }
                if let Some(end) = snap.actual_volume_end {
                    kv(
                        ui,
                        "Volume-end drift",
                        &format!("{:+.2}s", end - snap.predicted_volume_end),
                        label_color,
                        value_color,
                    );
                }

                // Chunk-level aggregates
                let total_empty = total_empty_polls(arrivals);
                let any_retry = arrivals.iter().filter(|a| a.empty_polls > 0).count();
                kv(
                    ui,
                    "Empty polls (total)",
                    &format!(
                        "{total_empty}  (chunks w/ ≥1 retry: {any_retry}/{})",
                        arrivals.len()
                    ),
                    label_color,
                    value_color,
                );
                let pred_errs: Vec<f64> = arrivals
                    .iter()
                    .filter_map(|a| a.prediction_error_secs())
                    .collect();
                if let Some((mean, median, max_abs)) = stats_on(&pred_errs) {
                    kv(
                        ui,
                        "Chunk prediction error (success - predicted)",
                        &format!("mean {mean:+.2}s  median {median:+.2}s  max|{max_abs:.2}s|"),
                        label_color,
                        value_color,
                    );
                }
                let wait_after_last_empty: Vec<f64> = arrivals
                    .iter()
                    .filter_map(|a| a.wait_after_last_empty_ms())
                    .collect();
                if let Some((mean, median, max_abs)) = stats_on(&wait_after_last_empty) {
                    kv(
                        ui,
                        "Wait after last empty (success - last_empty)",
                        &format!("mean {mean:.0}ms  median {median:.0}ms  max {max_abs:.0}ms"),
                        label_color,
                        value_color,
                    );
                }
                // Authoritative lag vs. S3 publish time (Last-Modified header).
                let wait_after_s3: Vec<f64> = arrivals
                    .iter()
                    .filter_map(|a| a.wait_after_s3_publish_ms())
                    .collect();
                if let Some((mean, median, max_abs)) = stats_on(&wait_after_s3) {
                    kv(
                        ui,
                        "Wait after S3 publish (success - Last-Modified)",
                        &format!("mean {mean:.0}ms  median {median:.0}ms  max {max_abs:.0}ms"),
                        label_color,
                        value_color,
                    );
                }
            });

            ui.separator();

            // ── Chunk arrivals ─────────────────────────────────────────
            ui.label(
                RichText::new("Chunk arrivals")
                    .size(12.0)
                    .strong()
                    .color(heading_color),
            );
            if arrivals.is_empty() {
                ui.indent("arrivals_empty", |ui| {
                    ui.label(
                        RichText::new("— no chunks recorded yet")
                            .size(11.0)
                            .color(label_color),
                    );
                });
            } else {
                egui::Grid::new("vcp_forecast_arrivals_grid")
                    .striped(true)
                    .min_col_width(44.0)
                    .spacing(Vec2::new(10.0, 4.0))
                    .show(ui, |ui| {
                        for header in [
                            "seq",
                            "type",
                            "empty",
                            "predicted_at",
                            "success_at",
                            "pred_err",
                            "last_empty",
                            "wait_after_empty",
                            "s3_last_mod",
                            "wait_after_s3",
                            "fetch_ms",
                        ] {
                            ui.label(
                                RichText::new(header)
                                    .size(10.0)
                                    .strong()
                                    .color(heading_color),
                            );
                        }
                        ui.end_row();

                        for a in arrivals {
                            arrival_row(ui, a, snap.volume_start, label_color, value_color);
                        }
                    });
            }

            ui.add_space(6.0);
        });

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Copy to clipboard").clicked() {
            let text = serialize_forecast(snap, arrivals);
            ctx.copy_text(text);
            state.status_message = "Forecast diagnostics copied to clipboard".to_string();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Close").clicked() {
                state.vcp_forecast_open = false;
            }
        });
    });
}

fn arrival_row(
    ui: &mut egui::Ui,
    a: &ChunkArrivalStat,
    vol_start: f64,
    label_color: egui::Color32,
    value_color: egui::Color32,
) {
    let mono = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
        ui.label(RichText::new(text).size(10.0).monospace().color(color));
    };
    let fmt_off = |t: f64| format!("+{:.2}s", t - vol_start);

    mono(ui, format!("{}", a.sequence), value_color);
    mono(ui, a.chunk_type.to_string(), label_color);
    let empty_color = if a.empty_polls > 0 {
        egui::Color32::from_rgb(220, 140, 60)
    } else {
        value_color
    };
    mono(ui, format!("{}", a.empty_polls), empty_color);
    mono(
        ui,
        a.predicted_available_at
            .map(fmt_off)
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(ui, fmt_off(a.success_at), value_color);
    mono(
        ui,
        a.prediction_error_secs()
            .map(|e| format!("{e:+.2}s"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        a.last_empty_poll_at
            .map(fmt_off)
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        a.wait_after_last_empty_ms()
            .map(|ms| format!("{ms:.0}ms"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        a.s3_last_modified_at
            .map(fmt_off)
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        a.wait_after_s3_publish_ms()
            .map(|ms| format!("{ms:.0}ms"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(ui, format!("{:.0}ms", a.fetch_latency_ms), value_color);
    ui.end_row();
}

fn total_empty_polls(arrivals: &[ChunkArrivalStat]) -> u32 {
    arrivals.iter().map(|a| a.empty_polls).sum()
}

// ── Grid row ────────────────────────────────────────────────────────────

fn grid_row(
    ui: &mut egui::Ui,
    s: &SweepForecast,
    vol_start: f64,
    label_color: egui::Color32,
    value_color: egui::Color32,
) {
    let mono = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
        ui.label(RichText::new(text).size(10.0).monospace().color(color));
    };

    mono(ui, format!("{}", s.elev_number), value_color);
    mono(ui, format!("{:.2}°", s.elev_angle), value_color);
    mono(ui, s.waveform.clone(), value_color);
    mono(ui, format!("{}", s.prf_number), value_color);
    mono(ui, yesno(s.is_sails), label_color);
    mono(ui, yesno(s.is_mrle), label_color);
    mono(ui, yesno(s.is_base_tilt), label_color);
    mono(
        ui,
        s.vcp_azimuth_rate
            .map(|r| format!("{r:.2}"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(ui, format!("{:.2}", s.fallback_azimuth_rate), value_color);
    mono(ui, format!("{:.2}", s.azimuth_rate_used), value_color);
    mono(ui, s.rate_source.short().to_string(), label_color);

    mono(ui, format!("{:.1}s", s.predicted_duration), value_color);
    mono(
        ui,
        s.actual_duration()
            .map(|d| format!("{d:.1}s"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        s.actual_duration()
            .map(|d| format!("{:+.2}s", d - s.predicted_duration))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );

    mono(
        ui,
        s.predicted_chunks
            .map(|c| format!("{c}"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        s.actual_chunks
            .map(|c| format!("{c}"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        match (s.actual_chunks, s.predicted_chunks) {
            (Some(a), Some(p)) => format!("{:+}", a as i32 - p as i32),
            _ => "—".into(),
        },
        value_color,
    );

    mono(
        ui,
        s.actual_start
            .map(|a| format!("{:+.2}s", a - s.predicted_start))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        s.observed_rate_dps
            .map(|r| format!("{r:.2}"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono(
        ui,
        match s.timing_source {
            Some(SweepTiming::Observed) => "Observed".to_string(),
            Some(SweepTiming::Anchored) => "Anchored".to_string(),
            Some(SweepTiming::Estimated) => "Estimated".to_string(),
            None => "—".into(),
        },
        label_color,
    );
    mono(
        ui,
        match s.status {
            SweepStatus::Complete => "Complete".to_string(),
            SweepStatus::InProgress { .. } => "InProg".to_string(),
            SweepStatus::Future => "Future".to_string(),
        },
        label_color,
    );
    let _ = vol_start; // reserved for future per-row offset columns
    ui.end_row();
}

fn kv(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    label_color: egui::Color32,
    value_color: egui::Color32,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(11.0).color(label_color));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(value)
                    .size(11.0)
                    .monospace()
                    .color(value_color),
            );
        });
    });
}

fn yesno(v: bool) -> String {
    if v {
        "Y".into()
    } else {
        "-".into()
    }
}

fn rate_source_tally(snap: &VolumeForecastSnapshot) -> (usize, usize, usize) {
    let mut a = 0;
    let mut b = 0;
    let mut lib = 0;
    for s in &snap.sweeps {
        match s.rate_source {
            RateSource::VcpMessage => a += 1,
            RateSource::MethodBFallback => b += 1,
            RateSource::ProjectionLibrary => lib += 1,
        }
    }
    (a, b, lib)
}

fn count_statuses(snap: &VolumeForecastSnapshot) -> (usize, usize, usize) {
    let mut complete = 0;
    let mut in_progress = 0;
    let mut future = 0;
    for s in &snap.sweeps {
        match s.status {
            SweepStatus::Complete => complete += 1,
            SweepStatus::InProgress { .. } => in_progress += 1,
            SweepStatus::Future => future += 1,
        }
    }
    (complete, in_progress, future)
}

/// Returns (mean, median, max_abs) for a sample of error values.
fn stats_on(values: &[f64]) -> Option<(f64, f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let mean: f64 = values.iter().sum::<f64>() / values.len() as f64;
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };
    let max_abs = values.iter().map(|v| v.abs()).fold(0.0f64, f64::max);
    Some((mean, median, max_abs))
}

/// Format a Unix-seconds timestamp as `YYYY-MM-DD HH:MM:SSZ`.
fn format_time(secs: f64) -> String {
    let ms = (secs * 1000.0) as i64;
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64));
    let iso = date.to_iso_string().as_string().unwrap_or_default();
    // "2026-04-21T18:14:03.000Z" → "2026-04-21 18:14:03Z"
    if iso.len() >= 20 {
        format!("{} {}Z", &iso[0..10], &iso[11..19])
    } else {
        iso
    }
}

// ── Plain-text serialization (clipboard payload) ────────────────────────

pub fn serialize_forecast(snap: &VolumeForecastSnapshot, arrivals: &[ChunkArrivalStat]) -> String {
    let mut out = String::new();

    let name = snap.vcp_name.unwrap_or("?");
    let _ = writeln!(
        out,
        "VCP {} ({})  clear_air={}  elevations={}",
        snap.vcp_number, name, snap.is_clear_air, snap.expected_elevation_count
    );
    let _ = writeln!(
        out,
        "volume_start={}  captured_at=+{:.1}s",
        format_time(snap.volume_start),
        snap.captured_at - snap.volume_start
    );
    let predicted_dur = snap.predicted_volume_end - snap.volume_start;
    let _ = writeln!(
        out,
        "predicted_end={} (+{:.1}s)  actual_end={}  drift={}",
        format_time(snap.predicted_volume_end),
        predicted_dur,
        snap.actual_volume_end
            .map(|e| format!("{} (+{:.1}s)", format_time(e), e - snap.volume_start))
            .unwrap_or_else(|| "—".into()),
        snap.actual_volume_end
            .map(|e| format!("{:+.1}s", e - snap.predicted_volume_end))
            .unwrap_or_else(|| "—".into())
    );
    let _ = writeln!(
        out,
        "inter_volume_gap={} (prev_end={})  predicted_gap={}",
        snap.inter_volume_gap_secs
            .map(|g| format!("{g:+.2}s"))
            .unwrap_or_else(|| "—".into()),
        snap.previous_volume_end
            .map(format_time)
            .unwrap_or_else(|| "—".into()),
        snap.predicted_inter_volume_gap_secs
            .map(|g| format!("{g:+.2}s"))
            .unwrap_or_else(|| "—".into())
    );
    let (m_a, m_b, m_lib) = rate_source_tally(snap);
    let _ = writeln!(
        out,
        "projections_at_start={}  rates: vcp={}  fallback={}  library={}",
        snap.chunk_projections_available_at_start, m_a, m_b, m_lib
    );
    out.push('\n');

    let _ = writeln!(
        out,
        "elv  angle  wf    prf S M B  vcp_r  fb_r  used  src | pred_dur act_dur  Δdur  | pred_ch act_ch Δch | Δstart obs_rate | timing     status"
    );

    for s in &snap.sweeps {
        let _ = writeln!(
            out,
            "{:>3}  {:>5.2}  {:<4} {:>3} {} {} {}  {:>5}  {:>5.2} {:>5.2} {:<3} | {:>6.1}s {:>6}s {:>5} | {:>6} {:>6} {:>3} | {:>6} {:>7} | {:<10} {}",
            s.elev_number,
            s.elev_angle,
            trim_str(&s.waveform, 4),
            s.prf_number,
            yesno(s.is_sails),
            yesno(s.is_mrle),
            yesno(s.is_base_tilt),
            s.vcp_azimuth_rate
                .map(|r| format!("{r:.2}"))
                .unwrap_or_else(|| "—".into()),
            s.fallback_azimuth_rate,
            s.azimuth_rate_used,
            s.rate_source.short(),
            s.predicted_duration,
            s.actual_duration()
                .map(|d| format!("{d:.1}"))
                .unwrap_or_else(|| "—".into()),
            s.actual_duration()
                .map(|d| format!("{:+.2}s", d - s.predicted_duration))
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
            s.actual_start
                .map(|a| format!("{:+.2}s", a - s.predicted_start))
                .unwrap_or_else(|| "—".into()),
            s.observed_rate_dps
                .map(|r| format!("{r:.2}"))
                .unwrap_or_else(|| "—".into()),
            match s.timing_source {
                Some(SweepTiming::Observed) => "Observed",
                Some(SweepTiming::Anchored) => "Anchored",
                Some(SweepTiming::Estimated) => "Estimated",
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

    let (complete, in_progress, future) = count_statuses(snap);
    let _ = writeln!(
        out,
        "summary: complete={complete}  in_progress={in_progress}  future={future}"
    );

    let dur_errs: Vec<f64> = snap
        .sweeps
        .iter()
        .filter_map(|s| s.actual_duration().map(|d| d - s.predicted_duration))
        .collect();
    if let Some((mean, median, max_abs)) = stats_on(&dur_errs) {
        let _ = writeln!(
            out,
            "duration_err: mean={mean:+.2}s  median={median:+.2}s  max_abs={max_abs:.2}s"
        );
    } else {
        let _ = writeln!(out, "duration_err: —");
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
        let _ = writeln!(out, "chunk_err:    mean={mean:+.2}  max_abs={max_abs:.0}");
    } else {
        let _ = writeln!(out, "chunk_err: —");
    }

    if let Some(end) = snap.actual_volume_end {
        let _ = writeln!(
            out,
            "volume_end_drift: {:+.2}s",
            end - snap.predicted_volume_end
        );
    }
    let _ = writeln!(
        out,
        "inter_volume_gap: observed={}  predicted={}  delta={}",
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

    // ── Chunk arrivals ───────────────────────────────────────────────
    let total_empty = total_empty_polls(arrivals);
    let any_retry = arrivals.iter().filter(|a| a.empty_polls > 0).count();
    let _ = writeln!(
        out,
        "chunk_arrivals: count={} total_empty_polls={} chunks_with_retries={}/{}",
        arrivals.len(),
        total_empty,
        any_retry,
        arrivals.len()
    );
    let pred_errs: Vec<f64> = arrivals
        .iter()
        .filter_map(|a| a.prediction_error_secs())
        .collect();
    if let Some((mean, median, max_abs)) = stats_on(&pred_errs) {
        let _ = writeln!(
            out,
            "chunk_pred_err: mean={mean:+.2}s  median={median:+.2}s  max_abs={max_abs:.2}s"
        );
    } else {
        let _ = writeln!(out, "chunk_pred_err: —");
    }
    let wait_after_empty_ms: Vec<f64> = arrivals
        .iter()
        .filter_map(|a| a.wait_after_last_empty_ms())
        .collect();
    if let Some((mean, median, max_abs)) = stats_on(&wait_after_empty_ms) {
        let _ = writeln!(
            out,
            "wait_after_last_empty_ms: mean={mean:.0}  median={median:.0}  max_abs={max_abs:.0}"
        );
    } else {
        let _ = writeln!(out, "wait_after_last_empty_ms: —  (no retries)");
    }
    let wait_after_s3_ms: Vec<f64> = arrivals
        .iter()
        .filter_map(|a| a.wait_after_s3_publish_ms())
        .collect();
    let s3_coverage = arrivals
        .iter()
        .filter(|a| a.s3_last_modified_at.is_some())
        .count();
    if let Some((mean, median, max_abs)) = stats_on(&wait_after_s3_ms) {
        let _ = writeln!(
            out,
            "wait_after_s3_publish_ms: mean={mean:.0}  median={median:.0}  max_abs={max_abs:.0}  (coverage {}/{})",
            s3_coverage,
            arrivals.len()
        );
    } else {
        let _ = writeln!(
            out,
            "wait_after_s3_publish_ms: —  (Last-Modified unavailable for all chunks)"
        );
    }
    let fetch_ms: Vec<f64> = arrivals.iter().map(|a| a.fetch_latency_ms).collect();
    if let Some((mean, median, max_abs)) = stats_on(&fetch_ms) {
        let _ = writeln!(
            out,
            "fetch_latency_ms: mean={mean:.0}  median={median:.0}  max_abs={max_abs:.0}"
        );
    }

    if !arrivals.is_empty() {
        out.push('\n');
        let _ = writeln!(
            out,
            "seq  type          empty  predicted_at  success_at  pred_err  last_empty  wait_after_empty  s3_last_mod  wait_after_s3  fetch_ms"
        );
        for a in arrivals {
            let fmt_off = |t: f64| format!("+{:.2}s", t - snap.volume_start);
            let _ = writeln!(
                out,
                "{:>3}  {:<12}  {:>5}  {:>12}  {:>10}  {:>8}  {:>10}  {:>15}  {:>11}  {:>13}  {:>8}",
                a.sequence,
                a.chunk_type,
                a.empty_polls,
                a.predicted_available_at
                    .map(fmt_off)
                    .unwrap_or_else(|| "—".into()),
                fmt_off(a.success_at),
                a.prediction_error_secs()
                    .map(|e| format!("{e:+.2}s"))
                    .unwrap_or_else(|| "—".into()),
                a.last_empty_poll_at
                    .map(fmt_off)
                    .unwrap_or_else(|| "—".into()),
                a.wait_after_last_empty_ms()
                    .map(|ms| format!("{ms:.0}ms"))
                    .unwrap_or_else(|| "—".into()),
                a.s3_last_modified_at
                    .map(fmt_off)
                    .unwrap_or_else(|| "—".into()),
                a.wait_after_s3_publish_ms()
                    .map(|ms| format!("{ms:.0}ms"))
                    .unwrap_or_else(|| "—".into()),
                format!("{:.0}ms", a.fetch_latency_ms),
            );
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
