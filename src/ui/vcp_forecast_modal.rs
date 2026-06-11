//! VCP forecast diagnostics modal.
//!
//! Shows the current (or most recently completed) live volume's forecast
//! snapshot side-by-side with observed actuals, and provides a copy-to-
//! clipboard button so the plain-text output can be pasted into a chat
//! message for iterating on the forecasting algorithms.
//!
//! The data shown here is intentionally curated for an "optimize the model"
//! workflow: every column either drives a tuning decision (which estimator
//! path fired, which bucket key was used, sample count, anchor source,
//! per-chunk lag) or quantifies prediction error against actuals. Raw
//! wall-clock timestamps and structural metadata that don't influence the
//! model are deliberately omitted — they can be reconstructed from the
//! deltas if needed and would otherwise bloat the clipboard payload.

use super::colors::ui as ui_colors;
use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::nexrad::timing::{AnchorSource, IntervalCase, PhysicsBreakdown, SchedulerPath};
use crate::state::{
    AppState, BucketKey, ChunkArrivalStat, ForecastTimingLabel, SweepForecast, SweepStatus,
    VolumeForecastSnapshot,
};
use eframe::egui::{self, RichText, Vec2};
use std::collections::BTreeMap;

pub(super) struct VcpForecastModalLayer;

impl Layer for VcpForecastModalLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        50
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.vcp_forecast_open
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_vcp_forecast_modal(ctx.ctx, ctx.state, ctx.live, ctx.chrome);
    }
}

fn draw_vcp_forecast_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    live: &crate::subsystem::Live,
    chrome: &mut crate::subsystem::Chrome,
) {
    if super::modal_helper::modal_backdrop(ctx, "vcp_forecast_backdrop", 160) {
        chrome.vcp_forecast_open = false;
        return;
    }

    let dark = state.is_dark;
    let site_id = state.viz_state.site_id.clone();
    let (snap_opt, arrivals) = {
        let eng = live.engine.borrow();
        let obs = eng.observations();
        let live = &live.mode_state;
        if let Some(snap) = live.derive_current_volume_forecast(obs) {
            (Some(snap), live.chunk_arrivals.clone())
        } else if let Some(record) = live.last_completed_volume.as_ref() {
            let snap = crate::state::derive_volume_forecast(
                &record.vcp,
                &record.volume_start_plan,
                record.volume_start_secs,
                &record.completed_sweep_metas,
                &record.chunk_elev_spans,
                record.previous_volume_end_secs,
                &record.chunk_arrivals,
                Some(record.volume_end_secs),
            );
            (Some(snap), record.chunk_arrivals.clone())
        } else {
            (None, Vec::new())
        }
    };

    egui::Window::new("VCP forecast diagnostics")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(940.0, 580.0))
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
                    chrome.vcp_forecast_open = false;
                }
            }
            Some(snap) => {
                render_snapshot(ui, ctx, &snap, &arrivals, &site_id, dark, state, chrome);
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn render_snapshot(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    snap: &VolumeForecastSnapshot,
    arrivals: &[ChunkArrivalStat],
    site_id: &str,
    dark: bool,
    state: &mut AppState,
    chrome: &mut crate::subsystem::Chrome,
) {
    let label_color = ui_colors::label(dark);
    let value_color = ui_colors::value(dark);
    let heading_color = ui_colors::ACTIVE;

    egui::ScrollArea::vertical()
        .max_height(500.0)
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
                    "Site / VCP",
                    &format!(
                        "{} · {} ({}) · {} · {} elev",
                        site_id,
                        snap.vcp_number,
                        name,
                        if snap.is_clear_air { "clear" } else { "precip" },
                        snap.expected_elevation_count
                    ),
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
                let drift_str = snap
                    .actual_volume_end
                    .map(|e| format!("{:+.1}s", e - snap.predicted_volume_end))
                    .unwrap_or_else(|| "—".into());
                let actual_dur_str = snap
                    .actual_volume_end
                    .map(|e| format!("{:.1}s", e - snap.volume_start))
                    .unwrap_or_else(|| "—".into());
                kv(
                    ui,
                    "Duration (pred / actual / drift)",
                    &format!("{predicted_dur:.1}s / {actual_dur_str} / {drift_str}"),
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
            });

            ui.separator();

            // ── Per-elevation table ─────────────────────────────────────
            ui.label(
                RichText::new("Per-elevation")
                    .size(12.0)
                    .strong()
                    .color(heading_color),
            );
            ui.label(
                RichText::new(
                    "src = LIB (projection library) / VCP (msg rate) / FB (Method-B fallback);  \
                     Δ = actual − predicted",
                )
                .size(10.0)
                .color(label_color),
            );
            egui::Grid::new("vcp_forecast_grid")
                .striped(true)
                .min_col_width(44.0)
                .spacing(Vec2::new(10.0, 4.0))
                .show(ui, |ui| {
                    for (header, tip) in [
                        ("elv", "Elevation number (1-based) within the volume."),
                        ("ang", "Elevation angle, degrees."),
                        ("wf", "Waveform type for this elevation (CS, CDW, CDWO, B, SPP)."),
                        (
                            "used",
                            "Azimuth rate (deg/s) actually used to predict this sweep's duration.",
                        ),
                        (
                            "src",
                            "Source of the azimuth rate: LIB = projection library, VCP = VCP-message rate, FB = Method-B fallback.",
                        ),
                        ("pred_dur", "Predicted sweep duration, seconds."),
                        (
                            "act_dur",
                            "Observed sweep duration, seconds. — until the sweep completes.",
                        ),
                        (
                            "Δdur",
                            "Sweep duration error = actual − predicted (s). Positive = sweep took longer than predicted.",
                        ),
                        (
                            "Δstart",
                            "Sweep start error = actual − predicted start (s). Positive = sweep started later than predicted.",
                        ),
                        ("pred_ch", "Predicted number of chunks in this sweep."),
                        ("act_ch", "Observed number of chunks in this sweep."),
                        (
                            "Δch",
                            "Chunk-count error = actual − predicted. Positive = more chunks than predicted.",
                        ),
                        (
                            "timing",
                            "Source of the actual timing: Obs = observed, Anch = anchored, Est = estimated.",
                        ),
                        ("status", "Sweep status: Complete / InProg / Future."),
                    ] {
                        header_label(ui, header, tip, heading_color);
                    }
                    ui.end_row();

                    for s in &snap.sweeps {
                        grid_row(ui, s, label_color, value_color);
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
                        "Sweep duration error",
                        &format!("mean {mean:+.2}s  median {median:+.2}s  max|{max_abs:.2}s|"),
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
                        "Sweep chunk-count error",
                        &format!("mean {mean:+.2}  max|{max_abs:.0}|"),
                        label_color,
                        value_color,
                    );
                }

                let total_empty = total_empty_polls(arrivals);
                let any_retry = arrivals.iter().filter(|a| a.empty_polls > 0).count();
                let total_requests = arrivals.len() as u32 + total_empty;
                let waste_pct = if total_requests > 0 {
                    100.0 * total_empty as f64 / total_requests as f64
                } else {
                    0.0
                };
                kv(
                    ui,
                    "S3 requests / wasted",
                    &format!(
                        "{total_requests} → {total_empty} wasted ({waste_pct:.1}%);  retries on {any_retry}/{} chunks",
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
                        "Chunk pred error (avail-space)",
                        &format!("mean {mean:+.2}s  median {median:+.2}s  max|{max_abs:.2}s|"),
                        label_color,
                        value_color,
                    );
                }
                let interval_errs_ms = collect_interval_errors_ms(arrivals);
                if let Some((mean, median, max_abs)) = stats_on(&interval_errs_ms) {
                    kv(
                        ui,
                        "Interval err (collection-space, +ve = underestimated)",
                        &format!(
                            "mean {mean:+.0}ms  median {median:+.0}ms  max|{max_abs:.0}ms|  (n={})",
                            interval_errs_ms.len()
                        ),
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
                        "Wait after last empty",
                        &format!("mean {mean:.0}ms  median {median:.0}ms  max {max_abs:.0}ms"),
                        label_color,
                        value_color,
                    );
                }
                let lag_ms_values: Vec<f64> = arrivals
                    .iter()
                    .filter_map(|a| a.availability_lag_ms.map(|m| m as f64))
                    .collect();
                if let Some((mean, median, max_abs)) = stats_on(&lag_ms_values) {
                    kv(
                        ui,
                        "Availability lag (s3_last_mod − chunk_collection_end)",
                        &format!(
                            "mean {mean:.0}ms  median {median:.0}ms  max|{max_abs:.0}ms|  ({}/{} samples)",
                            lag_ms_values.len(),
                            arrivals.len()
                        ),
                        label_color,
                        value_color,
                    );
                }
                let path_tally = scheduler_path_tally(arrivals);
                if path_tally.iter().any(|(_, n)| *n > 0) {
                    kv(
                        ui,
                        "Estimator path",
                        &format_path_tally(&path_tally),
                        label_color,
                        value_color,
                    );
                }
                let anchor_tally = anchor_source_tally(arrivals);
                if anchor_tally.iter().any(|(_, n)| *n > 0) {
                    kv(
                        ui,
                        "Anchor source",
                        &format_anchor_tally(&anchor_tally),
                        label_color,
                        value_color,
                    );
                }
            });

            ui.separator();

            // ── Per-bucket stats ───────────────────────────────────────
            let bucket_rows = compute_per_bucket_stats(arrivals);
            if !bucket_rows.is_empty() {
                ui.label(
                    RichText::new("Per-bucket stats (chunks observed this volume)")
                        .size(12.0)
                        .strong()
                        .color(heading_color),
                );
                ui.label(
                    RichText::new(
                        "bucket = chunk_type|waveform|channel|first_in_sweep;  \
                         lag = s3_last_mod − chunk_collection_end",
                    )
                    .size(10.0)
                    .color(label_color),
                );
                egui::Grid::new("vcp_forecast_bucket_grid")
                    .striped(true)
                    .min_col_width(44.0)
                    .spacing(Vec2::new(10.0, 4.0))
                    .show(ui, |ui| {
                        for (header, tip) in [
                            (
                                "bucket",
                                "Bucket key = chunk_type|waveform|channel|first_in_sweep. Groups chunks the estimator treats alike.",
                            ),
                            ("n", "Number of chunks observed in this bucket this volume."),
                            (
                                "med_pred_err",
                                "Median availability-clock error (ms): success_at − predicted_available_at. Positive = forecaster too optimistic (polled before the chunk was up).",
                            ),
                            (
                                "med_lag_ms",
                                "Median upload lag (ms): s3_last_modified − chunk_collection_end. Radar→S3 publish latency — NOT our acquisition lateness.",
                            ),
                            (
                                "n_lag",
                                "Number of chunks in this bucket with a measurable upload lag.",
                            ),
                            (
                                "med_wait_after_empty",
                                "Median poll-waste (ms): time from the last empty poll to success — wait potentially avoidable with better poll timing.",
                            ),
                        ] {
                            header_label(ui, header, tip, heading_color);
                        }
                        ui.end_row();

                        for row in &bucket_rows {
                            mono_label(ui, &row.bucket.short(), value_color);
                            mono_label(ui, &format!("{}", row.n), value_color);
                            mono_label(
                                ui,
                                &row.median_pred_err_ms
                                    .map(|m| format!("{m:+.0}ms"))
                                    .unwrap_or_else(|| "—".into()),
                                value_color,
                            );
                            mono_label(
                                ui,
                                &row.median_lag_ms
                                    .map(|m| format!("{m:+.0}ms"))
                                    .unwrap_or_else(|| "—".into()),
                                value_color,
                            );
                            mono_label(ui, &format!("{}", row.n_lag), value_color);
                            mono_label(
                                ui,
                                &row.median_wait_after_empty_ms
                                    .map(|m| format!("{m:.0}ms"))
                                    .unwrap_or_else(|| "—".into()),
                                value_color,
                            );
                            ui.end_row();
                        }
                    });
                ui.separator();
            }

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
                ui.label(
                    RichText::new(
                        "elev = elevation# (chunk-i+1/N);  \
                         path = hist|phys|legacy|start;  anchor = obs|median|default;  \
                         act_int / pred_wait / Δint are collection-space (Δint = act_int − pred_wait);  \
                         physics shows the dominant case + key terms",
                    )
                    .size(10.0)
                    .color(label_color),
                );
                egui::Grid::new("vcp_forecast_arrivals_grid")
                    .striped(true)
                    .min_col_width(40.0)
                    .spacing(Vec2::new(10.0, 4.0))
                    .show(ui, |ui| {
                        for (header, tip) in [
                            (
                                "seq",
                                "1-based sequence number within the volume at the time of success.",
                            ),
                            ("type", "Chunk type: Start / Intermediate / End."),
                            (
                                "elev",
                                "Elevation number, with (chunk-index+1 / chunks-in-sweep).",
                            ),
                            (
                                "empty",
                                "Empty polls (Ok(None)) before the successful fetch — wasted S3 requests. Orange when > 0.",
                            ),
                            (
                                "bucket",
                                "Bucket key used for this chunk's prediction = chunk_type|waveform|channel|first_in_sweep.",
                            ),
                            (
                                "stats_n",
                                "Samples in the bucket when the prediction was made. Distinguishes 'model wrong' from 'stats not warm yet'.",
                            ),
                            (
                                "path",
                                "Estimator branch that produced the prediction: hist / phys / legacy / start.",
                            ),
                            (
                                "anchor",
                                "Anchor branch in use at prediction time: obs / median / default. Non-obs = projections degraded by a fallback anchor.",
                            ),
                            (
                                "pred_err",
                                "Availability-clock error (s): success_at − predicted_available_at. Positive = forecaster too optimistic (polled before the chunk was up). This is acquisition lateness vs predicted availability.",
                            ),
                            (
                                "act_int",
                                "Actual collection-space interval (s): this chunk's collection time − the previous chunk's. Ground truth for pred_wait.",
                            ),
                            (
                                "pred_wait",
                                "Wait (s) the scheduler predicted for this chunk (collection-space).",
                            ),
                            (
                                "Δint",
                                "Interval error (ms) = act_int − pred_wait. Positive = we underestimated the gap (chunk took longer than predicted). Orange when |Δint| > 1s.",
                            ),
                            (
                                "lag_ms",
                                "Upload lag (ms): s3_last_modified − chunk_collection_end. Radar→S3 publish latency — NOT our acquisition lateness.",
                            ),
                            (
                                "physics",
                                "Dominant physics case + key terms. intra = intra-sweep; is = inter-sweep (g=gap, wf=waveform penalty, ch=chunk dur); inter_vol = inter-volume.",
                            ),
                        ] {
                            header_label(ui, header, tip, heading_color);
                        }
                        ui.end_row();

                        let mut prev_elev: Option<u8> = None;
                        let mut prev_arrival: Option<&ChunkArrivalStat> = None;
                        for a in arrivals {
                            if prev_elev.is_some() && a.elevation_number != prev_elev {
                                for _ in 0..14 {
                                    ui.label("");
                                }
                                ui.end_row();
                            }
                            prev_elev = a.elevation_number;
                            arrival_row(ui, a, prev_arrival, label_color, value_color);
                            prev_arrival = Some(a);
                        }
                    });
            }

            ui.add_space(6.0);
        });

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Copy to clipboard").clicked() {
            let text = super::vcp_forecast_serialize::serialize_forecast(snap, arrivals, site_id);
            ctx.copy_text(text);
            state.status_message = "Forecast diagnostics copied to clipboard".to_string();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Close").clicked() {
                chrome.vcp_forecast_open = false;
            }
        });
    });
}

fn arrival_row(
    ui: &mut egui::Ui,
    a: &ChunkArrivalStat,
    prev: Option<&ChunkArrivalStat>,
    label_color: egui::Color32,
    value_color: egui::Color32,
) {
    mono_label(ui, &format!("{}", a.sequence), value_color);
    mono_label(ui, a.chunk_type, label_color);
    mono_label(ui, &fmt_elev(a), label_color);
    let empty_color = if a.empty_polls > 0 {
        egui::Color32::from_rgb(220, 140, 60)
    } else {
        value_color
    };
    mono_label_color(ui, &format!("{}", a.empty_polls), empty_color);
    mono_label(
        ui,
        &a.bucket_key
            .as_ref()
            .map(BucketKey::short)
            .unwrap_or_else(|| "—".into()),
        label_color,
    );
    mono_label(
        ui,
        &if a.stats_n_at_prediction == 0 {
            "—".into()
        } else {
            format!("{}", a.stats_n_at_prediction)
        },
        value_color,
    );
    mono_label(
        ui,
        a.scheduler_path.map(|p| p.short()).unwrap_or("—"),
        label_color,
    );
    mono_label(
        ui,
        a.anchor_source.map(|s| s.short()).unwrap_or("—"),
        label_color,
    );
    mono_label(
        ui,
        &a.prediction_error_secs()
            .map(|e| format!("{e:+.2}s"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    let act_int = prev.and_then(|p| a.actual_interval_secs(p));
    mono_label(
        ui,
        &act_int
            .map(|s| format!("{s:.2}s"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono_label(
        ui,
        &a.predicted_wait_secs
            .map(|s| format!("{s:.2}s"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    let int_err = prev.and_then(|p| a.interval_error_ms(p));
    let int_err_color = match int_err {
        Some(e) if e.abs() > 1000.0 => egui::Color32::from_rgb(220, 140, 60),
        _ => value_color,
    };
    mono_label_color(
        ui,
        &int_err
            .map(|ms| format!("{ms:+.0}ms"))
            .unwrap_or_else(|| "—".into()),
        int_err_color,
    );
    mono_label(
        ui,
        &a.availability_lag_ms
            .map(|ms| format!("{ms:+}ms"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono_label(ui, &fmt_physics(a.physics_breakdown.as_ref()), value_color);
    ui.end_row();
}

fn mono_label(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(RichText::new(text).size(10.0).monospace().color(color));
}

/// Column header with hover help. The terse labels are load-bearing (they
/// double as the copy-to-clipboard payload), so the full name / units / sign
/// convention live in the tooltip rather than widening the column.
fn header_label(ui: &mut egui::Ui, text: &str, tip: &str, color: egui::Color32) {
    ui.label(RichText::new(text).size(10.0).strong().color(color))
        .on_hover_text(tip);
}

fn mono_label_color(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(RichText::new(text).size(10.0).monospace().color(color));
}

pub(super) fn total_empty_polls(arrivals: &[ChunkArrivalStat]) -> u32 {
    arrivals.iter().map(|a| a.empty_polls).sum()
}

// ── Per-elevation grid row ──────────────────────────────────────────────

fn grid_row(
    ui: &mut egui::Ui,
    s: &SweepForecast,
    label_color: egui::Color32,
    value_color: egui::Color32,
) {
    mono_label(ui, &format!("{}", s.elev_number), value_color);
    mono_label(ui, &format!("{:.2}°", s.elev_angle), value_color);
    mono_label(ui, &s.waveform, value_color);
    mono_label(ui, &format!("{:.2}", s.azimuth_rate_used), value_color);
    mono_label(ui, s.rate_source.short(), label_color);

    mono_label(ui, &format!("{:.1}s", s.predicted_duration), value_color);
    mono_label(
        ui,
        &s.actual_duration()
            .map(|d| format!("{d:.1}s"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono_label(
        ui,
        &s.actual_duration()
            .map(|d| format!("{:+.2}s", d - s.predicted_duration))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono_label(
        ui,
        &s.actual_start
            .map(|a| format!("{:+.2}s", a - s.predicted_start))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );

    mono_label(
        ui,
        &s.predicted_chunks
            .map(|c| format!("{c}"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono_label(
        ui,
        &s.actual_chunks
            .map(|c| format!("{c}"))
            .unwrap_or_else(|| "—".into()),
        value_color,
    );
    mono_label(
        ui,
        &match (s.actual_chunks, s.predicted_chunks) {
            (Some(a), Some(p)) => format!("{:+}", a as i32 - p as i32),
            _ => "—".into(),
        },
        value_color,
    );
    mono_label(
        ui,
        match s.timing_source {
            Some(ForecastTimingLabel::Observed) => "Obs",
            Some(ForecastTimingLabel::Anchored) => "Anch",
            Some(ForecastTimingLabel::Estimated) => "Est",
            None => "—",
        },
        label_color,
    );
    mono_label(
        ui,
        match s.status {
            SweepStatus::Complete => "Complete",
            SweepStatus::InProgress { .. } => "InProg",
            SweepStatus::Future => "Future",
        },
        label_color,
    );
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

pub(super) fn fmt_elev(a: &ChunkArrivalStat) -> String {
    match (
        a.elevation_number,
        a.chunk_index_in_sweep,
        a.chunks_in_sweep,
    ) {
        (Some(e), Some(i), Some(n)) => format!("{} ({}/{})", e, i + 1, n),
        (Some(e), _, _) => format!("{}", e),
        _ => "—".into(),
    }
}

/// Compact one-cell summary of the physics decomposition. Format:
///   intra: chunk_dur
///   inter_sweep: gap+wf=total
///   inter_volume: total
pub(super) fn fmt_physics(b: Option<&PhysicsBreakdown>) -> String {
    let Some(b) = b else {
        return "—".into();
    };
    match b.case {
        IntervalCase::IntraSweep => match b.chunk_duration_secs {
            Some(d) => format!("intra {d:.2}s"),
            None => format!("intra ~{:.2}s", b.total_secs),
        },
        IntervalCase::InterSweep => {
            let gap = b.inter_sweep_gap_secs.unwrap_or(0.0);
            let wf = b.waveform_penalty_secs.unwrap_or(0.0);
            let chunk = b.chunk_duration_secs.unwrap_or(0.0);
            // wf is included inside `gap` already; show it separately so
            // the source of any anomaly is obvious at a glance.
            format!(
                "is g={:.2} (wf={:.1}) ch={:.2} → {:.2}s",
                gap - wf,
                wf,
                chunk,
                b.total_secs
            )
        }
        IntervalCase::InterVolume => format!("inter_vol {:.2}s", b.total_secs),
    }
}

pub(super) fn count_statuses(snap: &VolumeForecastSnapshot) -> (usize, usize, usize) {
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
pub(super) fn stats_on(values: &[f64]) -> Option<(f64, f64, f64)> {
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

pub(super) fn median_of(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

/// Format a Unix-seconds timestamp as `YYYY-MM-DD HH:MM:SSZ`.
pub(super) fn format_time(secs: f64) -> String {
    let ms = (secs * 1000.0) as i64;
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64));
    let iso = date.to_iso_string().as_string().unwrap_or_default();
    if iso.len() >= 20 {
        format!("{} {}Z", &iso[0..10], &iso[11..19])
    } else {
        iso
    }
}

// ── Per-bucket aggregation (computed from arrivals) ─────────────────────

#[derive(Debug, Clone)]
pub(super) struct BucketRow {
    pub(super) bucket: BucketKey,
    pub(super) n: usize,
    pub(super) median_pred_err_ms: Option<f64>,
    pub(super) median_lag_ms: Option<f64>,
    pub(super) n_lag: usize,
    pub(super) median_wait_after_empty_ms: Option<f64>,
}

/// Collect per-chunk interval-prediction errors in collection space (ms).
/// Walks pairs of consecutive arrivals; only contributes when both have a
/// `collection_time_secs` and the later has a `predicted_wait_secs`.
pub(super) fn collect_interval_errors_ms(arrivals: &[ChunkArrivalStat]) -> Vec<f64> {
    let mut out = Vec::new();
    let mut prev: Option<&ChunkArrivalStat> = None;
    for a in arrivals {
        if let Some(p) = prev {
            if let Some(e) = a.interval_error_ms(p) {
                out.push(e);
            }
        }
        prev = Some(a);
    }
    out
}

/// Per-bucket sample collector — one entry per bucket key seen.
struct BucketAccum {
    pub(super) bucket: BucketKey,
    pred_errs_ms: Vec<f64>,
    lags_ms: Vec<f64>,
    waits_ms: Vec<f64>,
}

pub(super) fn compute_per_bucket_stats(arrivals: &[ChunkArrivalStat]) -> Vec<BucketRow> {
    let mut by_bucket: BTreeMap<String, BucketAccum> = BTreeMap::new();
    for a in arrivals {
        let Some(bucket) = a.bucket_key else {
            continue;
        };
        let entry = by_bucket.entry(bucket.short()).or_insert(BucketAccum {
            bucket,
            pred_errs_ms: Vec::new(),
            lags_ms: Vec::new(),
            waits_ms: Vec::new(),
        });
        if let Some(e) = a.prediction_error_secs() {
            entry.pred_errs_ms.push(e * 1000.0);
        }
        if let Some(l) = a.availability_lag_ms {
            entry.lags_ms.push(l as f64);
        }
        if let Some(w) = a.wait_after_last_empty_ms() {
            entry.waits_ms.push(w);
        }
    }
    by_bucket
        .into_values()
        .map(|acc| {
            let n = acc
                .pred_errs_ms
                .len()
                .max(acc.lags_ms.len())
                .max(acc.waits_ms.len())
                .max(1);
            let n_lag = acc.lags_ms.len();
            BucketRow {
                bucket: acc.bucket,
                n,
                median_pred_err_ms: median_of(acc.pred_errs_ms),
                median_lag_ms: median_of(acc.lags_ms),
                n_lag,
                median_wait_after_empty_ms: median_of(acc.waits_ms),
            }
        })
        .collect()
}

pub(super) fn scheduler_path_tally(arrivals: &[ChunkArrivalStat]) -> [(SchedulerPath, u32); 4] {
    let mut t = [
        (SchedulerPath::StartConstant, 0u32),
        (SchedulerPath::Blended, 0),
        (SchedulerPath::Physics, 0),
        (SchedulerPath::Legacy, 0),
    ];
    for a in arrivals {
        if let Some(p) = a.scheduler_path {
            for slot in t.iter_mut() {
                if slot.0 == p {
                    slot.1 += 1;
                }
            }
        }
    }
    t
}

pub(super) fn format_path_tally(t: &[(SchedulerPath, u32); 4]) -> String {
    t.iter()
        .filter(|(_, n)| *n > 0)
        .map(|(p, n)| format!("{}={}", p.short(), n))
        .collect::<Vec<_>>()
        .join("  ")
}

pub(super) fn anchor_source_tally(arrivals: &[ChunkArrivalStat]) -> [(AnchorSource, u32); 3] {
    let mut t = [
        (AnchorSource::ObservedCollection, 0u32),
        (AnchorSource::UploadMinusMedian, 0),
        (AnchorSource::UploadMinusDefault, 0),
    ];
    for a in arrivals {
        if let Some(s) = a.anchor_source {
            for slot in t.iter_mut() {
                if slot.0 == s {
                    slot.1 += 1;
                }
            }
        }
    }
    t
}

pub(super) fn format_anchor_tally(t: &[(AnchorSource, u32); 3]) -> String {
    t.iter()
        .filter(|(_, n)| *n > 0)
        .map(|(s, n)| format!("{}={}", s.short(), n))
        .collect::<Vec<_>>()
        .join("  ")
}
