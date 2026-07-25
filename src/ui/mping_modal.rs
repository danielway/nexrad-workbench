//! mPING settings modal — API key entry and integration status.
//!
//! Opened from the gear button next to the "Storm Reports (mPING)" checkbox
//! in the right panel. The user pastes their key from
//! https://mping.ou.edu/registration/ and saves it; it's persisted via
//! `UserPreferences` to localStorage. The modal also surfaces the most
//! recent fetch error verbatim so a CORS or auth problem is immediately
//! visible rather than silently leaving the layer empty.

use eframe::egui::{self, Color32, RichText, Vec2};

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

    if super::modal_helper::modal_backdrop(ctx, "mping_modal_backdrop", 180) {
        state.push_command(Intent::Diagnostics(DiagnosticsIntent::CloseMpingSettings));
        modal_state.seeded = false;
        return;
    }

    if !modal_state.seeded {
        modal_state.key_input = diagnostics.mping.api_key.clone().unwrap_or_default();
        modal_state.seeded = true;
    }

    egui::Window::new("mPING Storm Reports")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(420.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(6.0);

            ui.label(
                RichText::new(
                    "mPING is a crowd-sourced weather report service from NSSL/OU. \
                     Reports submitted by volunteer observers are shown as colored \
                     markers near the active radar.",
                )
                .small(),
            );

            ui.add_space(8.0);

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Get a key:").small());
                ui.hyperlink_to(
                    RichText::new("mping.ou.edu/registration").small(),
                    "https://mping.ou.edu/registration/",
                );
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);

            ui.label(RichText::new("API key").strong());
            ui.add_space(2.0);
            ui.add(
                egui::TextEdit::singleline(&mut modal_state.key_input)
                    .password(true)
                    .desired_width(f32::INFINITY)
                    .hint_text("Paste your mPING API token"),
            );

            ui.add_space(8.0);

            // Status block — most recent fetch outcome.
            if let Some(err) = diagnostics.mping.last_error.as_deref() {
                ui.label(
                    RichText::new(format!("\u{26A0} {}", err))
                        .small()
                        .color(Color32::from_rgb(220, 120, 120)),
                );
            } else if diagnostics.mping.fetch_in_flight {
                ui.label(RichText::new("Fetching reports\u{2026}").small().weak());
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
                ui.label(
                    RichText::new(format!("\u{2713} {} report(s) loaded{}", n, extra))
                        .small()
                        .color(Color32::from_rgb(120, 200, 120)),
                );
            } else if diagnostics.mping.api_key.is_some() {
                ui.label(
                    RichText::new("Layer is enabled but no fetch has run yet.")
                        .small()
                        .weak(),
                );
            } else {
                ui.label(
                    RichText::new("Save a key to enable the layer.")
                        .small()
                        .weak(),
                );
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    state.push_command(Intent::Diagnostics(DiagnosticsIntent::CloseMpingSettings));
                    modal_state.seeded = false;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let trimmed = modal_state.key_input.trim();
                    let save_btn = ui.add(
                        egui::Button::new(RichText::new("Save").color(Color32::WHITE))
                            .fill(Color32::from_rgb(70, 130, 200)),
                    );
                    if save_btn.clicked() {
                        let new_key = if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        };
                        // The reducer guards on key change, clears the error,
                        // requests invalidation, and closes the modal.
                        state.push_command(Intent::Diagnostics(
                            DiagnosticsIntent::SaveMpingApiKey(new_key),
                        ));
                        modal_state.seeded = false;
                    }

                    if diagnostics.mping.api_key.is_some()
                        && ui
                            .button(RichText::new("Clear").color(Color32::from_rgb(220, 120, 120)))
                            .clicked()
                    {
                        state.push_command(Intent::Diagnostics(DiagnosticsIntent::ClearMpingKey));
                        modal_state.key_input.clear();
                    }
                });
            });

            ui.add_space(4.0);
        });
}
