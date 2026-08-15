//! Last-successful-check stamp for NWS alert layers.

use crate::ui::time_format::{format_clock_12h, Compaction};
use eframe::egui::{self, Align2, Color32, FontId, Vec2};

pub(super) struct AlertsTimeOverlay;

impl super::Overlay for AlertsTimeOverlay {
    fn z_order(&self) -> i32 {
        60
    }

    fn visible(&self, ctx: &super::OverlayContext) -> bool {
        let layers = &ctx.state.layer_state.geo;
        ctx.state.viz_state.view_mode() == crate::geo::ViewMode::Flat2D
            && ctx.derived.data_is_live
            && (layers.alerts_warnings || layers.alerts_other)
            && ctx.diagnostics_vm.alerts_last_checked_secs.is_some()
    }

    fn draw(&self, ui: &mut egui::Ui, ctx: &super::OverlayContext) {
        let Some(checked_secs) = ctx.diagnostics_vm.alerts_last_checked_secs else {
            return;
        };
        let clock = format_clock_12h(
            checked_secs,
            ctx.state.use_local_time,
            Compaction::NoSeconds,
        );
        let label = format!("Alerts checked {clock}");
        // Keep one line above the national mosaic stamp, which owns the
        // bottom-most right-corner position when that layer is visible.
        let pos = ctx.rect.right_bottom() - Vec2::new(12.0, 28.0);
        let font = FontId::monospace(11.0);
        let color = Color32::from_rgba_unmultiplied(200, 200, 220, 220);
        let painter = ui.painter();

        painter.text(
            pos + Vec2::new(1.0, 1.0),
            Align2::RIGHT_BOTTOM,
            &label,
            font.clone(),
            Color32::from_rgba_unmultiplied(0, 0, 0, 140),
        );
        painter.text(pos, Align2::RIGHT_BOTTOM, label, font, color);
    }
}
