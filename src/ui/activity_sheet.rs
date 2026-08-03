//! The activity sheet — the full acquisition-transparency surface (spec §5, §10).
//!
//! Opened by the ambient activity chip in either layout. This is the *one*
//! place a user goes to answer "what is it downloading, how much is left, and
//! how fast is it going?". It absorbs what used to be three separate dev-only
//! surfaces (the pipeline lamps, the acquisition drawer, the network log), with
//! the deep diagnostics kept behind a collapsed `Details` disclosure so a
//! casual viewer never meets them (spec §14's disclosure ladder).
//!
//! Everything here is a 1:1 projection of [`crate::core::activity::ActivityVm`]
//! plus intent emission — no counting, no filtering, no state mutation. If a
//! number looks wrong, the bug is in `core::activity`, where it is tested
//! headlessly.
//!
//! Built as a modal [`Layer`] (registered in both layout slices) so it works
//! identically on desktop and mobile via the shared modal set — mobile has no
//! bottom panel, so a side drawer wouldn't reach it.

use super::colors::{acquisition as acq_colors, ui as ui_colors};
use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::activity::{
    ActivityDetailVm, ActivityRow, ActivityStage, ActivityState, ActivityVm, NetworkRow, RowStatus,
};
use crate::core::Intent;
use crate::state::{format_bytes, AppState};
use eframe::egui::{self, RichText, ScrollArea, Vec2};
use egui_phosphor::regular as icons;

pub(super) struct ActivitySheetLayer;

impl Layer for ActivitySheetLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        // Above the range-download modal (35), below the inspector (38) so a
        // tap-to-fetch from the inspector can stack visually over the sheet.
        37
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.activity_sheet_open
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_activity_sheet(ctx.ctx, ctx.state, ctx.activity_vm, ctx.chrome);
    }
}

/// Format a transfer rate for display.
fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec.max(0.0) as u64))
}

/// Compact age string for the recent-request list ("12s", "5m", "2h").
fn format_age(age_ms: f64) -> String {
    let secs = (age_ms / 1000.0).max(0.0);
    if secs < 60.0 {
        format!("{:.0}s", secs)
    } else if secs < 3600.0 {
        format!("{:.0}m", secs / 60.0)
    } else {
        format!("{:.0}h", secs / 3600.0)
    }
}

fn draw_activity_sheet(
    ctx: &egui::Context,
    state: &mut AppState,
    vm: &ActivityVm,
    chrome: &mut crate::subsystem::Chrome,
) {
    if super::modal_helper::modal_backdrop(ctx, "activity_sheet_backdrop", 150) {
        chrome.activity_sheet_open = false;
        return;
    }

    let dark = state.is_dark;

    egui::Window::new(format!("{} Activity", icons::DOWNLOAD_SIMPLE))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(440.0)
        .max_height(ctx.input(|i| i.viewport_rect().height()) * 0.8)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_min_width(380.0);

            render_header(ui, state, vm, dark);
            ui.add_space(4.0);
            render_stage_strip(ui, vm, dark);

            ui.separator();
            render_operation_list(ui, state, vm, dark);

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
            render_policy_section(ui, state, dark);

            ui.add_space(6.0);
            ui.separator();
            render_details(ui, state, vm, chrome, dark);

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        chrome.activity_sheet_open = false;
                    }
                });
            });
        });
}

/// Headline + throughput + queue controls.
fn render_header(ui: &mut egui::Ui, state: &mut AppState, vm: &ActivityVm, dark: bool) {
    ui.horizontal(|ui| {
        let mut headline = vm.headline.label.to_string();
        if let Some(count) = vm.headline.count {
            headline = format!(
                "{headline} \u{00b7} {count} scan{}",
                if count == 1 { "" } else { "s" }
            );
        }
        ui.label(
            RichText::new(headline)
                .size(13.0)
                .strong()
                .color(ui_colors::value(dark)),
        );

        if vm.failed.count > 0 {
            ui.label(
                RichText::new(format!("\u{00b7} {} failed", vm.failed.count))
                    .size(12.0)
                    .color(acq_colors::FAILED),
            );
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Pause/resume is meaningful only while there is work to hold.
            let toggle = match vm.state {
                ActivityState::Paused { .. } => {
                    Some((format!("{} Resume", icons::PLAY), Intent::ResumeQueue))
                }
                ActivityState::Downloading { .. } => {
                    Some((format!("{} Pause", icons::PAUSE), Intent::PauseQueue))
                }
                _ => None,
            };
            if let Some((label, intent)) = toggle {
                if ui.small_button(label).clicked() {
                    state.push_command(intent);
                }
            }

            // Throughput. Absent means "nothing moving" — rendered as an
            // em dash, never as a fabricated 0 B/s.
            let text = match vm.throughput {
                Some(t) => format_rate(t.bytes_per_sec),
                None => "\u{2014}".to_string(),
            };
            ui.label(
                RichText::new(text)
                    .size(11.0)
                    .monospace()
                    .color(ui_colors::label(dark)),
            )
            .on_hover_text("Transfer rate over the last 10 seconds");
        });
    });
}

/// The stage strip: `Queued n › Downloading n › Processing n › Finishing n`.
///
/// This is what replaced the dev-only DL/PROC/GPU lamps — same idea, real
/// counts instead of booleans, and visible to everyone.
fn render_stage_strip(ui: &mut egui::Ui, vm: &ActivityVm, dark: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (i, stage) in vm.stages.iter().enumerate() {
            if i > 0 {
                ui.label(
                    RichText::new("\u{203A}")
                        .size(10.0)
                        .color(ui_colors::label(dark)),
                );
            }
            render_stage(ui, stage, dark);
        }
    });
}

fn render_stage(ui: &mut egui::Ui, stage: &ActivityStage, dark: bool) {
    let color = if stage.active {
        ui_colors::ACTIVE
    } else {
        ui_colors::label(dark)
    };
    // Count first, label second: the number is the information, the word is
    // the key to reading it.
    let text = format!("{} {}", stage.count, stage.kind.label());
    ui.label(RichText::new(text).size(10.0).monospace().color(color))
        .on_hover_text(stage_hint(stage));
}

fn stage_hint(stage: &ActivityStage) -> &'static str {
    use crate::core::activity::ActivityStageKind as K;
    match stage.kind {
        K::Queued => "Scans waiting for a free download slot",
        K::Downloading => "Scans being fetched from the archive now",
        K::Processing => "Jobs sent to the decode workers, awaiting a result",
        K::Finishing => "Decoded and stored — waiting for the timeline to pick them up",
    }
}

/// The scrollable list of archive downloads, newest first.
fn render_operation_list(ui: &mut egui::Ui, state: &mut AppState, vm: &ActivityVm, dark: bool) {
    let label_color = ui_colors::label(dark);
    let value_color = ui_colors::value(dark);

    if vm.failed.count > 1 {
        ui.horizontal(|ui| {
            if ui
                .small_button(format!("{} Retry all failed", icons::ARROW_CLOCKWISE))
                .clicked()
            {
                state.push_command(Intent::RetryAllFailed);
            }
        });
        ui.add_space(2.0);
    }

    ScrollArea::vertical()
        .auto_shrink([false, true])
        .max_height(260.0)
        .show(ui, |ui| {
            if vm.rows.is_empty() {
                ui.label(
                    RichText::new(
                        "No downloads yet. Scrub or select a range and scans fetch automatically.",
                    )
                    .size(11.0)
                    .italics()
                    .color(label_color),
                );
                return;
            }

            for row in &vm.rows {
                ui.horizontal(|ui| {
                    let (icon, color) = row_icon(&row.status);
                    ui.label(RichText::new(icon).size(11.0).color(color));
                    ui.label(RichText::new(&row.title).size(11.0).color(value_color));

                    if let Some(bytes) = row.bytes {
                        let prefix = if row.bytes_estimated { "~" } else { "" };
                        ui.label(
                            RichText::new(format!("{prefix}{}", format_bytes(bytes)))
                                .size(10.0)
                                .color(label_color),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        render_row_actions(ui, state, row, label_color);
                    });
                });
            }
        });
}

fn row_icon(status: &RowStatus) -> (&'static str, egui::Color32) {
    match status {
        RowStatus::Downloading => (icons::SPINNER, acq_colors::ACTIVE),
        RowStatus::Queued { .. } => (icons::CLOCK, acq_colors::QUEUED),
        RowStatus::Completed { .. } => (icons::CHECK_CIRCLE, acq_colors::COMPLETED),
        RowStatus::Failed { .. } => (icons::WARNING, acq_colors::FAILED),
        RowStatus::Cancelled => (icons::MINUS_CIRCLE, acq_colors::CANCELLED),
    }
}

fn render_row_actions(
    ui: &mut egui::Ui,
    state: &mut AppState,
    row: &ActivityRow,
    label_color: egui::Color32,
) {
    match &row.status {
        RowStatus::Downloading => {
            ui.label(
                RichText::new("downloading")
                    .size(10.0)
                    .italics()
                    .color(acq_colors::ACTIVE),
            );
        }
        RowStatus::Queued { position } => {
            if row.can_cancel && ui.small_button(icons::X).on_hover_text("Cancel").clicked() {
                state.push_command(Intent::CancelOperation(row.id));
            }
            if row.can_reorder {
                if ui
                    .small_button(icons::CARET_DOWN)
                    .on_hover_text("Move down")
                    .clicked()
                {
                    state.push_command(Intent::ReorderOperation(row.id, 1));
                }
                if ui
                    .small_button(icons::CARET_UP)
                    .on_hover_text("Move up")
                    .clicked()
                {
                    state.push_command(Intent::ReorderOperation(row.id, -1));
                }
            }
            ui.label(
                RichText::new(format!("#{position}"))
                    .size(10.0)
                    .color(label_color),
            )
            .on_hover_text("Position in the download queue");
        }
        RowStatus::Failed { error } => {
            if row.can_retry
                && ui
                    .small_button(format!("{} Retry", icons::ARROW_CLOCKWISE))
                    .clicked()
            {
                state.push_command(Intent::RetryFailed(row.id));
            }
            if ui.small_button("Skip").clicked() {
                state.push_command(Intent::SkipFailed(row.id));
            }
            ui.label(RichText::new("failed").size(10.0).color(acq_colors::FAILED))
                .on_hover_text(error);
        }
        RowStatus::Completed { duration_ms } => {
            ui.label(
                RichText::new(format!("{:.1}s", duration_ms / 1000.0))
                    .size(10.0)
                    .color(label_color),
            );
        }
        RowStatus::Cancelled => {
            ui.label(
                RichText::new("cancelled")
                    .size(10.0)
                    .color(acq_colors::CANCELLED),
            );
        }
    }
}

/// The acquisition policy toggles (spec §10 / alignment §5). Both persist via
/// `UserPreferences` on the next throttled save.
fn render_policy_section(ui: &mut egui::Ui, state: &mut AppState, dark: bool) {
    ui.label(
        RichText::new("Data saving")
            .size(11.0)
            .strong()
            .color(ui_colors::label(dark)),
    );
    ui.add_space(2.0);

    let mut autofetch = state.autofetch_while_scrubbing;
    if ui
        .checkbox(&mut autofetch, "Auto-fetch while scrubbing")
        .on_hover_text(
            "Download scans automatically as you scrub and seek. Turn off to save data — \
             you can still fetch a selected range or fetch from the scan inspector.",
        )
        .changed()
    {
        state.push_command(Intent::SetAutofetchWhileScrubbing(autofetch));
    }

    let mut pause_stream = state.pause_stream_while_reviewing;
    if ui
        .checkbox(&mut pause_stream, "Pause live stream while reviewing")
        .on_hover_text(
            "When you scrub away from the live edge, stop the background live stream \
             instead of letting it keep downloading.",
        )
        .changed()
    {
        state.push_command(Intent::SetPauseStreamWhileReviewing(pause_stream));
    }
}

/// The `Details` disclosure: session totals and the recent-request log for
/// everyone, plus deep timing diagnostics when dev mode is on.
fn render_details(
    ui: &mut egui::Ui,
    state: &mut AppState,
    vm: &ActivityVm,
    chrome: &mut crate::subsystem::Chrome,
    dark: bool,
) {
    let label_color = ui_colors::label(dark);
    let value_color = ui_colors::value(dark);
    let mut open_vcp_forecast = false;

    let header = egui::CollapsingHeader::new(RichText::new("Details").size(11.0).strong())
        .open(Some(chrome.activity_details_open))
        .show(ui, |ui| {
            egui::Grid::new("activity_details_grid")
                .num_columns(2)
                .spacing([10.0, 2.0])
                .show(ui, |ui| {
                    detail_row(
                        ui,
                        "Requests",
                        vm.session.requests.to_string(),
                        label_color,
                        value_color,
                    );
                    if vm.session.failed_requests > 0 {
                        detail_row(
                            ui,
                            "Failed",
                            vm.session.failed_requests.to_string(),
                            label_color,
                            acq_colors::FAILED,
                        );
                    }
                    detail_row(
                        ui,
                        "Transferred",
                        format_bytes(vm.session.bytes),
                        label_color,
                        value_color,
                    );
                    detail_row(
                        ui,
                        "Cache",
                        format_bytes(vm.session.cache_bytes),
                        label_color,
                        value_color,
                    );

                    if let Some(detail) = &vm.detail {
                        render_dev_rows(ui, detail, label_color, value_color);
                    }
                });

            // The VCP forecast diagnostics, previously reached from the
            // Performance modal. Dev-only and deliberately last.
            if let Some(detail) = &vm.detail {
                if ui
                    .add_enabled(
                        detail.vcp_forecast_available,
                        egui::Button::new("VCP forecast diagnostics").small(),
                    )
                    .on_hover_text(
                        "Compare VCP-based predictions against observed sweeps for the \
                         current live volume",
                    )
                    .on_disabled_hover_text("Available after a live VCP message has been received")
                    .clicked()
                {
                    open_vcp_forecast = true;
                }
            }

            ui.add_space(4.0);
            render_network_list(ui, vm, label_color, value_color);
        });

    // The disclosure's header is the click target; route it through an intent
    // so the open flag stays real state rather than egui memory.
    if header.header_response.clicked() {
        state.push_command(Intent::SetActivityDetailsOpen(
            !chrome.activity_details_open,
        ));
    }
    if open_vcp_forecast {
        chrome.vcp_forecast_open = true;
    }
}

fn detail_row(
    ui: &mut egui::Ui,
    label: &str,
    value: String,
    label_color: egui::Color32,
    value_color: egui::Color32,
) {
    ui.label(RichText::new(label).size(10.0).color(label_color));
    ui.label(RichText::new(value).size(10.0).color(value_color));
    ui.end_row();
}

fn render_dev_rows(
    ui: &mut egui::Ui,
    detail: &ActivityDetailVm,
    label_color: egui::Color32,
    value_color: egui::Color32,
) {
    ui.label(
        RichText::new("HTTP in flight")
            .size(10.0)
            .color(label_color),
    );
    ui.label(
        RichText::new(detail.http_in_flight.to_string())
            .size(10.0)
            .color(value_color),
    )
    .on_hover_text("Raw HTTP requests open right now, including archive listings");
    ui.end_row();

    let w = detail.worker;
    ui.label(RichText::new("Worker jobs").size(10.0).color(label_color));
    ui.label(
        RichText::new(format!(
            "{} (ingest {} \u{00b7} chunk {} \u{00b7} render {} \u{00b7} live {} \u{00b7} vol {})",
            w.total(),
            w.ingest,
            w.chunk_ingest,
            w.render,
            w.render_live,
            w.volume
        ))
        .size(10.0)
        .color(value_color),
    )
    .on_hover_text("Jobs sent to the decode workers whose result hasn't returned yet");
    ui.end_row();

    if detail.unavailable > 0 {
        detail_row(
            ui,
            "Unavailable tilts",
            detail.unavailable.to_string(),
            label_color,
            value_color,
        );
    }
    if let Some(ms) = detail.avg_fetch_ms {
        detail_row(
            ui,
            "Fetch latency",
            format!("{ms:.0} ms avg"),
            label_color,
            value_color,
        );
    }
    if let Some(ms) = detail.avg_processing_ms {
        detail_row(
            ui,
            "Processing",
            format!("{ms:.0} ms avg"),
            label_color,
            value_color,
        );
    }
    if let Some(ms) = detail.avg_render_ms {
        detail_row(
            ui,
            "Render",
            format!("{ms:.0} ms avg"),
            label_color,
            value_color,
        );
    }
    for (label, ms) in detail.ingest_phases.iter().flatten() {
        detail_row(ui, label, format!("{ms:.1} ms"), label_color, value_color);
    }
    for (label, ms) in detail.render_phases.iter().flatten() {
        detail_row(ui, label, format!("{ms:.1} ms"), label_color, value_color);
    }
    if let Some(fps) = detail.fps {
        detail_row(
            ui,
            "Frame rate",
            format!("{fps:.0} fps"),
            label_color,
            value_color,
        );
    }
    detail_row(
        ui,
        "Cross-origin isolated",
        if detail.cross_origin_isolated {
            "active".into()
        } else {
            "inactive".into()
        },
        label_color,
        value_color,
    );
}

/// The recent-request log, absorbed from the old dev-only network modal.
fn render_network_list(
    ui: &mut egui::Ui,
    vm: &ActivityVm,
    label_color: egui::Color32,
    value_color: egui::Color32,
) {
    ui.label(
        RichText::new("Recent requests")
            .size(10.0)
            .strong()
            .color(label_color),
    );
    if vm.network.is_empty() {
        ui.label(
            RichText::new("No requests recorded yet.")
                .size(10.0)
                .italics()
                .color(label_color),
        );
        return;
    }

    ScrollArea::vertical()
        .id_salt("activity_network_list")
        .auto_shrink([false, true])
        .max_height(140.0)
        .show(ui, |ui| {
            egui::Grid::new("activity_network_grid")
                .num_columns(4)
                .spacing([10.0, 2.0])
                .striped(true)
                .show(ui, |ui| {
                    for row in &vm.network {
                        ui.label(
                            RichText::new(row.status.to_string())
                                .size(10.0)
                                .monospace()
                                .color(status_color(row)),
                        );
                        ui.label(RichText::new(&row.label).size(10.0).color(value_color));
                        ui.label(
                            RichText::new(format_bytes(row.bytes))
                                .size(10.0)
                                .color(label_color),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{:.0} ms \u{00b7} {}",
                                row.duration_ms,
                                format_age(row.age_ms)
                            ))
                            .size(10.0)
                            .color(label_color),
                        );
                        ui.end_row();
                    }
                });
        });
}

/// Hue for an HTTP status. Red is reserved for failure, per the accent rule.
fn status_color(row: &NetworkRow) -> egui::Color32 {
    if !row.ok || row.status == 0 || row.status >= 400 {
        acq_colors::FAILED
    } else if row.status >= 300 {
        egui::Color32::from_rgb(220, 180, 80)
    } else {
        ui_colors::SUCCESS
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn net(status: u16, ok: bool) -> NetworkRow {
        NetworkRow {
            label: "x".into(),
            status,
            ok,
            bytes: 1,
            duration_ms: 1.0,
            age_ms: 0.0,
        }
    }

    /// Rates are byte-formatted with a per-second suffix, and a negative rate
    /// (impossible, but cheap to guard) clamps rather than wrapping on the
    /// f64→u64 cast.
    #[wasm_bindgen_test]
    fn format_rate_appends_per_second_and_clamps() {
        assert!(format_rate(1024.0).ends_with("/s"));
        assert_eq!(format_rate(-5.0), format_rate(0.0));
    }

    /// Ages roll up through seconds, minutes and hours.
    #[wasm_bindgen_test]
    fn format_age_picks_a_unit() {
        assert_eq!(format_age(5_000.0), "5s");
        assert_eq!(format_age(120_000.0), "2m");
        assert_eq!(format_age(7_200_000.0), "2h");
        // Negative (clock skew) clamps to zero rather than rendering "-3s".
        assert_eq!(format_age(-3_000.0), "0s");
    }

    /// Red is reserved for failure: 4xx, 5xx, a zero status, and any not-ok
    /// response all take the failure tone; a healthy 2xx does not.
    #[wasm_bindgen_test]
    fn status_color_reserves_red_for_failures() {
        assert_eq!(status_color(&net(404, false)), acq_colors::FAILED);
        assert_eq!(status_color(&net(500, false)), acq_colors::FAILED);
        assert_eq!(status_color(&net(0, false)), acq_colors::FAILED);
        // A 2xx the worker still flagged as not-ok is a failure too.
        assert_eq!(status_color(&net(200, false)), acq_colors::FAILED);
        assert_eq!(status_color(&net(200, true)), ui_colors::SUCCESS);
        assert_ne!(status_color(&net(302, true)), acq_colors::FAILED);
    }

    /// Every row status maps to a distinct icon, so the list reads without
    /// relying on colour.
    #[wasm_bindgen_test]
    fn row_icons_are_distinct_per_status() {
        let statuses = [
            RowStatus::Queued { position: 1 },
            RowStatus::Downloading,
            RowStatus::Completed { duration_ms: 1.0 },
            RowStatus::Failed { error: "e".into() },
            RowStatus::Cancelled,
        ];
        let icons: Vec<&str> = statuses.iter().map(|s| row_icon(s).0).collect();
        for (i, a) in icons.iter().enumerate() {
            for b in icons.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    /// Every stage has a non-empty hint and label — the strip is the main
    /// explanation of what the pipeline is doing.
    #[wasm_bindgen_test]
    fn every_stage_has_a_hint_and_label() {
        use crate::core::activity::ActivityStageKind as K;
        for kind in [K::Queued, K::Downloading, K::Processing, K::Finishing] {
            let stage = ActivityStage {
                kind,
                count: 0,
                active: false,
            };
            assert!(!stage_hint(&stage).is_empty());
            assert!(!kind.label().is_empty());
        }
    }
}
