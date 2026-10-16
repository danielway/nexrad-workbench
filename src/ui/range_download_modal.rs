//! Confirmation modal for bulk-downloading a long timeline selection.
//!
//! Selecting a range arms a bulk fetch of every scan in it ("selection = the
//! fetch"). Short spans download silently; a span longer than
//! [`crate::SELECTION_BULK_CONFIRM_SECS`] routes here first so the user can
//! confirm a potentially large download. Not now keeps the selection as loop
//! bounds but downloads nothing; Download arms the fetch pump.

use super::dialog::{self, Dialog, Width};
use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::dialogs::DialogId;
use crate::subsystem::acquisition::SelectionFetchTarget;
use crate::subsystem::{Acquisition, Chrome};
use crate::ui::theme;
use eframe::egui::{self, RichText};

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
        draw_range_download_modal(ctx.ctx, ctx.state, ctx.acquisition, ctx.chrome);
    }
}

fn draw_range_download_modal(
    ctx: &egui::Context,
    state: &mut crate::state::AppState,
    acquisition: &mut Acquisition,
    chrome: &mut Chrome,
) {
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
    let hours = dur / 3600.0;

    let mut download = false;
    let mut cancel = false;
    let out = Dialog::new(DialogId::RangeDownload, "Download this range?")
        .subtitle(format!("{hours:.1} hours selected"))
        .width(Width::Narrow)
        .show(ctx, |ui| {
            dialog::bullet(
                ui,
                &format!(
                    "About {count} scans, roughly {}",
                    crate::state::format_bytes(est_bytes)
                ),
                theme::TEXT,
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new("Not now keeps the range as a loop without downloading.")
                    .size(theme::CAPTION)
                    .color(theme::TEXT_DIM),
            );
            dialog::footer(ui, |ui| {
                download = dialog::primary(ui, "Download").clicked();
                cancel = dialog::secondary(ui, "Not now").clicked();
            });
        });

    if download {
        if let Some(range) = chrome.range_download_modal.take() {
            acquisition.selection_fetch_target = Some(SelectionFetchTarget {
                range,
                armed_at_secs: crate::core::TimeModel::wall_clock_time(),
            });
        }
    } else if cancel || out.close {
        // Not now / Esc / click outside: the selection stays as loop bounds.
        state.push_command(crate::core::Intent::Dialog(
            crate::core::dialogs::DialogIntent::Close(DialogId::RangeDownload),
        ));
    }
}
