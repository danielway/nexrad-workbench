//! Canvas mouse/keyboard interaction handlers.
//!
//! Separates input handling from rendering: pan (drag), zoom (scroll),
//! distance tool clicks, globe orbit/translate, and double-click reset.

use crate::data::NEXRAD_SITES;
use crate::geo::MapProjection;
use crate::state::AppState;
use eframe::egui::{self, Rect, Vec2};
use geo_types::Coord;

use super::site_modal::apply_site_selection;

/// Pixel radius around a site marker that counts as a click hit.
const SITE_HIT_RADIUS_PX: f32 = 10.0;

pub(crate) fn handle_globe_interaction(
    response: &egui::Response,
    rect: &Rect,
    state: &mut AppState,
) {
    use crate::geo::camera::CameraMode;

    // Multi-touch: two-finger pinch zooms, two-finger drag pans the pivot
    // (orbit modes). When a pinch is active we skip the single-finger drag
    // and scroll-wheel branches below to avoid double-applying motion.
    if let Some(t) = super::mobile::gestures::consume(&response.ctx) {
        if (t.zoom - 1.0).abs() > f32::EPSILON {
            // Camera's zoom() takes a scroll-like delta; convert the
            // proportional zoom_delta into a comparable magnitude.
            let scroll_equivalent = (t.zoom - 1.0) * 120.0;
            state.viz_state.camera.zoom(scroll_equivalent);
        }
        if t.pan != Vec2::ZERO {
            let viewport_h = response.rect.height();
            match state.viz_state.camera.mode {
                CameraMode::PlanetOrbit | CameraMode::SiteOrbit => {
                    state
                        .viz_state
                        .camera
                        .pan_pivot(t.pan.x, t.pan.y, viewport_h);
                }
                CameraMode::FreeLook => {
                    state
                        .viz_state
                        .camera
                        .free_translate(t.pan.x, t.pan.y, viewport_h);
                }
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

        match state.viz_state.camera.mode {
            CameraMode::FreeLook => {
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
            CameraMode::PlanetOrbit | CameraMode::SiteOrbit => {
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

pub(crate) fn handle_canvas_interaction(
    response: &egui::Response,
    rect: &Rect,
    state: &mut AppState,
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
            // Alert hit-testing first: when the alerts overlay is on, a click
            // inside an alert polygon opens that alert's detail modal.
            let mut handled = false;
            if state.layer_state.geo.alerts {
                let geo = projection.screen_to_geo(click_pos);
                let bounds = projection.visible_bounds();
                let mut best: Option<(u8, String)> = None;
                for alert in &state.alerts.alerts {
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
            // Fall through to site-marker click selection.
            if !handled {
                if let Some((site_id, lat, lon)) = pick_site_at(click_pos, projection, state) {
                    apply_site_selection(state, site_id, lat, lon);
                }
            }
        }
    }

    // Multi-touch takes priority over single-finger drag + scroll so a
    // two-finger pinch doesn't double-apply motion through both paths.
    let touch = super::mobile::gestures::consume(&response.ctx);

    if let Some(t) = touch {
        // Pinch-zoom anchored on the gesture focus.
        if (t.zoom - 1.0).abs() > f32::EPSILON {
            let old_zoom = state.viz_state.zoom;
            let new_zoom = (old_zoom * t.zoom).clamp(0.1, 25.0);
            let focus_rel = t.focus - rect.center();
            let ratio = new_zoom / old_zoom;
            state.viz_state.pan_offset =
                focus_rel * (1.0 - ratio) + state.viz_state.pan_offset * ratio;
            state.viz_state.zoom = new_zoom;
        }
        // Two-finger drag = pan.
        state.viz_state.pan_offset += t.pan;
    } else {
        if response.dragged() {
            state.viz_state.pan_offset += response.drag_delta();
        }

        if response.hovered() {
            let scroll_delta = response.ctx.input(|i| i.raw_scroll_delta);
            if scroll_delta.y != 0.0 {
                let zoom_factor = 1.0 + scroll_delta.y * 0.001;
                let old_zoom = state.viz_state.zoom;
                let new_zoom = (old_zoom * zoom_factor).clamp(0.1, 25.0);

                if let Some(cursor_pos) = response.hover_pos() {
                    let cursor_rel = cursor_pos - rect.center();
                    let ratio = new_zoom / old_zoom;
                    state.viz_state.pan_offset =
                        cursor_rel * (1.0 - ratio) + state.viz_state.pan_offset * ratio;
                }

                state.viz_state.zoom = new_zoom;
            }
        }
    }

    if response.double_clicked() {
        state.viz_state.zoom = 1.0;
        state.viz_state.pan_offset = Vec2::ZERO;
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
