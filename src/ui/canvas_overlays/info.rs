//! Site/sweep info overlay in the top-left corner of the canvas.
//!
//! Displays the current site and product at top, followed by symmetric
//! "Current" and "Previous" sections showing elevation, time, and age.

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
    let overlay_height = if has_prev { 170.0 } else { 105.0 };
    let overlay_rect = Rect::from_min_size(overlay_pos, Vec2::new(310.0, overlay_height));

    ui.scope_builder(egui::UiBuilder::new().max_rect(overlay_rect), |ui| {
        ui.vertical(|ui| {
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
            let header_color = Color32::from_rgb(220, 220, 240);

            // ── Site + Product header ────────────────────────────────
            ui.label(
                RichText::new(format!(
                    "{} \u{00B7} {}",
                    state.viz_state.site_id,
                    state.viz_state.product.label(),
                ))
                .monospace()
                .size(12.0)
                .color(header_color),
            );

            // ── Current sweep section ────────────────────────────────
            draw_sweep_section(
                ui,
                "Current",
                state.viz_state.displayed_sweep_elevation_number,
                &state.viz_state.elevation,
                &state.viz_state.timestamp,
                state.viz_state.data_staleness_secs,
                state.viz_state.data_staleness_start_secs,
                if is_live {
                    let active = state.live_radar_model.active_sweep.as_ref();
                    let chunks = active.map(|s| s.chunks.len() as u32).unwrap_or(0);
                    let expected = active.and_then(|s| s.chunks_expected);
                    let is_partial = active.map(|s| s.radials_received < 360).unwrap_or(false);
                    if is_partial {
                        Some(format!(
                            "streaming{}",
                            expected
                                .map(|e| format!(" {}/{}", chunks, e))
                                .unwrap_or_default()
                        ))
                    } else {
                        Some("live".to_string())
                    }
                } else {
                    None
                },
                info_color,
            );

            // ── Previous sweep section ───────────────────────────────
            if let Some((prev_elev_deg, prev_start, prev_end)) = state.viz_state.prev_sweep_overlay
            {
                ui.add_space(2.0);
                let now = js_sys::Date::now() / 1000.0;
                let prev_age_end = now - prev_end;
                let prev_age_start = now - prev_start;

                let prev_elev_str = format!("{:.1}\u{00B0}", prev_elev_deg);
                let prev_time = format_unix_timestamp_with_date(
                    (prev_start + prev_end) / 2.0,
                    state.use_local_time,
                );

                draw_sweep_section(
                    ui,
                    "Previous",
                    state.viz_state.prev_sweep_elevation_number,
                    &prev_elev_str,
                    &prev_time,
                    if prev_age_end >= 0.0 {
                        Some(prev_age_end)
                    } else {
                        None
                    },
                    if prev_age_start >= 0.0 {
                        Some(prev_age_start)
                    } else {
                        None
                    },
                    None,
                    Color32::from_rgb(170, 170, 190),
                );
            }
        });
    });
}

/// Draw a sweep info section (current or previous) with consistent layout.
#[allow(clippy::too_many_arguments)]
fn draw_sweep_section(
    ui: &mut egui::Ui,
    label: &str,
    elev_num: Option<u8>,
    elev_angle: &str,
    timestamp: &str,
    staleness_end: Option<f64>,
    staleness_start: Option<f64>,
    mode_tag: Option<String>,
    text_color: Color32,
) {
    // Header: "Current" or "Previous" with elevation
    let elev_text = match elev_num {
        Some(n) => format!("{}: Elev {} ({})", label, n, elev_angle),
        None => format!("{}: {}", label, elev_angle),
    };
    ui.label(
        RichText::new(elev_text)
            .monospace()
            .size(12.0)
            .color(text_color),
    );

    // Time
    ui.label(
        RichText::new(format!("  Time: {}", timestamp))
            .monospace()
            .size(11.0)
            .color(text_color),
    );

    // Age + optional mode tag
    if let Some(end_secs) = staleness_end {
        let color = age_color(end_secs);
        let age_str = if end_secs < AGE_RANGE_COLLAPSE_SECS {
            if let Some(start_secs) = staleness_start {
                format!("{} – {}", format_age(start_secs), format_age(end_secs))
            } else {
                format_age(end_secs)
            }
        } else {
            format_age(end_secs)
        };
        let suffix = mode_tag
            .map(|m| format!(" \u{00B7} {}", m))
            .unwrap_or_default();
        ui.label(
            RichText::new(format!("  Age: {}{}", age_str, suffix))
                .monospace()
                .size(11.0)
                .color(color),
        );
    }
}
