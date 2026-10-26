//! Scan inspector (spec §5, §6.3, §12): the per-scan volume breakdown, as a
//! popover anchored where the scan was right-clicked / long-pressed.
//!
//! Opened by right-click (desktop) or long-press (touch) on a scan container.
//! Lists *every* sweep of the inspected volume — all tilts, including SAILS /
//! MRLE revisits — with its elevation angle, cache state, an (estimated) size,
//! per-chunk progress for the in-progress live volume, and a tap-to-fetch
//! affordance for cuts not yet downloaded. Plus a "Loop from here" action.
//!
//! The breakdown is read from the merged [`crate::core::TimelineView`] — the SAME source the
//! strip and tooltips read — so the inspector can never disagree with the strip
//! about what is cached / collecting / available. It rebuilds the view from the
//! subsystems each frame (cheap; the view borrows the cache) and finds the
//! container whose `key_secs` matches the stored scan-start.

use super::dialog::{self, Popover};
use super::layout::{Layer, LayerKind, LayoutCtx};
use super::timeline::format_timestamp_full;
use crate::core::dialogs::{DialogId, DialogIntent};
use crate::core::Intent;
use crate::core::{FrameCell, FrameCellState, FrameJoinInputs, ScanContainer};
use crate::state::{format_bytes, AppState};
use crate::subsystem::{Acquisition, Chrome, Live, Playback};
use crate::ui::icons;
use crate::ui::theme;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, ScrollArea, Vec2};

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
            ctx.sites.focus(),
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

/// Where the inspector is anchored: the pointer at the right-click / long
/// press that opened it, remembered per inspected scan (egui memory — it is
/// a screen position, the shell's fact).
fn anchor_for(ctx: &egui::Context, scan_start: f64) -> Rect {
    let id = egui::Id::unique("scan_inspector_anchor");
    let stored: Option<(f64, Pos2)> = ctx.data(|d| d.get_temp(id));
    let pos = match stored {
        Some((s, pos)) if s == scan_start => pos,
        _ => {
            let pos = ctx
                .input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()))
                .unwrap_or_else(|| ctx.input(|i| i.viewport_rect()).center());
            ctx.data_mut(|d| d.insert_temp(id, (scan_start, pos)));
            pos
        }
    };
    Rect::from_center_size(pos, Vec2::ZERO)
}

#[allow(clippy::too_many_arguments)]
fn draw_scan_inspector(
    ctx: &egui::Context,
    state: &mut AppState,
    site: &crate::subsystem::SiteState,
    live: &mut Live,
    _playback: &mut Playback,
    acquisition: &Acquisition,
    chrome: &mut Chrome,
) {
    let Some(scan_start) = chrome.scan_inspector else {
        return;
    };

    // Rebuild the merged view and find the inspected container. We use
    // `TiltFilter::Every` so EVERY elevation becomes a frame cell (the full volume
    // breakdown), unlike the strip which filters to the selected tilt; the
    // per-cell state resolution (cached/in-flight/available/...) is identical.
    let view = site.timeline_view(live.mode_state.is_active());

    let product = state.viz_state.product.to_worker_string();
    let mut in_flight_all = state.download_progress.in_flight_scans.clone();
    in_flight_all.extend_from_slice(&state.download_progress.active_scans);
    // Failed scan-starts come from the acquisition operations — the same source
    // the strip's failed-cell ticks use (so a retry clears it here too).
    let failed_secs = acquisition.state.failed_scan_starts();
    let active = site.displayed.as_ref().map(|d| {
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
        tilt: crate::core::tilt::TiltFilter::Every,
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
    let (title, subtitle) = match container.as_ref() {
        Some(c) => (format_timestamp_full(c.key_secs, use_local), header_line(c)),
        None => ("Scan".to_string(), "No longer in view".to_string()),
    };

    let out = Popover::new(DialogId::ScanInspector, anchor_for(ctx, scan_start))
        .title(&title)
        .subtitle(subtitle)
        .show(ctx, |ui, room| {
            let Some(container) = container.as_ref() else {
                ui.label(
                    RichText::new("That scan is no longer on the timeline.").color(theme::TEXT_DIM),
                );
                return;
            };
            render_sweep_table(ui, state, container, scan_start, (room - 70.0).max(80.0));

            // Actions: fetch the whole volume, loop from here to now.
            dialog::footer(ui, |ui| {
                if dialog::primary(
                    ui,
                    &format!("{} Loop from here \u{2192} now", icons::REPEAT),
                )
                .on_hover_text("A loop from this scan up to now")
                .clicked()
                {
                    state.push_command(Intent::LoopFromScan(scan_start));
                }
                let missing = container
                    .cells
                    .iter()
                    .filter(|c| {
                        matches!(c.state, FrameCellState::Available | FrameCellState::Failed)
                    })
                    .count();
                if (missing > 0 || container.is_available)
                    && dialog::secondary(
                        ui,
                        &format!(
                            "{} Fetch volume (~{})",
                            icons::DOWNLOAD_SIMPLE,
                            format_bytes(estimated_sweep_bytes() * missing.max(1) as u64)
                        ),
                    )
                    .on_hover_text("Download every cut of this volume not on this device")
                    .clicked()
                {
                    state.push_command(Intent::FetchScan {
                        scan_start: scan_start.round() as i64,
                        elevation_filter: None,
                    });
                }
            });
        });
    if out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::ScanInspector)));
    }
}

/// The quiet line under the inspector's title: VCP and the cut count on
/// device (the ladder's words).
fn header_line(container: &ScanContainer) -> String {
    let total = container.cells.len();
    let cached = container
        .cells
        .iter()
        .filter(|c| c.state == FrameCellState::Cached)
        .count();
    let vcp = if container.vcp == 0 {
        "VCP \u{2014}".to_string()
    } else {
        format!("VCP {}", container.vcp)
    };
    let live_tag = if container.is_live { " · live" } else { "" };
    format!("{vcp} · {cached}/{total} cuts on device{live_tag}")
}

/// One row per cut (SAILS / MRLE revisits included), in collection order:
/// the angle as the ladder writes it, the state as a mark and a word, and a
/// fetch, chunk count or size. The cut on screen has the white outline.
fn render_sweep_table(
    ui: &mut egui::Ui,
    state: &mut AppState,
    container: &ScanContainer,
    scan_start: f64,
    max_height: f32,
) {
    let mut cells: Vec<&FrameCell> = container.cells.iter().collect();
    cells.sort_by(|a, b| {
        a.start_secs
            .partial_cmp(&b.start_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    ScrollArea::vertical()
        .auto_shrink([false, true])
        .max_height(max_height)
        .show(ui, |ui| {
            if cells.is_empty() {
                ui.label(
                    RichText::new("No cut structure known for this scan yet.")
                        .color(theme::TEXT_DIM),
                );
                return;
            }
            for cell in cells {
                let row = ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    let angle = if cell.elevation_angle > 0.0 {
                        format!("{:.1}\u{00b0}", cell.elevation_angle)
                    } else {
                        "\u{2014}".to_string()
                    };
                    ui.label(
                        RichText::new(angle)
                            .monospace()
                            .size(theme::READOUT)
                            .color(theme::TEXT),
                    )
                    .on_hover_text(format!("Cut {}", cell.elevation_number));

                    let (mark, word, color) = cell_state_display(cell.state);
                    ui.label(RichText::new(mark).color(color));
                    ui.label(
                        RichText::new(word)
                            .size(theme::CAPTION)
                            .color(theme::TEXT_DIM),
                    );

                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| match cell.state {
                            FrameCellState::Available | FrameCellState::Failed => {
                                if ui
                                    .small_button(format!("{} Fetch", icons::DOWNLOAD_SIMPLE))
                                    .on_hover_text("Download this cut")
                                    .clicked()
                                {
                                    state.push_command(Intent::FetchScan {
                                        scan_start: scan_start.round() as i64,
                                        elevation_filter: Some(cell.elevation_number),
                                    });
                                }
                            }
                            FrameCellState::InFlight => {
                                if let Some(chunks) =
                                    cell.chunks.as_ref().filter(|c| c.chunks_expected > 0)
                                {
                                    ui.label(
                                        RichText::new(format!(
                                            "chunk {}/{}",
                                            chunks.chunks_received.min(chunks.chunks_expected),
                                            chunks.chunks_expected
                                        ))
                                        .monospace()
                                        .size(theme::CAPTION)
                                        .color(theme::TEXT_DIM),
                                    );
                                }
                            }
                            FrameCellState::Cached => {
                                ui.label(
                                    RichText::new(format!(
                                        "~{}",
                                        format_bytes(estimated_sweep_bytes())
                                    ))
                                    .monospace()
                                    .size(theme::CAPTION)
                                    .color(theme::TEXT_FAINT),
                                );
                            }
                            _ => {}
                        },
                    );
                });
                if cell.is_active {
                    // The white outline: this cut is the one on screen.
                    ui.painter().rect_stroke(
                        row.response.rect.expand(1.0),
                        egui::CornerRadius::same(3),
                        egui::Stroke::new(1.0, theme::SEEN),
                        egui::StrokeKind::Inside,
                    );
                    row.response.on_hover_text("On screen");
                }
            }
        });
}

/// A cell state's mark, word and mark color — the strip's grammar: slate
/// fill for on device, hollow for in the archive, a tilde for expected, the
/// ⚠ glyph in text color for a failure (red means now).
fn cell_state_display(state: FrameCellState) -> (&'static str, &'static str, Color32) {
    match state {
        FrameCellState::Cached => (icons::RECORD, "on device", theme::ON_DEVICE),
        FrameCellState::Available => (icons::CIRCLE, "in archive", theme::TEXT_DIM),
        FrameCellState::InFlight => (icons::CIRCLE_HALF, "downloading", theme::ON_DEVICE),
        FrameCellState::Queued => (icons::CIRCLE_DASHED, "queued", theme::TEXT_DIM),
        FrameCellState::Projected => ("~", "expected", theme::TEXT_FAINT),
        FrameCellState::Failed => (icons::WARNING, "failed", theme::TEXT),
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
            let (mark, word, _) = cell_state_display(s);
            assert!(!mark.is_empty() && !word.is_empty());
        }
    }
}
