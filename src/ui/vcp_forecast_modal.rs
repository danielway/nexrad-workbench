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
use crate::core::timing::{AnchorSource, IntervalCase, PhysicsBreakdown, SchedulerPath};
use crate::core::{
    BucketKey, ChunkArrivalStat, ForecastTimingLabel, SweepForecast, SweepStatus,
    VolumeForecastSnapshot,
};
use crate::state::AppState;
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
            let snap = crate::core::derive_volume_forecast(
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::core::RateSource;
    use wasm_bindgen_test::wasm_bindgen_test;

    // ── builders ────────────────────────────────────────────────────────

    /// Default-ish arrival; callers tweak the few fields the helper reads.
    fn arrival(sequence: u32, success_at: f64) -> ChunkArrivalStat {
        ChunkArrivalStat::minimal_for_test(sequence, success_at)
    }

    fn bucket(chunk_type: &'static str, waveform: &'static str) -> BucketKey {
        BucketKey {
            chunk_type,
            waveform,
            channel: "RP",
            first_in_sweep: false,
        }
    }

    fn sweep(status: SweepStatus) -> SweepForecast {
        SweepForecast {
            elev_number: 1,
            elev_angle: 0.5,
            waveform: "CS".to_string(),
            azimuth_rate_used: 20.0,
            rate_source: RateSource::VcpMessage,
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

    fn snapshot(sweeps: Vec<SweepForecast>) -> VolumeForecastSnapshot {
        VolumeForecastSnapshot {
            vcp_number: 212,
            vcp_name: None,
            is_clear_air: false,
            volume_start: 500.0,
            predicted_volume_end: 800.0,
            actual_volume_end: None,
            expected_elevation_count: 14,
            sweeps,
            inter_volume_gap_secs: None,
            predicted_inter_volume_gap_secs: None,
        }
    }

    // ── total_empty_polls ──────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn total_empty_polls_sums_each_arrival() {
        let mut a = arrival(1, 10.0);
        a.empty_polls = 2;
        let mut b = arrival(2, 11.0);
        b.empty_polls = 3;
        let c = arrival(3, 12.0); // 0
        assert_eq!(total_empty_polls(&[a, b, c]), 5);
    }

    #[wasm_bindgen_test]
    fn total_empty_polls_empty_slice_is_zero() {
        assert_eq!(total_empty_polls(&[]), 0);
    }

    // ── fmt_elev ───────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn fmt_elev_full_triple_shows_one_based_index() {
        let mut a = arrival(1, 0.0);
        a.elevation_number = Some(3);
        a.chunk_index_in_sweep = Some(0); // displayed as 0+1 = 1
        a.chunks_in_sweep = Some(3);
        assert_eq!(fmt_elev(&a), "3 (1/3)".to_string());
    }

    #[wasm_bindgen_test]
    fn fmt_elev_elev_only_when_indices_absent() {
        let mut a = arrival(1, 0.0);
        a.elevation_number = Some(7);
        a.chunk_index_in_sweep = None;
        a.chunks_in_sweep = None;
        assert_eq!(fmt_elev(&a), "7".to_string());
    }

    #[wasm_bindgen_test]
    fn fmt_elev_dash_when_no_elevation() {
        let a = arrival(1, 0.0); // elevation_number None
        assert_eq!(fmt_elev(&a), "\u{2014}".to_string());
    }

    // ── fmt_physics ────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn fmt_physics_none_is_dash() {
        assert_eq!(fmt_physics(None), "\u{2014}".to_string());
    }

    #[wasm_bindgen_test]
    fn fmt_physics_intra_with_chunk_duration() {
        let b = PhysicsBreakdown {
            case: IntervalCase::IntraSweep,
            total_secs: 99.0,
            chunk_duration_secs: Some(4.0),
            inter_sweep_gap_secs: None,
            waveform_penalty_secs: None,
        };
        // Uses chunk_duration when present, not total_secs.
        assert_eq!(fmt_physics(Some(&b)), "intra 4.00s".to_string());
    }

    #[wasm_bindgen_test]
    fn fmt_physics_intra_falls_back_to_total_when_no_chunk_dur() {
        let b = PhysicsBreakdown {
            case: IntervalCase::IntraSweep,
            total_secs: 2.5,
            chunk_duration_secs: None,
            inter_sweep_gap_secs: None,
            waveform_penalty_secs: None,
        };
        assert_eq!(fmt_physics(Some(&b)), "intra ~2.50s".to_string());
    }

    #[wasm_bindgen_test]
    fn fmt_physics_inter_sweep_subtracts_wf_from_gap() {
        // gap=2.0, wf=0.5 → displayed g = gap-wf = 1.50; wf=0.5; ch=3.0; total.
        let b = PhysicsBreakdown {
            case: IntervalCase::InterSweep,
            total_secs: 5.0,
            chunk_duration_secs: Some(3.0),
            inter_sweep_gap_secs: Some(2.0),
            waveform_penalty_secs: Some(0.5),
        };
        assert_eq!(
            fmt_physics(Some(&b)),
            "is g=1.50 (wf=0.5) ch=3.00 \u{2192} 5.00s".to_string()
        );
    }

    #[wasm_bindgen_test]
    fn fmt_physics_inter_sweep_treats_missing_terms_as_zero() {
        let b = PhysicsBreakdown {
            case: IntervalCase::InterSweep,
            total_secs: 1.0,
            chunk_duration_secs: None,
            inter_sweep_gap_secs: None,
            waveform_penalty_secs: None,
        };
        assert_eq!(
            fmt_physics(Some(&b)),
            "is g=0.00 (wf=0.0) ch=0.00 \u{2192} 1.00s".to_string()
        );
    }

    #[wasm_bindgen_test]
    fn fmt_physics_inter_volume_uses_total() {
        let b = PhysicsBreakdown {
            case: IntervalCase::InterVolume,
            total_secs: 8.5,
            chunk_duration_secs: None,
            inter_sweep_gap_secs: None,
            waveform_penalty_secs: None,
        };
        assert_eq!(fmt_physics(Some(&b)), "inter_vol 8.50s".to_string());
    }

    // ── count_statuses ─────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn count_statuses_tallies_each_variant() {
        let snap = snapshot(vec![
            sweep(SweepStatus::Complete),
            sweep(SweepStatus::Complete),
            sweep(SweepStatus::InProgress {
                radials_received: 1,
                chunks_received: 1,
                chunks_expected: Some(3),
            }),
            sweep(SweepStatus::Future),
        ]);
        let (complete, in_progress, future) = count_statuses(&snap);
        assert_eq!(complete, 2);
        assert_eq!(in_progress, 1);
        assert_eq!(future, 1);
    }

    #[wasm_bindgen_test]
    fn count_statuses_all_zero_when_no_sweeps() {
        let snap = snapshot(vec![]);
        let counts = count_statuses(&snap);
        assert!(counts == (0usize, 0usize, 0usize));
    }

    // ── stats_on ───────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn stats_on_empty_is_none() {
        assert!(stats_on(&[]).is_none());
    }

    #[wasm_bindgen_test]
    fn stats_on_odd_count_median_is_middle() {
        // values [1, -3, 2] → mean 0, sorted [-3,1,2] median 1, max_abs 3.
        let (mean, median, max_abs) = stats_on(&[1.0, -3.0, 2.0]).expect("some");
        assert!((mean - 0.0).abs() < 1e-9);
        assert!((median - 1.0).abs() < 1e-9);
        assert!((max_abs - 3.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn stats_on_even_count_median_is_average_of_middle_pair() {
        // [4,1,3,2] → mean 2.5, sorted [1,2,3,4] median (2+3)/2 = 2.5, max_abs 4.
        let (mean, median, max_abs) = stats_on(&[4.0, 1.0, 3.0, 2.0]).expect("some");
        assert!((mean - 2.5).abs() < 1e-9);
        assert!((median - 2.5).abs() < 1e-9);
        assert!((max_abs - 4.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn stats_on_max_abs_picks_largest_magnitude_negative() {
        // single negative value: mean = median = -5, max_abs = 5.
        let (mean, median, max_abs) = stats_on(&[-5.0]).expect("some");
        assert!((mean + 5.0).abs() < 1e-9);
        assert!((median + 5.0).abs() < 1e-9);
        assert!((max_abs - 5.0).abs() < 1e-9);
    }

    // ── median_of ──────────────────────────────────────────────────────

    #[wasm_bindgen_test]
    fn median_of_empty_is_none() {
        assert!(median_of(vec![]).is_none());
    }

    #[wasm_bindgen_test]
    fn median_of_odd_returns_middle() {
        let m = median_of(vec![3.0, 1.0, 2.0]).expect("some");
        assert!((m - 2.0).abs() < 1e-9);
    }

    #[wasm_bindgen_test]
    fn median_of_even_returns_mean_of_middle_two() {
        let m = median_of(vec![1.0, 2.0, 3.0, 4.0]).expect("some");
        assert!((m - 2.5).abs() < 1e-9);
    }

    // ── collect_interval_errors_ms ─────────────────────────────────────

    #[wasm_bindgen_test]
    fn collect_interval_errors_ms_uses_consecutive_pairs() {
        // a0: collection 100. a1: collection 103, predicted_wait 2.0.
        // interval = 3s; error_ms = (3 - 2) * 1000 = 1000.
        let mut a0 = arrival(1, 0.0);
        a0.collection_time_secs = Some(100.0);
        let mut a1 = arrival(2, 0.0);
        a1.collection_time_secs = Some(103.0);
        a1.predicted_wait_secs = Some(2.0);
        let out = collect_interval_errors_ms(&[a0, a1]);
        assert_eq!(out.len(), 1);
        assert!((out[0] - 1000.0).abs() < 1e-6);
    }

    #[wasm_bindgen_test]
    fn collect_interval_errors_ms_skips_when_data_missing() {
        // a1 has no predicted_wait_secs → interval_error_ms returns None.
        let mut a0 = arrival(1, 0.0);
        a0.collection_time_secs = Some(100.0);
        let mut a1 = arrival(2, 0.0);
        a1.collection_time_secs = Some(103.0);
        // predicted_wait_secs left None
        assert!(collect_interval_errors_ms(&[a0, a1]).is_empty());
    }

    #[wasm_bindgen_test]
    fn collect_interval_errors_ms_single_arrival_has_no_pairs() {
        let a0 = arrival(1, 0.0);
        assert!(collect_interval_errors_ms(&[a0]).is_empty());
    }

    // ── compute_per_bucket_stats ───────────────────────────────────────

    #[wasm_bindgen_test]
    fn compute_per_bucket_stats_skips_arrivals_without_bucket() {
        let a = arrival(1, 0.0); // bucket_key None
        assert!(compute_per_bucket_stats(&[a]).is_empty());
    }

    #[wasm_bindgen_test]
    fn compute_per_bucket_stats_aggregates_lag_and_counts() {
        // Two arrivals in the same bucket; lags 100 and 200 → median 150, n_lag 2.
        let mut a0 = arrival(1, 0.0);
        a0.bucket_key = Some(bucket("I", "CS"));
        a0.availability_lag_ms = Some(100);
        let mut a1 = arrival(2, 0.0);
        a1.bucket_key = Some(bucket("I", "CS"));
        a1.availability_lag_ms = Some(200);
        let rows = compute_per_bucket_stats(&[a0, a1]);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.n_lag, 2);
        // n = max(pred_errs=0, lags=2, waits=0, 1) = 2.
        assert_eq!(row.n, 2);
        let med = row.median_lag_ms.expect("median lag");
        assert!((med - 150.0).abs() < 1e-9);
        // No prediction errors or waits were recorded.
        assert!(row.median_pred_err_ms.is_none());
        assert!(row.median_wait_after_empty_ms.is_none());
    }

    #[wasm_bindgen_test]
    fn compute_per_bucket_stats_separate_buckets_sorted_by_key() {
        // Different waveforms → distinct bucket keys; BTreeMap keys sort
        // "I|B|RP|F" before "I|CS|RP|F" (uppercase 'B' < 'C').
        let mut a0 = arrival(1, 0.0);
        a0.bucket_key = Some(bucket("I", "CS"));
        let mut a1 = arrival(2, 0.0);
        a1.bucket_key = Some(bucket("I", "B"));
        let rows = compute_per_bucket_stats(&[a0, a1]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].bucket.short(), "I|B|RP|F".to_string());
        assert_eq!(rows[1].bucket.short(), "I|CS|RP|F".to_string());
        // No measurable samples → n defaults to the floor of 1.
        assert_eq!(rows[0].n, 1);
    }

    // ── scheduler_path_tally / format_path_tally ───────────────────────

    #[wasm_bindgen_test]
    fn scheduler_path_tally_counts_by_variant() {
        let mut a0 = arrival(1, 0.0);
        a0.scheduler_path = Some(SchedulerPath::Physics);
        let mut a1 = arrival(2, 0.0);
        a1.scheduler_path = Some(SchedulerPath::Physics);
        let mut a2 = arrival(3, 0.0);
        a2.scheduler_path = Some(SchedulerPath::Blended);
        let a3 = arrival(4, 0.0); // None — ignored
        let t = scheduler_path_tally(&[a0, a1, a2, a3]);
        // Fixed order: StartConstant, Blended, Physics, Legacy.
        assert!(t[0].0 == SchedulerPath::StartConstant && t[0].1 == 0);
        assert!(t[1].0 == SchedulerPath::Blended && t[1].1 == 1);
        assert!(t[2].0 == SchedulerPath::Physics && t[2].1 == 2);
        assert!(t[3].0 == SchedulerPath::Legacy && t[3].1 == 0);
    }

    #[wasm_bindgen_test]
    fn format_path_tally_omits_zero_buckets() {
        let t = [
            (SchedulerPath::StartConstant, 0u32),
            (SchedulerPath::Blended, 1),
            (SchedulerPath::Physics, 2),
            (SchedulerPath::Legacy, 0),
        ];
        // Only non-zero entries, joined by two spaces; short codes blend/phys.
        assert_eq!(format_path_tally(&t), "blend=1  phys=2".to_string());
    }

    #[wasm_bindgen_test]
    fn format_path_tally_all_zero_is_empty_string() {
        let t = [
            (SchedulerPath::StartConstant, 0u32),
            (SchedulerPath::Blended, 0),
            (SchedulerPath::Physics, 0),
            (SchedulerPath::Legacy, 0),
        ];
        assert_eq!(format_path_tally(&t), String::new());
    }

    // ── anchor_source_tally / format_anchor_tally ──────────────────────

    #[wasm_bindgen_test]
    fn anchor_source_tally_counts_by_variant() {
        let mut a0 = arrival(1, 0.0);
        a0.anchor_source = Some(AnchorSource::ObservedCollection);
        let mut a1 = arrival(2, 0.0);
        a1.anchor_source = Some(AnchorSource::UploadMinusDefault);
        let a2 = arrival(3, 0.0); // None — ignored
        let t = anchor_source_tally(&[a0, a1, a2]);
        // Fixed order: ObservedCollection, UploadMinusMedian, UploadMinusDefault.
        assert!(t[0].0 == AnchorSource::ObservedCollection && t[0].1 == 1);
        assert!(t[1].0 == AnchorSource::UploadMinusMedian && t[1].1 == 0);
        assert!(t[2].0 == AnchorSource::UploadMinusDefault && t[2].1 == 1);
    }

    #[wasm_bindgen_test]
    fn format_anchor_tally_omits_zero_buckets() {
        let t = [
            (AnchorSource::ObservedCollection, 2u32),
            (AnchorSource::UploadMinusMedian, 0),
            (AnchorSource::UploadMinusDefault, 1),
        ];
        // short codes: obs / default.
        assert_eq!(format_anchor_tally(&t), "obs=2  default=1".to_string());
    }

    #[wasm_bindgen_test]
    fn format_anchor_tally_all_zero_is_empty() {
        let t = [
            (AnchorSource::ObservedCollection, 0u32),
            (AnchorSource::UploadMinusMedian, 0),
            (AnchorSource::UploadMinusDefault, 0),
        ];
        assert_eq!(format_anchor_tally(&t), String::new());
    }

    // ── format_time (deterministic constructor, not Date::now) ─────────

    #[wasm_bindgen_test]
    fn format_time_epoch_renders_iso_date_and_time() {
        // secs=0 → 1970-01-01T00:00:00.000Z → "1970-01-01 00:00:00Z".
        assert_eq!(format_time(0.0), "1970-01-01 00:00:00Z".to_string());
    }

    #[wasm_bindgen_test]
    fn format_time_known_instant_drops_subsecond_and_appends_z() {
        // 2001-09-09T01:46:40Z is exactly 1_000_000_000 unix seconds.
        assert_eq!(
            format_time(1_000_000_000.0),
            "2001-09-09 01:46:40Z".to_string()
        );
    }
}
