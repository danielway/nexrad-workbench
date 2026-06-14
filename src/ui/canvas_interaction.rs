//! Canvas mouse/keyboard interaction handlers.
//!
//! Separates input handling from rendering: pan (drag), zoom (scroll),
//! distance tool clicks, globe orbit/translate, and double-click reset.

use crate::data::NEXRAD_SITES;
use crate::geo::MapProjection;
use crate::mping::StormReport;
use crate::state::AppState;
use eframe::egui::{self, Rect, Vec2};
use geo_types::Coord;

use super::site_modal::apply_site_selection;

/// Pixel radius around a site marker that counts as a click hit.
const SITE_HIT_RADIUS_PX: f32 = 10.0;

/// Pixel radius around an mPING report marker that counts as a click hit.
/// Slightly larger than the rendered dot (4.5 px) so small targets are
/// still easy to hit on touch screens.
const MPING_HIT_RADIUS_PX: f32 = 9.0;

pub(crate) fn handle_globe_interaction(
    response: &egui::Response,
    rect: &Rect,
    state: &mut AppState,
) {
    use crate::geo::Camera;

    // Multi-touch: two-finger pinch zooms, two-finger drag pans the pivot
    // (orbit modes). When a pinch is active we skip the single-finger drag
    // and scroll-wheel branches below to avoid double-applying motion. A pinch
    // focused over the timeline belongs to the strip, not the globe.
    if let Some(t) =
        super::mobile::gestures::consume(&response.ctx).filter(|t| rect.contains(t.focus))
    {
        if (t.zoom - 1.0).abs() > f32::EPSILON {
            // Camera's zoom() takes a scroll-like delta; convert the
            // proportional zoom_delta into a comparable magnitude.
            let scroll_equivalent = (t.zoom - 1.0) * 120.0;
            state.viz_state.camera.zoom(scroll_equivalent);
        }
        if t.pan != Vec2::ZERO {
            let viewport_h = response.rect.height();
            match state.viz_state.camera {
                Camera::PlanetOrbit(_) | Camera::SiteOrbit(_) => {
                    state
                        .viz_state
                        .camera
                        .pan_pivot(t.pan.x, t.pan.y, viewport_h);
                }
                Camera::FreeLook(_) => {
                    state
                        .viz_state
                        .camera
                        .free_translate(t.pan.x, t.pan.y, viewport_h);
                }
                // Flat2D never reaches the globe interaction handler.
                Camera::Flat2D(_) => {}
            }
        }
        // Double-click still falls through below.
        if response.double_clicked() {
            if let Some(click_pos) = response.interact_pointer_pos() {
                if let Some((lat, lon)) = state.viz_state.camera.screen_to_geo(click_pos, *rect) {
                    state.viz_state.camera.move_pivot_to(lat, lon);
                } else {
                    state.viz_state.camera.recenter();
                }
            }
        }
        return;
    }

    if response.dragged() {
        let delta = response.drag_delta();
        let viewport_h = response.rect.height();
        let shift_held = response.ctx.input(|i| i.modifiers.shift);
        let right_button = response.dragged_by(egui::PointerButton::Secondary);
        let middle_button = response.dragged_by(egui::PointerButton::Middle);

        match state.viz_state.camera {
            Camera::FreeLook(_) => {
                if middle_button || (shift_held && !right_button) {
                    // Middle-drag or Shift+left: translate camera sideways
                    state
                        .viz_state
                        .camera
                        .free_translate(delta.x, delta.y, viewport_h);
                } else if right_button {
                    // Right-drag: look around without moving
                    state
                        .viz_state
                        .camera
                        .free_look(delta.x, delta.y, viewport_h);
                } else {
                    // Left-drag: look around (primary control in free look)
                    state
                        .viz_state
                        .camera
                        .free_look(delta.x, delta.y, viewport_h);
                }
            }
            Camera::PlanetOrbit(_) | Camera::SiteOrbit(_) => {
                if middle_button || (shift_held && !right_button) {
                    state
                        .viz_state
                        .camera
                        .pan_pivot(delta.x, delta.y, viewport_h);
                } else if right_button {
                    // Right-drag: horizontal rotates (heading), vertical pitches
                    state.viz_state.camera.orbit(delta.x, delta.y, viewport_h);
                } else {
                    // Left-drag: orbit
                    state.viz_state.camera.orbit(delta.x, delta.y, viewport_h);
                }
            }
            // Flat2D never reaches the globe interaction handler.
            Camera::Flat2D(_) => {}
        }
    }

    if response.hovered() {
        let scroll_delta = response.ctx.input(|i| i.raw_scroll_delta);
        if scroll_delta.y != 0.0 {
            state.viz_state.camera.zoom(scroll_delta.y);
        }
    }

    // Double-click: move pivot to clicked surface point
    if response.double_clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            if let Some((lat, lon)) = state.viz_state.camera.screen_to_geo(click_pos, *rect) {
                state.viz_state.camera.move_pivot_to(lat, lon);
            } else {
                // Clicked off-globe: recenter on site
                state.viz_state.camera.recenter();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_canvas_interaction(
    response: &egui::Response,
    rect: &Rect,
    state: &mut AppState,
    playback: &crate::subsystem::Playback,
    chrome: &mut crate::subsystem::Chrome,
    diagnostics: &mut crate::subsystem::Diagnostics,
    derived: &crate::subsystem::Derived,
    projection: &MapProjection,
) {
    // Distance tool: click to place points
    if state.viz_state.distance_tool_active && response.clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            let geo = projection.screen_to_geo(click_pos);
            if state.viz_state.distance_start.is_none() || state.viz_state.distance_end.is_some() {
                // First click or restart: set start, clear end
                state.viz_state.distance_start = Some((geo.y, geo.x));
                state.viz_state.distance_end = None;
            } else {
                // Second click: set end
                state.viz_state.distance_end = Some((geo.y, geo.x));
            }
        }
    } else if response.clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            // Point-like markers (sites, mPING) sit visually on top of alert
            // polygons, so they hit-test first; alerts catch the rest.
            let mut handled = false;
            if let Some((site_id, lat, lon)) = pick_site_at(click_pos, projection, state) {
                apply_site_selection(state, chrome, site_id, lat, lon);
                handled = true;
            }
            if !handled && state.layer_state.geo.mping {
                if let Some(id) = pick_mping_report_at(
                    click_pos,
                    projection,
                    &diagnostics.mping.reports,
                    playback.state.playback_position(),
                ) {
                    diagnostics.mping.selected_report_id = Some(id);
                    handled = true;
                }
            }
            let show_warnings = state.layer_state.geo.alerts_warnings;
            let show_other = state.layer_state.geo.alerts_other;
            if !handled && (show_warnings || show_other) && derived.data_is_live {
                let geo = projection.screen_to_geo(click_pos);
                let bounds = projection.visible_bounds();
                let mut best: Option<(u8, String)> = None;
                for alert in &diagnostics.alerts.alerts {
                    // Only hit-test alerts whose class is actually visible.
                    if !(if alert.is_warning() {
                        show_warnings
                    } else {
                        show_other
                    }) {
                        continue;
                    }
                    if !crate::alerts::bbox_intersects(alert, bounds) {
                        continue;
                    }
                    if crate::alerts::contains_point(alert, geo.x, geo.y) {
                        let rank = alert.severity.rank();
                        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
                            best = Some((rank, alert.id.clone()));
                        }
                    }
                }
                if let Some((_, id)) = best {
                    state.push_command(crate::state::AppCommand::OpenAlert(id));
                    handled = true;
                }
            }
            // Click missed every interactive overlay — dismiss any open
            // mPING popover.
            if !handled {
                diagnostics.mping.selected_report_id = None;
            }
        }
    }

    // Multi-touch takes priority over single-finger drag + scroll so a
    // two-finger pinch doesn't double-apply motion through both paths. A pinch
    // whose focus is over the timeline belongs to the timeline (it zooms the
    // strip); ignore it here so the canvas and strip don't both consume it.
    let touch = super::mobile::gestures::consume(&response.ctx).filter(|t| rect.contains(t.focus));

    if let Some(t) = touch {
        // Pinch-zoom anchored on the gesture focus.
        if (t.zoom - 1.0).abs() > f32::EPSILON {
            let old_zoom = state.viz_state.zoom();
            let new_zoom = (old_zoom * t.zoom).clamp(0.1, 25.0);
            let focus_rel = t.focus - rect.center();
            let ratio = new_zoom / old_zoom;
            state
                .viz_state
                .set_pan_offset(focus_rel * (1.0 - ratio) + state.viz_state.pan_offset() * ratio);
            state.viz_state.set_zoom(new_zoom);
        }
        // Two-finger drag = pan.
        state
            .viz_state
            .set_pan_offset(state.viz_state.pan_offset() + t.pan);
    } else {
        if response.dragged() {
            state
                .viz_state
                .set_pan_offset(state.viz_state.pan_offset() + response.drag_delta());
        }

        if response.hovered() {
            let scroll_delta = response.ctx.input(|i| i.raw_scroll_delta);
            if scroll_delta.y != 0.0 {
                let zoom_factor = 1.0 + scroll_delta.y * 0.001;
                let old_zoom = state.viz_state.zoom();
                let new_zoom = (old_zoom * zoom_factor).clamp(0.1, 25.0);

                if let Some(cursor_pos) = response.hover_pos() {
                    let cursor_rel = cursor_pos - rect.center();
                    let ratio = new_zoom / old_zoom;
                    state.viz_state.set_pan_offset(
                        cursor_rel * (1.0 - ratio) + state.viz_state.pan_offset() * ratio,
                    );
                }

                state.viz_state.set_zoom(new_zoom);
            }
        }
    }

    if response.double_clicked() {
        state.viz_state.set_zoom(1.0);
        state.viz_state.set_pan_offset(Vec2::ZERO);
    }
}

/// Return `(site_id, lat, lon)` for the NEXRAD site closest to `click_pos`
/// within [`SITE_HIT_RADIUS_PX`], or `None` if no site was hit. The currently
/// active site is excluded so re-selecting it is a no-op rather than a spurious
/// camera recenter.
fn pick_site_at(
    click_pos: egui::Pos2,
    projection: &MapProjection,
    state: &AppState,
) -> Option<(&'static str, f64, f64)> {
    let (min_lon, min_lat, max_lon, max_lat) = projection.visible_bounds();
    let padding = 2.0;
    let current_upper = state.viz_state.site_id.to_uppercase();
    let hit_radius_sq = SITE_HIT_RADIUS_PX * SITE_HIT_RADIUS_PX;

    let mut best: Option<(&'static str, f64, f64, f32)> = None;
    for site in NEXRAD_SITES.iter() {
        if site.id == current_upper {
            continue;
        }
        if site.lat < min_lat - padding
            || site.lat > max_lat + padding
            || site.lon < min_lon - padding
            || site.lon > max_lon + padding
        {
            continue;
        }
        let screen_pos = projection.geo_to_screen(Coord {
            x: site.lon,
            y: site.lat,
        });
        let dist_sq = (screen_pos - click_pos).length_sq();
        if dist_sq <= hit_radius_sq && best.is_none_or(|(_, _, _, d)| dist_sq < d) {
            best = Some((site.id, site.lat, site.lon, dist_sq));
        }
    }
    best.map(|(id, lat, lon, _)| (id, lat, lon))
}

/// Return the id of the mPING report whose marker is closest to `click_pos`
/// and within [`MPING_HIT_RADIUS_PX`], or `None` if no marker was hit.
fn pick_mping_report_at(
    click_pos: egui::Pos2,
    projection: &MapProjection,
    reports: &[StormReport],
    playback_secs: f64,
) -> Option<i64> {
    let (min_lon, min_lat, max_lon, max_lat) = projection.visible_bounds();
    let padding = 0.5;
    let hit_radius_sq = MPING_HIT_RADIUS_PX * MPING_HIT_RADIUS_PX;

    let mut best: Option<(i64, f32)> = None;
    for report in reports {
        // Don't hit-test markers that aren't drawn (future of the playhead).
        if !report.visible_at(playback_secs) {
            continue;
        }
        if report.lat < min_lat - padding
            || report.lat > max_lat + padding
            || report.lon < min_lon - padding
            || report.lon > max_lon + padding
        {
            continue;
        }
        let screen_pos = projection.geo_to_screen(Coord {
            x: report.lon,
            y: report.lat,
        });
        let dist_sq = (screen_pos - click_pos).length_sq();
        if dist_sq <= hit_radius_sq && best.is_none_or(|(_, d)| dist_sq < d) {
            best = Some((report.id, dist_sq));
        }
    }
    best.map(|(id, _)| id)
}
