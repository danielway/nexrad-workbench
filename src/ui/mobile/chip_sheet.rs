//! The phone's chip sheet: a tap on the selection chip opens it in place of
//! the desktop's menu. A thin projection of
//! [`crate::core::selection_chip::chip_sheet_vm`]: five rows, verbs first,
//! the current one highlighted, the loop lengths inline. A row or choice
//! emits what its menu entry does ([`crate::core::selection_chip::intent_for`])
//! and closes the sheet.

use super::super::layout::{Layer, LayerKind, LayoutCtx};
use super::action_sheet::{action_row, ActionRow, RowHit};
use crate::core::dialogs::{DialogId, DialogIntent};
use crate::core::phone_range::action_sheet_rect;
use crate::core::selection_chip::{
    chip_menu, chip_sheet_vm, intent_for, menu_view, ChipEnv, SheetRow,
};
use crate::core::Intent;
use crate::ui::dialog::Sheet;
use eframe::egui::{Pos2, Rect, Vec2};

pub(in crate::ui) struct ChipSheetLayer;

impl Layer for ChipSheetLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        96
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.chip_sheet_open && ctx.state.is_mobile
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        draw(ctx);
    }
}

/// The glyph beside each row.
fn icon(row: SheetRow) -> &'static str {
    use crate::ui::icons as ph;
    match row {
        SheetRow::Live => ph::BROADCAST,
        SheetRow::LoopLast => ph::REPEAT,
        SheetRow::LoopView => ph::RECTANGLE,
        SheetRow::GoToDay => ph::CALENDAR_BLANK,
        SheetRow::StopStream => ph::STOP,
    }
}

fn draw(ctx: &mut LayoutCtx) {
    let env = ChipEnv {
        stream_active: ctx.live.session.stream.is_running(),
        touch: true,
    };
    let now = ctx.state.frame_now.secs();
    let ps = &ctx.playback.state;
    let view = menu_view(
        true,
        (
            ps.timeline_view_start,
            ps.timeline_view_start + ps.view_width_secs(),
        ),
        ctx.chrome.phone_strip_view.map(|(w, _)| (w.start, w.end)),
    );
    let vm = chip_sheet_vm(&chip_menu(ps, env, view, now));

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
    let out = Sheet::new(DialogId::ChipSheet, vm.title, rect).show(egui_ctx, |ui, _| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let n = vm.rows.len();
        for (i, row) in vm.rows.iter().enumerate() {
            let hit = action_row(
                ui,
                &ActionRow {
                    icon: icon(row.row),
                    tone: row.tone,
                    label: row.label,
                    sub: row.sub.as_deref(),
                    choices: row
                        .choices
                        .iter()
                        .map(|c| (c.label.as_str(), c.on))
                        .collect(),
                    unit: row.choice_unit,
                    current: row.current,
                    enabled: row.action.is_some(),
                    last: i + 1 == n,
                },
            );
            picked = match hit {
                Some(RowHit::Choice(c)) => row.choices.get(c).map(|c| c.action),
                Some(RowHit::Row) => row.action,
                None => picked,
            };
        }
    });
    let state = &mut *ctx.state;
    if let Some(action) = picked {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::ChipSheet)));
        state.push_command(intent_for(
            action,
            env,
            crate::core::phone_dock::PHONE_STOP_PLACEMENT,
        ));
    } else if out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::ChipSheet)));
    }
}
