//! The destructive confirmation (§12): one pattern for clear cache, reset
//! settings and delete event. It lists exactly what will be deleted and what
//! stays, with an outlined destructive button. The words are the core's
//! ([`crate::core::dialogs::confirm_vm`]); confirming emits
//! [`DialogIntent::Confirm`].
//!
//! (The file keeps its old name so the layout registry is unchanged; the
//! layer renders whichever confirmation `Chrome::confirm` holds.)

use super::dialog::{self, Dialog, Width};
use super::layout::{Layer, LayerKind, LayoutCtx};
use crate::core::dialogs::{confirm_vm, Confirm, ConfirmFacts, DialogId, DialogIntent, ResetFacts};
use crate::core::Intent;
use crate::state::{format_bytes, AppState};
use crate::ui::theme;
use eframe::egui::{self, RichText};

pub(super) struct ResetSettingsModalLayer;

impl Layer for ResetSettingsModalLayer {
    fn kind(&self) -> LayerKind {
        LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        30
    }
    fn visible(&self, ctx: &LayoutCtx) -> bool {
        ctx.chrome.confirm.is_some()
    }
    fn render(&self, ctx: &mut LayoutCtx) {
        if let Some(confirm) = ctx.chrome.confirm {
            let reset = ResetFacts {
                has_mping_key: ctx.diagnostics.mping.api_key.is_some(),
                has_preferred_site: ctx.state.preferred_site.is_some(),
                forget_key_and_site: ctx.chrome.reset_forgets_key_and_site,
            };
            draw_confirm(ctx.ctx, ctx.state, reset, confirm);
        }
    }
}

fn facts(
    state: &AppState,
    reset: ResetFacts,
    confirm: crate::core::dialogs::Confirm,
) -> ConfirmFacts {
    let event = match confirm {
        crate::core::dialogs::Confirm::DeleteEvent(id) => state
            .saved_events
            .events
            .iter()
            .find(|e| e.id == id)
            .map(|e| (e.name.clone(), e.site_id.clone(), e.start_time, e.end_time)),
        _ => None,
    };
    ConfirmFacts {
        cache_bytes: state.session_stats.cache_size_bytes,
        saved_events: state.saved_events.events.len(),
        event,
        reset,
    }
}

fn draw_confirm(ctx: &egui::Context, state: &mut AppState, reset: ResetFacts, confirm: Confirm) {
    let clock = crate::ui::time_format::ZoneClock {
        use_local: state.use_local_time,
    };
    let vm = confirm_vm(
        confirm,
        &facts(state, reset, confirm),
        format_bytes,
        &clock,
        state.frame_now.secs(),
    );
    let mut cancel = false;
    let mut confirmed = false;
    let out = Dialog::new(DialogId::Confirm, &vm.title)
        .width(Width::Narrow)
        .show(ctx, |ui| {
            dialog::group_heading(ui, "Deletes");
            for line in &vm.deletes {
                dialog::bullet(ui, line, theme::TEXT);
            }
            ui.add_space(8.0);
            dialog::group_heading(ui, "Keeps");
            for line in &vm.keeps {
                dialog::bullet(ui, line, theme::TEXT_DIM);
            }
            // The mPING key and the preferred site stay unless asked.
            if confirm == Confirm::ResetSettings
                && (reset.has_mping_key || reset.has_preferred_site)
            {
                ui.add_space(8.0);
                let mut forget = reset.forget_key_and_site;
                if ui
                    .checkbox(&mut forget, "Also forget the mPING key and preferred site")
                    .changed()
                {
                    state.push_command(Intent::SetResetForgetsKeyAndSite(forget));
                }
            }
            if let Some(note) = &vm.note {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(note)
                        .size(theme::CAPTION)
                        .color(theme::TEXT_DIM),
                );
            }
            dialog::footer(ui, |ui| {
                confirmed = dialog::destructive(ui, &vm.action).clicked();
                cancel = dialog::secondary(ui, "Cancel").clicked();
            });
        });
    if confirmed {
        state.push_command(Intent::Dialog(DialogIntent::Confirm));
    } else if cancel || out.close {
        state.push_command(Intent::Dialog(DialogIntent::Close(DialogId::Confirm)));
    }
}
