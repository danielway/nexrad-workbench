//! Site/sweep info overlay in the top-left corner of the canvas.
//!
//! Displays the current site, product, timestamp, elevation, data age, and
//! rendering mode. During sweep animation or live streaming, also shows the
//! previous sweep's info for comparison including its own age.

use crate::state::AppState;
use eframe::egui::{self, Color32, Rect, RichText, Vec2};

use super::super::canvas::{
    age_color, format_age, format_unix_timestamp_with_date, AGE_RANGE_COLLAPSE_SECS,
    ARCHIVE_AGE_THRESHOLD_SECS,
};

pub(crate) fn draw_overlay_info(ui: &mut egui::Ui, rect: &Rect, state: &AppState) {
    let has_prev = state.viz_state.prev_sweep_overlay.is_some();
    let is_live = state.live_radar_model.active;
    let overlay_pos = rect.left_top() + Vec2::new(10.0, 10.0);
    let overlay_height = if has_prev { 155.0 } else { 110.0 };
    let overlay_rect = Rect::from_min_size(overlay_pos, Vec2::new(310.0, overlay_height));

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
            let dim_color = Color32::from_rgb(150, 150, 170);

            // ── Current sweep ────────────────────────────────────────
            let header = if has_prev { "Current" } else { "Site" };
            ui.label(
                RichText::new(format!(
                    "{}: {} \u{00B7} {}",
                    header,
                    state.viz_state.site_id,
                    state.viz_state.product.label(),
                ))
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

            // Elevation: show both number and angle
            let elev_num = state.viz_state.displayed_sweep_elevation_number;
            let elev_text = match elev_num {
                Some(n) => format!("Elev: {} ({})", n, state.viz_state.elevation),
                None => format!("Elev: {}", state.viz_state.elevation),
            };
            ui.label(
                RichText::new(elev_text)
                    .monospace()
                    .size(12.0)
                    .color(info_color),
            );

            // Age + rendering mode indicator
            if let Some(end_secs) = state.viz_state.data_staleness_secs {
                let color = age_color(end_secs);
                let age_str = if end_secs < AGE_RANGE_COLLAPSE_SECS {
                    if let Some(start_secs) = state.viz_state.data_staleness_start_secs {
                        format!("{} – {}", format_age(start_secs), format_age(end_secs))
                    } else {
                        format_age(end_secs)
                    }
                } else {
                    format_age(end_secs)
                };
                let mode = if is_live {
                    let active = state.live_radar_model.active_sweep.as_ref();
                    let is_partial = active.map(|s| s.radials_received < 360).unwrap_or(false);
                    if is_partial {
                        " \u{00B7} streaming"
                    } else {
                        " \u{00B7} live"
                    }
                } else {
                    ""
                };
                ui.label(
                    RichText::new(format!("Age: {}{}", age_str, mode))
                        .monospace()
                        .size(12.0)
                        .color(color),
                );
            }

            // ── Previous sweep ───────────────────────────────────────
            if let Some((prev_elev_deg, prev_start, prev_end)) = state.viz_state.prev_sweep_overlay
            {
                ui.add_space(2.0);
                let prev_color = Color32::from_rgb(170, 170, 190);

                let prev_elev_num = state.viz_state.prev_sweep_elevation_number;
                let prev_elev_text = match prev_elev_num {
                    Some(n) => format!("Previous: Elev {} ({:.1}\u{00B0})", n, prev_elev_deg),
                    None => format!("Previous: {:.1}\u{00B0}", prev_elev_deg),
                };
                ui.label(
                    RichText::new(prev_elev_text)
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
                        .color(dim_color),
                );

                // Previous sweep age
                let now = js_sys::Date::now() / 1000.0;
                let prev_age_end = now - prev_end;
                if prev_age_end >= 0.0 {
                    let prev_age_color = age_color(prev_age_end);
                    let prev_age_str = if prev_age_end < AGE_RANGE_COLLAPSE_SECS {
                        let prev_age_start = now - prev_start;
                        format!(
                            "Age: {} – {}",
                            format_age(prev_age_start),
                            format_age(prev_age_end)
                        )
                    } else {
                        format!("Age: {}", format_age(prev_age_end))
                    };
                    ui.label(
                        RichText::new(prev_age_str)
                            .monospace()
                            .size(12.0)
                            .color(prev_age_color),
                    );
                }
            }
        });
    });
}
