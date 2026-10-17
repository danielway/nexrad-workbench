//! The phone strip's long-press sheet (phone timeline §4): a thin projection
//! of [`crate::core::phone_range::press_sheet_vm`].
//!
//! A small bottom sheet (on the right in landscape) on the dialog component
//! and the Esc stack, headed by the pressed time, tilt and product, with
//! Inspect this scan, Start range here, End range here and Save event here.
//! A disabled row says why beside its label, since a finger cannot hover.
//! A tapped row emits [`Intent::PressSheetRow`]; what it does is the core's
//! ([`crate::core::phone_range::row_intent`]).

use super::super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::dialogs::{DialogId, DialogIntent};
use crate::core::phone_range::{
    press_heading, press_sheet_rect, press_sheet_vm, PressRow, RangeFacts,
};
use crate::core::Intent;
use crate::ui::dialog::Sheet;
use crate::ui::theme;
use eframe::egui::{self, Align2, FontId, Pos2, Rect, RichText, Vec2};

/// A row's height: a comfortable thumb target.
const ROW_H: f32 = 44.0;

pub(in crate::ui) struct PressSheetLayer;

impl Layer for PressSheetLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        96
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.press_sheet.is_some() && ctx.state.is_mobile
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw(ctx);
    }
}

/// The glyph beside each row.
fn icon(row: PressRow) -> &'static str {
    match row {
        PressRow::Inspect => egui_phosphor::regular::MAGNIFYING_GLASS,
        PressRow::StartRange => egui_phosphor::regular::ARROW_LINE_RIGHT,
        PressRow::EndRange => egui_phosphor::regular::ARROW_LINE_LEFT,
        PressRow::SaveEvent => egui_phosphor::regular::BOOKMARK_SIMPLE,
    }
}

fn draw(ctx: &mut LayoutCtx) {
    let Some(press) = ctx.chrome.press_sheet else {
        return;
    };
    let state = &mut *ctx.state;
    let facts = RangeFacts::of(&ctx.playback.state, state.frame_now.secs());
    let offset = crate::ui::timeline::tz_offset_secs(press.ts, state.use_local_time);
    let heading = press_heading(
        press.ts,
        offset,
        &state.viz_state.elevation_selection,
        state.viz_state.product,
    );
    let vm = press_sheet_vm(&press, &facts, heading);

    let egui_ctx = ctx.ctx;
    let viewport = egui_ctx.input(|i| i.viewport_rect());
    let r = press_sheet_rect(
        viewport.width(),
        viewport.height(),
        ctx.chrome.device_layout.is_landscape_phone(),
        super::safe_area_insets(),
    );
    let rect = Rect::from_min_size(Pos2::new(r.x, r.y), Vec2::new(r.w, r.h));
    let mut picked = None;
    let out = Sheet::new(DialogId::PressSheet, &vm.heading, rect).show(egui_ctx, |ui, _| {
        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
            for row in &vm.rows {
                let enabled = row.disabled.is_none();
                let color = if enabled {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                };
                let resp = ui.add_enabled(
                    enabled,
                    egui::Button::new(
                        RichText::new(format!("{}  {}", icon(row.row), row.label))
                            .size(theme::BODY)
                            .color(color),
                    )
                    .frame(false)
                    .min_size(Vec2::new(ui.available_width(), ROW_H)),
                );
                if let Some(reason) = row.disabled {
                    ui.painter().text(
                        Pos2::new(resp.rect.right() - 4.0, resp.rect.center().y),
                        Align2::RIGHT_CENTER,
                        reason,
                        FontId::proportional(theme::CAPTION),
                        theme::TEXT_DIM,
                    );
                }
                if resp.clicked() {
                    picked = Some(row.row);
                }
            }
        });
    });
    if let Some(row) = picked {
        state.push_command(Intent::PressSheetRow(row));
    } else if out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::PressSheet)));
    }
}
