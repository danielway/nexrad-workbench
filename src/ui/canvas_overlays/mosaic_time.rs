//! National mosaic product-valid-time stamp in the bottom-right canvas corner.
//!
//! Shown only while the mosaic layer is on, data is live, and a texture has
//! been successfully loaded. The time is the MRMS product valid time from the
//! WMS `time` dimension (not client fetch time). Formats through the shared
//! clock helpers so a local/UTC flip reformats this stamp in the same frame as
//! every other readout.

use crate::ui::time_format::format_mosaic_stamp;
use eframe::egui::{self, Align2, Color32, FontId, Vec2};

/// Trait wrapper for the registry. Z-order sits above the scale bar so the
/// stamp stays readable over map chrome.
pub(super) struct MosaicTimeOverlay;

impl super::Overlay for MosaicTimeOverlay {
    fn z_order(&self) -> i32 {
        50
    }

    fn visible(&self, ctx: &super::OverlayContext) -> bool {
        ctx.state.layer_state.geo.national_mosaic
            && ctx.derived.data_is_live
            && ctx.state.national_mosaic.image_time().is_some()
    }

    fn draw(&self, ui: &mut egui::Ui, ctx: &super::OverlayContext) {
        let Some(label) = format_mosaic_stamp(
            ctx.state.national_mosaic.image_time(),
            ctx.state.use_local_time,
        ) else {
            return;
        };

        let painter = ui.painter();
        let pos = ctx.rect.right_bottom() - Vec2::new(12.0, 12.0);
        let font = FontId::monospace(11.0);
        let color = Color32::from_rgba_unmultiplied(200, 200, 220, 220);

        // Soft shadow so the stamp stays legible over bright mosaic cells.
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
