//! Scan inspector (spec §5, §6.3, §12): the per-scan volume breakdown.
//!
//! Opened by right-click (desktop) or long-press (touch) on a scan container.
//! Lists *every* sweep of the inspected volume — all tilts, including SAILS /
//! MRLE revisits — with its elevation angle, cache state, an (estimated) size,
//! per-chunk progress for the in-progress live volume, and a tap-to-fetch
//! affordance for cuts not yet downloaded. Plus a "Loop from here" action.
//!
//! The breakdown is read from the merged [`TimelineView`] — the SAME source the
//! strip and tooltips read — so the inspector can never disagree with the strip
//! about what is cached / collecting / available. It rebuilds the view from the
//! subsystems each frame (cheap; the view borrows the cache) and finds the
//! container whose `key_secs` matches the stored scan-start.

use super::layout::{Layer, LayerKind, LayoutCtx};
use super::timeline::format_timestamp_full;
use crate::core::{FrameCell, FrameCellState, FrameJoinInputs, ScanContainer, TimelineView};
use crate::state::{format_bytes, AppCommand, AppState};
use crate::subsystem::{Acquisition, Chrome, Live, Playback, Timeline};
use eframe::egui::{self, Color32, RichText, ScrollArea, Vec2};
use egui_phosphor::regular as icons;

pub(super) struct ScanInspectorLayer;

impl Layer for ScanInspectorLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        // Above the queue sheet (37): a tap-to-fetch from here may also open
        // the queue, and the inspector should stay on top.
        38
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.scan_inspector.is_some()
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_scan_inspector(
            ctx.ctx,
            ctx.state,
            ctx.timeline,
            ctx.live,
            ctx.playback,
            ctx.acquisition,
            ctx.chrome,
        );
    }
}

/// Tolerance (seconds) for matching the stored inspected scan-start against a
/// container's `key_secs` — the same join tolerance the strip uses, so the
/// inspector resolves to the same container the user clicked.
const MATCH_TOLERANCE_SECS: f64 = crate::core::SCAN_JOIN_TOLERANCE_SECS as f64;

/// Estimated bytes for one sweep's blob. Real per-sweep blob sizes live in IDB
/// behind an async read we don't add this phase; the volume estimate
/// ([`crate::AVG_SCAN_BYTES`]) divided across a typical ~10-sweep volume is a
/// reasonable per-cut figure. Tunable.
fn estimated_sweep_bytes() -> u64 {
    (crate::AVG_SCAN_BYTES / 10).max(1)
}

#[allow(clippy::too_many_arguments)]
fn draw_scan_inspector(
    ctx: &egui::Context,
    state: &mut AppState,
    timeline: &Timeline,
    live: &mut Live,
    playback: &mut Playback,
    acquisition: &Acquisition,
    chrome: &mut Chrome,
) {
    if super::modal_helper::modal_backdrop(ctx, "scan_inspector_backdrop", 150) {
        chrome.scan_inspector = None;
        return;
    }
    let Some(scan_start) = chrome.scan_inspector else {
        return;
    };

    // Rebuild the merged view and find the inspected container. We use
    // `tilt: None` so EVERY elevation becomes a frame cell (the full volume
    // breakdown), unlike the strip which filters to the selected tilt; the
    // per-cell state resolution (cached/in-flight/available/...) is identical.
    let view = TimelineView::build(
        &timeline.scans,
        &timeline.shadow_scan_boundaries,
        Some(&live.mode_state),
        live.radar_model.position.as_ref(),
    );

    let product = state.viz_state.product.to_worker_string();
    let mut in_flight_all = state.download_progress.in_flight_scans.clone();
    in_flight_all.extend_from_slice(&state.download_progress.active_scans);
    // Failed scan-starts come from the acquisition operations — the same source
    // the strip's failed-cell ticks use (so a retry clears it here too).
    let failed_secs = acquisition.state.failed_scan_starts();
    let active = state.viz_state.displayed.as_ref().map(|d| {
        (
            d.identity.scan_timestamp_secs(),
            d.identity.elevation_number,
        )
    });
    let join = FrameJoinInputs {
        queued: &state.download_progress.pending_scans,
        in_flight: &in_flight_all,
        failed: &failed_secs,
        product,
        // `None` ⇒ every elevation is a frame cell, so the inspector lists the
        // whole volume (all tilts), not just the selected one.
        tilt: None,
        active,
        prev_active: None,
    };

    // A generous window around the scan so its container is included.
    let containers = view.frame_containers_in_range(scan_start - 7200.0, scan_start + 7200.0, join);
    let container = containers
        .iter()
        .min_by(|a, b| {
            (a.key_secs - scan_start)
                .abs()
                .partial_cmp(&(b.key_secs - scan_start).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .filter(|c| (c.key_secs - scan_start).abs() <= MATCH_TOLERANCE_SECS)
        .cloned();

    let use_local = state.use_local_time;
    let dark = state.is_dark;
    let now_secs = state.frame_now.secs();
    let mut commands: Vec<AppCommand> = Vec::new();
    let mut close = false;
    let mut loop_from_here = false;

    egui::Window::new(format!("{} Scan inspector", icons::STACK))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(440.0)
        .max_height(ctx.input(|i| i.viewport_rect().height()) * 0.85)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_min_width(380.0);

            let Some(container) = container.as_ref() else {
                ui.label(
                    RichText::new("That scan is no longer in view.")
                        .size(12.0)
                        .italics()
                        .color(super::colors::ui::label(dark)),
                );
                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
                return;
            };

            render_header(ui, container, use_local, dark);
            ui.separator();
            render_sweep_table(ui, container, dark, scan_start, &mut commands);
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);

            // Actions row: fetch whole scan + loop from here.
            ui.horizontal(|ui| {
                let any_missing = container
                    .cells
                    .iter()
                    .any(|c| matches!(c.state, FrameCellState::Available | FrameCellState::Failed));
                if (any_missing || container.is_available)
                    && ui
                        .button(format!("{} Fetch whole scan", icons::DOWNLOAD_SIMPLE))
                        .on_hover_text("Download the full volume (all tilts)")
                        .clicked()
                {
                    commands.push(AppCommand::FetchScan {
                        scan_start: scan_start.round() as i64,
                        elevation_filter: None,
                    });
                }

                if ui
                    .button(format!("{} Loop from here", icons::REPEAT))
                    .on_hover_text("Create a loop from this scan up to now")
                    .clicked()
                {
                    loop_from_here = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        });

    // Apply deferred intents (after the closure releases its borrows).
    if loop_from_here {
        apply_loop_from_here(live, playback, state, scan_start, now_secs);
        close = true;
    }
    for cmd in commands {
        state.push_command(cmd);
    }
    if close {
        chrome.scan_inspector = None;
    }
}

/// Scan-level header: time span, VCP, and the cached/total tilt count (spec
/// §6.3 "carries the complete breakdown").
fn render_header(ui: &mut egui::Ui, container: &ScanContainer, use_local: bool, dark: bool) {
    let title = format_timestamp_full(container.key_secs, use_local);
    ui.label(RichText::new(title).strong().size(13.0));

    let total = container.cells.len();
    let cached = container
        .cells
        .iter()
        .filter(|c| c.state == FrameCellState::Cached)
        .count();
    let vcp = if container.vcp == 0 {
        "VCP —".to_string()
    } else {
        format!("VCP {}", container.vcp)
    };
    let live_tag = if container.is_live { " · live" } else { "" };
    ui.label(
        RichText::new(format!(
            "{vcp} · {cached}/{total} tilts on device{live_tag}"
        ))
        .size(11.0)
        .color(super::colors::ui::label(dark)),
    );
}

/// One row per sweep (all tilts, SAILS/MRLE revisits included), sorted by
/// collection time. Each row: tilt number + angle, cache-state pill, size, and
/// either chunk progress (in-flight live) or a tap-to-fetch button (available).
fn render_sweep_table(
    ui: &mut egui::Ui,
    container: &ScanContainer,
    dark: bool,
    scan_start: f64,
    commands: &mut Vec<AppCommand>,
) {
    let value_color = super::colors::ui::value(dark);
    let label_color = super::colors::ui::label(dark);

    let mut cells: Vec<&FrameCell> = container.cells.iter().collect();
    cells.sort_by(|a, b| {
        a.start_secs
            .partial_cmp(&b.start_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    ScrollArea::vertical()
        .auto_shrink([false, true])
        .max_height(320.0)
        .show(ui, |ui| {
            if cells.is_empty() {
                ui.label(
                    RichText::new("No sweep structure known for this scan yet.")
                        .size(11.0)
                        .italics()
                        .color(label_color),
                );
                return;
            }
            for cell in cells {
                ui.horizontal(|ui| {
                    // Tilt number + angle.
                    let tilt = if cell.elevation_number == 0 {
                        "tilt —".to_string()
                    } else {
                        format!("tilt {}", cell.elevation_number)
                    };
                    ui.label(
                        RichText::new(tilt)
                            .size(11.0)
                            .monospace()
                            .color(value_color),
                    );
                    if cell.elevation_angle > 0.0 {
                        ui.label(
                            RichText::new(format!("{:.1}\u{00b0}", cell.elevation_angle))
                                .size(11.0)
                                .color(label_color),
                        );
                    }
                    if cell.is_active {
                        ui.label(
                            RichText::new("\u{25c9}")
                                .size(11.0)
                                .color(super::colors::timeline::ACTIVE_SWEEP),
                        )
                        .on_hover_text("Currently displayed");
                    }

                    // State pill + (estimated) size.
                    let (state_label, state_color) = cell_state_display(cell.state);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match cell.state {
                            FrameCellState::Available | FrameCellState::Failed => {
                                // Tap-to-fetch this single tilt
                                // (elevation-filtered scan fetch).
                                let label =
                                    format!("{} Fetch", egui_phosphor::regular::DOWNLOAD_SIMPLE);
                                if ui.small_button(label).clicked() {
                                    commands.push(AppCommand::FetchScan {
                                        scan_start: scan_start.round() as i64,
                                        elevation_filter: Some(cell.elevation_number),
                                    });
                                }
                            }
                            FrameCellState::InFlight => {
                                if let Some(chunks) = &cell.chunks {
                                    if chunks.chunks_expected > 0 {
                                        ui.label(
                                            RichText::new(format!(
                                                "chunk {}/{}",
                                                chunks.chunks_received.min(chunks.chunks_expected),
                                                chunks.chunks_expected
                                            ))
                                            .size(10.0)
                                            .color(super::colors::acquisition::ACTIVE),
                                        );
                                    } else {
                                        ui.label(
                                            RichText::new("downloading")
                                                .size(10.0)
                                                .italics()
                                                .color(super::colors::acquisition::ACTIVE),
                                        );
                                    }
                                }
                            }
                            FrameCellState::Cached => {
                                ui.label(
                                    RichText::new(format!(
                                        "~{}",
                                        format_bytes(estimated_sweep_bytes())
                                    ))
                                    .size(10.0)
                                    .color(label_color),
                                );
                            }
                            _ => {}
                        }
                        ui.label(
                            RichText::new(state_label)
                                .size(10.0)
                                .strong()
                                .color(state_color),
                        );
                    });
                });
            }
        });
}

/// A grayscale-distinguishable label + accent for a cell state (the accent is
/// secondary; the word carries the meaning for accessibility).
fn cell_state_display(state: FrameCellState) -> (&'static str, Color32) {
    // Neutral gray for the non-accent states (in archive / expected); accent
    // colors only for the states that carry one (red = failure).
    let neutral = Color32::from_rgb(150, 150, 160);
    match state {
        FrameCellState::Cached => ("on device", super::colors::acquisition::COMPLETED),
        FrameCellState::Available => ("in archive", neutral),
        FrameCellState::InFlight => ("downloading", super::colors::acquisition::ACTIVE),
        FrameCellState::Queued => ("queued", super::colors::acquisition::QUEUED),
        FrameCellState::Projected => ("expected", neutral),
        FrameCellState::Failed => ("failed", super::colors::acquisition::FAILED),
    }
}

/// "Loop from here" (spec §12): a loop spanning this scan up to now. Reuses the
/// selection→bounds path (the same one shift/right-drag uses), detaching the
/// playhead first so the Free-mode seek invariant holds, and anchoring the
/// selection to the live edge if streaming so the loop follows new data.
fn apply_loop_from_here(
    live: &mut Live,
    playback: &mut Playback,
    state: &mut AppState,
    scan_start: f64,
    now_secs: f64,
) {
    live.detach_playhead(
        &mut playback.state,
        state.frame_now.secs(),
        state.pause_stream_while_reviewing,
    );
    playback.state.set_playback_position(scan_start);
    playback.state.set_selection(scan_start, now_secs);
    if live.mode_state.is_active() {
        playback.state.anchor_selection_to_live();
    }
    playback.state.apply_selection_as_bounds();
    // Arm the bulk fetch for the selected span (selection = the fetch), the
    // same one-shot the strip's selection gesture uses.
    if let Some(range) = playback.state.selection_range() {
        state.selection_just_finalized = Some(range);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Per-sweep estimate is a sensible fraction of the whole-volume estimate.
    #[wasm_bindgen_test]
    fn sweep_estimate_is_a_fraction_of_volume_estimate() {
        let per = estimated_sweep_bytes();
        assert!(per >= 1);
        assert!(per < crate::AVG_SCAN_BYTES);
        assert_eq!(per, crate::AVG_SCAN_BYTES / 10);
    }

    /// Every cell state maps to a non-empty, grayscale-legible label.
    #[wasm_bindgen_test]
    fn every_state_has_a_label() {
        for s in [
            FrameCellState::Cached,
            FrameCellState::Available,
            FrameCellState::InFlight,
            FrameCellState::Queued,
            FrameCellState::Projected,
            FrameCellState::Failed,
        ] {
            let (label, _) = cell_state_display(s);
            assert!(!label.is_empty());
        }
    }
}
