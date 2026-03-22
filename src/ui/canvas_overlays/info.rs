//! Site/sweep info overlay in the top-left corner of the canvas.
//!
//! Displays the current site, timestamp, elevation, and data age. During
//! sweep animation also shows the previous sweep's info for comparison.
//! An "ARCHIVE DATA" banner appears when data staleness exceeds the threshold.

use crate::state::AppState;
use eframe::egui::{self, Color32, Rect, RichText, Vec2};

use super::super::canvas::{
    age_color, format_age, format_unix_timestamp_with_date, AGE_RANGE_COLLAPSE_SECS,
    ARCHIVE_AGE_THRESHOLD_SECS,
};

pub(crate) fn draw_overlay_info(ui: &mut egui::Ui, rect: &Rect, state: &AppState) {
    let has_prev = state.viz_state.prev_sweep_overlay.is_some();
    let overlay_pos = rect.left_top() + Vec2::new(10.0, 10.0);
    let overlay_height = if has_prev { 130.0 } else { 90.0 };
    let overlay_rect = Rect::from_min_size(overlay_pos, Vec2::new(290.0, overlay_height));

    ui.scope_builder(egui::UiBuilder::new().max_rect(overlay_rect), |ui| {
        ui.vertical(|ui| {
            // Show loud "ARCHIVE DATA" banner when data is old enough to be confusable
            let is_archive = state
                .viz_state
                .data_staleness_secs
                .is_some_and(|s| s > ARCHIVE_AGE_THRESHOLD_SECS);
            if is_archive {
                ui.label(
                    RichText::new("ARCHIVE DATA")
                        .monospace()
                        .size(14.0)
                        .strong()
                        .color(Color32::from_rgb(255, 160, 40)),
                );
            }

            let info_color = Color32::from_rgb(200, 200, 220);
            let label = if has_prev { "Current" } else { "Site" };
            ui.label(
                RichText::new(format!("{}: {}", label, state.viz_state.site_id))
                    .monospace()
                    .size(12.0)
                    .color(info_color),
            );
            ui.label(
                RichText::new(format!("Time: {}", state.viz_state.timestamp))
                    .monospace()
                    .size(12.0)
                    .color(info_color),
            );
            ui.label(
                RichText::new(format!("Elev: {}", state.viz_state.elevation))
                    .monospace()
                    .size(12.0)
                    .color(info_color),
            );
            if let Some(end_secs) = state.viz_state.data_staleness_secs {
                let color = age_color(end_secs);
                let age_text = if end_secs < AGE_RANGE_COLLAPSE_SECS {
                    if let Some(start_secs) = state.viz_state.data_staleness_start_secs {
                        format!("Age: {} – {}", format_age(start_secs), format_age(end_secs),)
                    } else {
                        format!("Age: {}", format_age(end_secs))
                    }
                } else {
                    format!("Age: {}", format_age(end_secs))
                };
                ui.label(RichText::new(age_text).monospace().size(12.0).color(color));
            }

            // Previous sweep info during sweep animation
            if let Some((prev_elev, prev_start, prev_end)) = state.viz_state.prev_sweep_overlay {
                ui.add_space(2.0);
                let prev_color = Color32::from_rgb(170, 170, 190);
                ui.label(
                    RichText::new("Previous")
                        .monospace()
                        .size(12.0)
                        .color(prev_color),
                );
                let prev_time = format_unix_timestamp_with_date(
                    (prev_start + prev_end) / 2.0,
                    state.use_local_time,
                );
                ui.label(
                    RichText::new(format!("Time: {}", prev_time))
                        .monospace()
                        .size(12.0)
                        .color(prev_color),
                );
                ui.label(
                    RichText::new(format!("Elev: {:.1}\u{00B0}", prev_elev))
                        .monospace()
                        .size(12.0)
                        .color(prev_color),
                );
            }
        });
    });
}
