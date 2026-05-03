//! mPING settings modal — API key entry and integration status.
//!
//! Opened from the gear button next to the "Storm Reports (mPING)" checkbox
//! in the right panel. The user pastes their key from
//! https://mping.ou.edu/registration/ and saves it; it's persisted via
//! `UserPreferences` to localStorage. The modal also surfaces the most
//! recent fetch error verbatim so a CORS or auth problem is immediately
//! visible rather than silently leaving the layer empty.

use crate::state::AppState;
use eframe::egui::{self, Color32, RichText, Vec2};

/// Persistent UI state for the mPING settings modal — kept outside
/// `AppState` because it holds a transient text-edit buffer that should
/// not survive a reload.
#[derive(Default)]
pub struct MpingModalState {
    /// In-flight text entry. Populated from `state.mping.api_key` when the
    /// modal opens; written back to state on Save.
    pub key_input: String,
    /// Tracks whether `key_input` has been seeded from state for the
    /// current modal opening so we don't clobber user typing each frame.
    seeded: bool,
}

/// Render the mPING settings modal if open. Returns `true` if the user
/// just saved a new (or cleared) API key, indicating the manager should
/// invalidate its cache and refetch.
pub fn render_mping_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    modal_state: &mut MpingModalState,
) -> bool {
    if !state.mping.settings_modal_open {
        modal_state.seeded = false;
        return false;
    }

    if super::modal_helper::modal_backdrop(ctx, "mping_modal_backdrop", 180) {
        state.mping.settings_modal_open = false;
        modal_state.seeded = false;
        return false;
    }

    if !modal_state.seeded {
        modal_state.key_input = state.mping.api_key.clone().unwrap_or_default();
        modal_state.seeded = true;
    }

    let mut saved = false;

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
            if let Some(err) = state.mping.last_error.as_deref() {
                ui.label(
                    RichText::new(format!("\u{26A0} {}", err))
                        .small()
                        .color(Color32::from_rgb(220, 120, 120)),
                );
            } else if state.mping.fetch_in_flight {
                ui.label(RichText::new("Fetching reports\u{2026}").small().weak());
            } else if state.mping.last_success_ms > 0.0 {
                let n = state.mping.reports.len();
                let total = state.mping.total_count;
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
            } else if state.mping.api_key.is_some() {
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
                    state.mping.settings_modal_open = false;
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
                        if state.mping.api_key.as_deref() != new_key.as_deref() {
                            state.mping.api_key = new_key;
                            state.mping.last_error = None;
                            state.mping.invalidate_requested = true;
                            saved = true;
                        }
                        state.mping.settings_modal_open = false;
                        modal_state.seeded = false;
                    }

                    if state.mping.api_key.is_some()
                        && ui
                            .button(RichText::new("Clear").color(Color32::from_rgb(220, 120, 120)))
                            .clicked()
                    {
                        state.mping.api_key = None;
                        state.mping.reports.clear();
                        state.mping.total_count = 0;
                        state.mping.last_error = None;
                        state.mping.last_success_ms = 0.0;
                        state.mping.invalidate_requested = true;
                        modal_state.key_input.clear();
                        saved = true;
                    }
                });
            });

            ui.add_space(4.0);
        });

    saved
}
