//! The phone strip's long-press sheet (phone timeline §4): a thin projection
//! of [`crate::core::phone_range::press_sheet_vm`].
//!
//! A small bottom sheet (on the right in landscape) on the dialog component
//! and the Esc stack, headed by the pressed time, tilt and product, with
//! Loop from here, Loop to here and Inspect this scan. A disabled row says
//! why at its right, since a finger cannot hover. A tapped row emits
//! [`Intent::PressSheetRow`]; what it does is the core's
//! ([`crate::core::phone_range::row_intent`]).

use super::super::layout::{Layer, LayerKind, LayoutCtx};
use super::action_sheet::{action_row, ActionRow};
use crate::core::dialogs::{DialogId, DialogIntent};
use crate::core::phone_range::{
    action_sheet_rect, press_heading, press_sheet_vm, PressRow, RangeFacts,
};
use crate::core::Intent;
use crate::ui::dialog::Sheet;
use eframe::egui::{Pos2, Rect, Vec2};

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
        PressRow::LoopFromHere => crate::ui::icons::ARROW_LINE_RIGHT,
        PressRow::LoopToHere => crate::ui::icons::ARROW_LINE_LEFT,
        PressRow::Inspect => crate::ui::icons::MAGNIFYING_GLASS,
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
    let r = action_sheet_rect(
        viewport.width(),
        viewport.height(),
        ctx.chrome.device_layout.is_landscape_phone(),
        super::safe_area_insets(),
        vm.rows.len(),
    );
    let rect = Rect::from_min_size(Pos2::new(r.x, r.y), Vec2::new(r.w, r.h));
    let mut picked = None;
    let out = Sheet::new(DialogId::PressSheet, &vm.heading, rect).show(egui_ctx, |ui, _| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let n = vm.rows.len();
        for (i, row) in vm.rows.iter().enumerate() {
            let hit = action_row(
                ui,
                &ActionRow {
                    icon: icon(row.row),
                    tone: row.tone,
                    label: row.label,
                    sub: row.sub,
                    choices: Vec::new(),
                    unit: None,
                    current: false,
                    enabled: row.enabled,
                    last: i + 1 == n,
                },
            );
            if hit.is_some() {
                picked = Some(row.row);
            }
        }
    });
    if let Some(row) = picked {
        state.push_command(Intent::PressSheetRow(row));
    } else if out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::PressSheet)));
    }
}
