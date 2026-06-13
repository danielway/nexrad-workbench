//! User-facing acquisition queue sheet (spec §5, §10).
//!
//! The status chip near the transport (and the mobile top-bar acquiring
//! indicator) open this sheet. It is the *user* surface for acquisition
//! transparency — distinct from the dev-only acquisition drawer
//! (`acquisition_drawer.rs`), which keeps the Network tab + latency
//! diagnostics behind `dev_mode`. This sheet shows only what a normal user
//! needs: active / queued / recent downloads with size estimates and
//! cancel/retry, plus the two acquisition policy toggles (alignment §5:
//! "auto-fetch while scrubbing" and "pause live stream while reviewing"; the
//! spec's "Wi-Fi only" is intentionally dropped — not implementable in a
//! browser).
//!
//! Built as a modal [`Layer`] (registered in both layout slices) so it works
//! identically on desktop and mobile via the shared modal set — mobile has no
//! bottom panel, so a side drawer wouldn't reach it.

use super::colors::{acquisition as acq_colors, ui as ui_colors};
use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::state::{
    format_bytes, AcquisitionState, AppCommand, AppState, OperationKind, OperationStatus,
};
use crate::subsystem::{Acquisition, Chrome};
use eframe::egui::{self, RichText, ScrollArea, Vec2};
use egui_phosphor::regular as icons;

pub(super) struct QueueSheetLayer;

impl Layer for QueueSheetLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        // Above the range-download modal (35), below the inspector (38) so a
        // tap-to-fetch from the inspector can stack visually over the sheet.
        37
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.queue_sheet_open
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_queue_sheet(ctx.ctx, ctx.state, ctx.acquisition, ctx.chrome);
    }
}

/// Estimated byte size of an operation's download for the queue-sheet readout.
///
/// Completed operations carry their real transferred byte count. Active /
/// queued operations have no real size — the S3 listing in this client's
/// `nexrad-data` version exposes only file names, not sizes (so
/// `ArchiveFileMeta.size` is always 0), and per-sweep blob sizes live in IDB
/// behind an async read we don't add this phase. So in-flight/queued archive
/// downloads fall back to a flat per-scan estimate, the same idiom the
/// range-download modal uses. Returns `None` for kinds with no meaningful size
/// (listings, realtime chunks) or cancelled/failed ops.
fn operation_estimated_bytes(status: &OperationStatus, kind: &OperationKind) -> Option<u64> {
    match status {
        OperationStatus::Completed { bytes, .. } => Some(*bytes),
        OperationStatus::Queued | OperationStatus::Active => match kind {
            OperationKind::ArchiveDownload { .. } => Some(crate::AVG_SCAN_BYTES),
            _ => None,
        },
        OperationStatus::Failed { .. } | OperationStatus::Cancelled => None,
    }
}

/// Whether an operation belongs in the user-facing queue sheet's list. We show
/// archive downloads (the user's "downloads") in any state, and hide listings /
/// realtime-chunk bookkeeping (those are plumbing, surfaced in the dev drawer).
fn shows_in_queue_sheet(kind: &OperationKind) -> bool {
    matches!(kind, OperationKind::ArchiveDownload { .. })
}

fn draw_queue_sheet(
    ctx: &egui::Context,
    state: &mut AppState,
    acquisition: &mut Acquisition,
    chrome: &mut Chrome,
) {
    if super::modal_helper::modal_backdrop(ctx, "queue_sheet_backdrop", 150) {
        chrome.queue_sheet_open = false;
        return;
    }

    let dark = state.is_dark;
    let mut commands: Vec<AppCommand> = Vec::new();

    egui::Window::new(format!("{} Downloads", icons::DOWNLOAD_SIMPLE))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(420.0)
        .max_height(ctx.input(|i| i.viewport_rect().height()) * 0.8)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_min_width(360.0);

            // Header: aggregate counts + a manual pause/resume of the whole
            // queue. (Failures no longer pause the queue — alignment §5 — so
            // this is purely a user control now.)
            let active = acquisition.state.active_count();
            let queued = acquisition.state.queued_count();
            let failed = acquisition.state.failed_scan_starts().len();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{active} downloading \u{00b7} {queued} queued"))
                        .size(12.0)
                        .color(ui_colors::value(dark)),
                );
                if failed > 0 {
                    ui.label(
                        RichText::new(format!("\u{00b7} {failed} failed"))
                            .size(12.0)
                            .color(acq_colors::FAILED),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if acquisition.state.is_paused() {
                        if ui.small_button(format!("{} Resume", icons::PLAY)).clicked() {
                            commands.push(AppCommand::ResumeQueue);
                        }
                    } else if acquisition.state.has_active_operations()
                        && ui.small_button(format!("{} Pause", icons::PAUSE)).clicked()
                    {
                        commands.push(AppCommand::PauseQueue);
                    }
                });
            });

            ui.separator();

            render_operation_list(ui, acquisition, dark, &mut commands);

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);

            render_policy_section(ui, state, dark);

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        chrome.queue_sheet_open = false;
                    }
                });
            });
        });

    for cmd in commands {
        state.push_command(cmd);
    }
}

/// Render the scrollable list of archive-download operations, newest first.
fn render_operation_list(
    ui: &mut egui::Ui,
    acquisition: &mut Acquisition,
    dark: bool,
    commands: &mut Vec<AppCommand>,
) {
    let label_color = ui_colors::label(dark);
    let value_color = ui_colors::value(dark);

    let ops: Vec<_> = acquisition
        .state
        .operations
        .iter()
        .rev()
        .filter(|op| shows_in_queue_sheet(&op.kind))
        .cloned()
        .collect();

    ScrollArea::vertical()
        .auto_shrink([false, true])
        .max_height(260.0)
        .show(ui, |ui| {
            if ops.is_empty() {
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

            for op in &ops {
                ui.horizontal(|ui| {
                    let (icon, color) = match &op.status {
                        OperationStatus::Active => (icons::SPINNER, acq_colors::ACTIVE),
                        OperationStatus::Queued => (icons::CLOCK, acq_colors::QUEUED),
                        OperationStatus::Completed { .. } => {
                            (icons::CHECK_CIRCLE, acq_colors::COMPLETED)
                        }
                        OperationStatus::Failed { .. } => (icons::WARNING, acq_colors::FAILED),
                        OperationStatus::Cancelled => (icons::MINUS_CIRCLE, acq_colors::CANCELLED),
                    };
                    ui.label(RichText::new(icon).size(11.0).color(color));

                    let desc = AcquisitionState::operation_description(&op.kind);
                    ui.label(RichText::new(&desc).size(11.0).color(value_color));

                    // Size column: real bytes when completed, "~N MB" estimate
                    // otherwise.
                    if let Some(bytes) = operation_estimated_bytes(&op.status, &op.kind) {
                        let est = !matches!(op.status, OperationStatus::Completed { .. });
                        let prefix = if est { "~" } else { "" };
                        ui.label(
                            RichText::new(format!("{prefix}{}", format_bytes(bytes)))
                                .size(10.0)
                                .color(label_color),
                        );
                    }

                    // Status text + actions (right-aligned).
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| match &op.status {
                            OperationStatus::Active => {
                                ui.label(
                                    RichText::new("downloading")
                                        .size(10.0)
                                        .italics()
                                        .color(acq_colors::ACTIVE),
                                );
                            }
                            OperationStatus::Queued => {
                                if ui.small_button(icons::X).on_hover_text("Cancel").clicked() {
                                    commands.push(AppCommand::CancelOperation(op.id));
                                }
                                if ui
                                    .small_button(icons::CARET_DOWN)
                                    .on_hover_text("Move down")
                                    .clicked()
                                {
                                    commands.push(AppCommand::ReorderOperation(op.id, 1));
                                }
                                if ui
                                    .small_button(icons::CARET_UP)
                                    .on_hover_text("Move up")
                                    .clicked()
                                {
                                    commands.push(AppCommand::ReorderOperation(op.id, -1));
                                }
                            }
                            OperationStatus::Failed { error } => {
                                if ui
                                    .small_button(format!("{} Retry", icons::ARROW_CLOCKWISE))
                                    .clicked()
                                {
                                    commands.push(AppCommand::RetryFailed(op.id));
                                }
                                if ui.small_button("Skip").clicked() {
                                    commands.push(AppCommand::SkipFailed(op.id));
                                }
                                ui.label(
                                    RichText::new("failed").size(10.0).color(acq_colors::FAILED),
                                )
                                .on_hover_text(error);
                            }
                            OperationStatus::Completed { duration_ms, .. } => {
                                ui.label(
                                    RichText::new(format!("{:.1}s", duration_ms / 1000.0))
                                        .size(10.0)
                                        .color(label_color),
                                );
                            }
                            OperationStatus::Cancelled => {
                                ui.label(
                                    RichText::new("cancelled")
                                        .size(10.0)
                                        .color(acq_colors::CANCELLED),
                                );
                            }
                        },
                    );
                });
            }
        });
}

/// The acquisition policy toggles (spec §10 / alignment §5). Both persist via
/// `UserPreferences` on the next throttled save; we only flip the live
/// `AppState` fields here, matching how the rest of the prefs UI works.
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
        state.autofetch_while_scrubbing = autofetch;
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
        state.pause_stream_while_reviewing = pause_stream;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn download_kind() -> OperationKind {
        OperationKind::ArchiveDownload {
            site_id: "KDMX".to_string(),
            file_name: "KDMX_1000".to_string(),
            scan_start: 1000,
            scan_end: 1300,
        }
    }

    /// Completed ops report their real transferred bytes (no estimate).
    #[wasm_bindgen_test]
    fn completed_op_uses_real_bytes() {
        let s = OperationStatus::Completed {
            duration_ms: 1000.0,
            bytes: 7_654_321,
        };
        assert_eq!(
            operation_estimated_bytes(&s, &download_kind()),
            Some(7_654_321)
        );
    }

    /// Queued / active archive downloads fall back to the flat per-scan
    /// estimate (real S3 sizes aren't available to the client).
    #[wasm_bindgen_test]
    fn queued_and_active_archive_downloads_estimate_avg_scan_bytes() {
        for s in [OperationStatus::Queued, OperationStatus::Active] {
            assert_eq!(
                operation_estimated_bytes(&s, &download_kind()),
                Some(crate::AVG_SCAN_BYTES)
            );
        }
    }

    /// Failed / cancelled ops, and non-download kinds, carry no size readout.
    #[wasm_bindgen_test]
    fn no_estimate_for_failed_cancelled_or_non_download() {
        assert_eq!(
            operation_estimated_bytes(
                &OperationStatus::Failed { error: "x".into() },
                &download_kind()
            ),
            None
        );
        assert_eq!(
            operation_estimated_bytes(&OperationStatus::Cancelled, &download_kind()),
            None
        );
        let listing = OperationKind::ArchiveListing {
            site_id: "KDMX".into(),
            date: chrono::NaiveDate::from_ymd_opt(2024, 5, 1).unwrap(),
        };
        assert_eq!(
            operation_estimated_bytes(&OperationStatus::Queued, &listing),
            None
        );
    }

    /// Only archive downloads appear in the user-facing sheet; listing /
    /// realtime plumbing is hidden (it lives in the dev drawer).
    #[wasm_bindgen_test]
    fn only_archive_downloads_show_in_sheet() {
        assert!(shows_in_queue_sheet(&download_kind()));
        assert!(!shows_in_queue_sheet(&OperationKind::ArchiveListing {
            site_id: "KDMX".into(),
            date: chrono::NaiveDate::from_ymd_opt(2024, 5, 1).unwrap(),
        }));
        assert!(!shows_in_queue_sheet(&OperationKind::RealtimeChunk {
            site_id: "KDMX".into(),
            chunk_index: 1,
            is_start: true,
            is_end: false,
            scan_timestamp: 1000,
        }));
    }
}
