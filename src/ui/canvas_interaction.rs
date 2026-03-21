use crate::geo::MapProjection;
use crate::state::AppState;
use eframe::egui::{self, Rect, Vec2};

pub(crate) fn handle_globe_interaction(
    response: &egui::Response,
    rect: &Rect,
    state: &mut AppState,
) {
    use crate::geo::camera::CameraMode;

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
    if state.distance_tool_active && response.clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            let geo = projection.screen_to_geo(click_pos);
            if state.distance_start.is_none() || state.distance_end.is_some() {
                // First click or restart: set start, clear end
                state.distance_start = Some((geo.y, geo.x));
                state.distance_end = None;
            } else {
                // Second click: set end
                state.distance_end = Some((geo.y, geo.x));
            }
        }
    }

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

    if response.double_clicked() {
        state.viz_state.zoom = 1.0;
        state.viz_state.pan_offset = Vec2::ZERO;
    }
}
