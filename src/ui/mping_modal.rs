//! mPING settings modal — API key entry and integration status.
//!
//! Opened from the gear button next to the "Storm Reports (mPING)" checkbox
//! in the right panel. The user pastes their key from
//! https://mping.ou.edu/registration/ and saves it; it's persisted via
//! `UserPreferences` to localStorage. The modal also surfaces the most
//! recent fetch error verbatim so a CORS or auth problem is immediately
//! visible rather than silently leaving the layer empty.

use super::dialog::{self, Dialog};
use crate::core::dialogs::DialogId;
use crate::ui::theme;
use eframe::egui::{self, RichText};

/// Persistent UI state for the mPING settings modal — kept outside
/// `AppState` because it holds a transient text-edit buffer that should
/// not survive a reload.
#[derive(Default)]
pub(crate) struct MpingModalState {
    /// In-flight text entry. Populated from `diagnostics.mping.api_key` when the
    /// modal opens; written back to state on Save.
    pub key_input: String,
    /// Tracks whether `key_input` has been seeded from state for the
    /// current modal opening so we don't clobber user typing each frame.
    seeded: bool,
}

pub(super) struct MpingModalLayer;

impl super::layout::Layer for MpingModalLayer {
    fn kind(&self) -> super::layout::LayerKind {
        super::layout::LayerKind::Modal
    }
    fn z_order(&self) -> i32 {
        90
    }
    fn visible(&self, ctx: &super::layout::LayoutCtx) -> bool {
        // Also visible while modal_state needs the close-time reset that
        // the body's early-return performs.
        ctx.diagnostics.mping.settings_modal_open || ctx.modals.mping.seeded
    }
    fn render(&self, ctx: &mut super::layout::LayoutCtx) {
        let playback_secs = ctx.playback.state.playback_position();
        draw_mping_modal(
            ctx.ctx,
            ctx.state,
            ctx.diagnostics,
            playback_secs,
            &mut ctx.modals.mping,
        );
    }
}

/// Render the mPING settings modal. Key save/clear/close are emitted as
/// [`DiagnosticsIntent`]s (applied by the pure reducer); only the transient
/// text-edit buffer (`modal_state`) is mutated locally.
fn draw_mping_modal(
    ctx: &egui::Context,
    state: &mut crate::state::AppState,
    diagnostics: &crate::subsystem::Diagnostics,
    playback_secs: f64,
    modal_state: &mut MpingModalState,
) {
    use crate::core::diagnostics::DiagnosticsIntent;
    use crate::core::Intent;

    if !diagnostics.mping.settings_modal_open {
        modal_state.seeded = false;
        return;
    }

    if !modal_state.seeded {
        modal_state.key_input = diagnostics.mping.api_key.clone().unwrap_or_default();
        modal_state.seeded = true;
    }

    let close_intent = Intent::Dialog(crate::core::dialogs::DialogIntent::Close(
        crate::core::dialogs::DialogId::Mping,
    ));
    let out = Dialog::new(DialogId::Mping, "mPING storm reports")
        .subtitle("Crowd-sourced reports from NSSL / OU")
        .show(ctx, |ui| {
            ui.label(
                RichText::new(
                    "Reports submitted by volunteer observers show as markers \
                     near the active radar.",
                )
                .color(theme::TEXT_DIM),
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Get a key:").color(theme::TEXT_DIM));
                ui.hyperlink_to(
                    "mping.ou.edu/registration",
                    "https://mping.ou.edu/registration/",
                );
            });

            ui.add_space(10.0);
            dialog::group_heading(ui, "API key");
            ui.add(
                // two-way binding: the egui widget owns this value while the user edits it.
                egui::TextEdit::singleline(&mut modal_state.key_input)
                    .password(true)
                    .desired_width(f32::INFINITY)
                    .hint_text("Paste your mPING API token"),
            );
            ui.add_space(8.0);

            // Status line — the most recent fetch outcome. Failure is the ⚠
            // glyph in text color (red means now).
            let status = if let Some(err) = diagnostics.mping.last_error.as_deref() {
                RichText::new(format!("{} {}", crate::ui::icons::WARNING, err)).color(theme::TEXT)
            } else if diagnostics.mping.fetch_in_flight {
                RichText::new("Fetching reports\u{2026}").color(theme::TEXT_DIM)
            } else if diagnostics.mping.last_success_ms > 0.0 {
                // Count only reports visible at the current playhead — the
                // marker layer hides any observed after the rendered time.
                let n = diagnostics
                    .mping
                    .reports
                    .iter()
                    .filter(|r| r.visible_at(playback_secs))
                    .count();
                let total = diagnostics.mping.total_count;
                let extra = if total > n {
                    format!(" (showing {} of {})", n, total)
                } else {
                    String::new()
                };
                RichText::new(format!("{} report(s) loaded{}", n, extra)).color(theme::TEXT_DIM)
            } else if diagnostics.mping.api_key.is_some() {
                RichText::new("Key saved; reports load while the layer is on.")
                    .color(theme::TEXT_DIM)
            } else {
                RichText::new("Save a key to enable the layer.").color(theme::TEXT_DIM)
            };
            ui.label(status.size(theme::CAPTION));

            dialog::footer(ui, |ui| {
                if dialog::primary(ui, "Save").clicked() {
                    let trimmed = modal_state.key_input.trim();
                    let new_key = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    };
                    // The reducer guards on key change, clears the error,
                    // requests invalidation, and closes the modal.
                    state.push_command(Intent::Diagnostics(DiagnosticsIntent::SaveMpingApiKey(
                        new_key,
                    )));
                    modal_state.seeded = false;
                }
                if dialog::secondary(ui, "Cancel").clicked() {
                    state.push_command(close_intent.clone());
                    modal_state.seeded = false;
                }
                if diagnostics.mping.api_key.is_some()
                    && dialog::secondary(ui, "Clear key").clicked()
                {
                    state.push_command(Intent::Diagnostics(DiagnosticsIntent::ClearMpingKey));
                    modal_state.key_input.clear();
                }
            });
        });
    if out.close {
        state.push_command(close_intent);
        modal_state.seeded = false;
    }
}
