//! Confirmation modal for bulk-downloading a long timeline selection.
//!
//! Selecting a range arms a bulk fetch of every scan in it ("selection = the
//! fetch"). Short spans download silently; a span longer than
//! [`crate::SELECTION_BULK_CONFIRM_SECS`] routes here first so the user can
//! confirm a potentially large download. Cancel keeps the selection as loop
//! bounds but downloads nothing; Download Anyway arms the fetch pump.

use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::subsystem::acquisition::SelectionFetchTarget;
use crate::subsystem::{Acquisition, Chrome};
use eframe::egui::{self, Color32, RichText, Vec2};

pub(super) struct RangeDownloadModalLayer;

impl Layer for RangeDownloadModalLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        35
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.range_download_modal.is_some()
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw_range_download_modal(ctx.ctx, ctx.acquisition, ctx.chrome);
    }
}

fn draw_range_download_modal(
    ctx: &egui::Context,
    acquisition: &mut Acquisition,
    chrome: &mut Chrome,
) {
    // Backdrop click / Escape = Cancel (selection stays as bounds, no download).
    if super::modal_helper::modal_backdrop(ctx, "range_download_backdrop", 180) {
        chrome.range_download_modal = None;
        return;
    }

    let Some((start, end)) = chrome.range_download_modal else {
        return;
    };
    let dur = (end - start).abs();
    // Rough estimate: scans ≈ span / typical volume cadence; bytes ≈ scans ×
    // an approximate per-scan size (real sizes aren't in the listing).
    let count = (dur / crate::FALLBACK_SCAN_DURATION_SECS as f64)
        .ceil()
        .max(1.0) as u64;
    let est_bytes = count.saturating_mul(crate::AVG_SCAN_BYTES);
    let est_mb = est_bytes as f64 / (1024.0 * 1024.0);
    let hours = dur / 3600.0;

    egui::Window::new("Download selected range?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(380.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(8.0);

            ui.label(RichText::new("This selection spans a long time range:").strong());

            ui.add_space(8.0);

            ui.label(format!("  \u{2022} Duration: {hours:.1} hours"));
            ui.label(format!(
                "  \u{2022} Estimated download: ~{count} scans (~{est_mb:.0} MB)"
            ));

            ui.add_space(8.0);

            ui.label(
                RichText::new("Cancel keeps the selection for looping without downloading.")
                    .weak()
                    .italics(),
            );

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    chrome.range_download_modal = None;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let confirm = ui.add(
                        egui::Button::new(RichText::new("Download Anyway").color(Color32::WHITE))
                            .fill(Color32::from_rgb(60, 120, 200)),
                    );
                    if confirm.clicked() {
                        if let Some(range) = chrome.range_download_modal.take() {
                            acquisition.selection_fetch_target = Some(SelectionFetchTarget {
                                range,
                                armed_at_secs: crate::state::TimeModel::wall_clock_time(),
                            });
                        }
                    }
                });
            });

            ui.add_space(4.0);
        });
}
