//! Site selection modal overlay.
//!
//! Presents all NEXRAD sites in a searchable list. Opens automatically on
//! first launch (no prior site) and can be reopened from the top bar.

use crate::data::{all_sites_sorted, get_site};
use crate::state::AppState;
use eframe::egui::{self, Color32, RichText, Vec2};

/// Persistent state for the site modal's search filter.
/// Stored outside AppState to avoid cluttering it with transient UI state.
#[derive(Default)]
pub struct SiteModalState {
    pub filter: String,
}

/// Render the site selection modal if open.
///
/// Returns `true` if a site was selected (so the caller can trigger acquisition).
pub fn render_site_modal(
    ctx: &egui::Context,
    state: &mut AppState,
    modal_state: &mut SiteModalState,
) -> bool {
    if !state.site_modal_open {
        return false;
    }

    let mut selected = false;

    // Semi-transparent backdrop
    egui::Area::new(egui::Id::new("site_modal_backdrop"))
        .fixed_pos(egui::Pos2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let screen_rect = ctx.input(|i| i.viewport_rect());
            let (response, painter) = ui.allocate_painter(screen_rect.size(), egui::Sense::click());
            painter.rect_filled(
                screen_rect,
                0.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 160),
            );
            // Click backdrop to close
            if response.clicked() {
                // Only close if a site has already been selected (not first-run)
                if get_site(&state.viz_state.site_id).is_some() {
                    state.site_modal_open = false;
                }
            }
        });

    // Modal window
    egui::Window::new("Select Radar Site")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .fixed_size(Vec2::new(420.0, 500.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Search/filter input
            ui.horizontal(|ui| {
                ui.label("Search:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut modal_state.filter)
                        .hint_text("Site ID, name, or state...")
                        .desired_width(300.0),
                );
                // Auto-focus the search field
                if state.site_modal_open {
                    response.request_focus();
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            // Filter sites
            let filter_upper = modal_state.filter.to_uppercase();
            let sites = all_sites_sorted();
            let filtered: Vec<_> = if modal_state.filter.is_empty() {
                sites.clone()
            } else {
                sites
                    .into_iter()
                    .filter(|s| {
                        s.id.contains(&filter_upper)
                            || s.name.contains(&filter_upper)
                            || s.state
                                .map(|st| st.to_uppercase().contains(&filter_upper))
                                .unwrap_or(false)
                    })
                    .collect()
            };

            // Site count
            ui.label(
                RichText::new(format!("{} sites", filtered.len()))
                    .small()
                    .color(Color32::GRAY),
            );

            ui.add_space(4.0);

            // Scrollable site list
            egui::ScrollArea::vertical()
                .max_height(380.0)
                .show(ui, |ui| {
                    for site in &filtered {
                        let is_current = site.id == state.viz_state.site_id;
                        let label = site.display_label();

                        let text = if is_current {
                            RichText::new(format!("{} \u{2713}", label))
                                .color(Color32::from_rgb(100, 200, 255))
                        } else {
                            RichText::new(label)
                        };

                        if ui.selectable_label(is_current, text).clicked() && !is_current {
                            state.viz_state.site_id = site.id.to_string();
                            state.viz_state.center_lat = site.lat;
                            state.viz_state.center_lon = site.lon;
                            state.viz_state.pan_offset = Vec2::ZERO;
                            state.timeline_needs_refresh = true;
                            state.auto_position_on_timeline_load = true;
                            state.site_modal_open = false;
                            modal_state.filter.clear();
                            selected = true;
                        }
                    }
                });
        });

    selected
}
