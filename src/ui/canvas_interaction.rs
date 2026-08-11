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
    // Multi-touch: two-finger pinch zooms, two-finger drag pans the pivot.
    // When a pinch is active we skip the single-finger drag and scroll-wheel
    // branches below to avoid double-applying motion. A pinch focused over
    // the timeline belongs to the strip, not the globe.
    if let Some(t) =
        super::mobile::gestures::consume(&response.ctx).filter(|t| rect.contains(t.focus))
    {
        if (t.zoom - 1.0).abs() > f32::EPSILON {
            // Camera's zoom_about() takes a scroll-like delta; convert the
            // proportional zoom_delta into a comparable magnitude. Anchored
            // on the pinch focus like the 2D path.
            let scroll_equivalent = (t.zoom - 1.0) * 120.0;
            state
                .viz_state
                .camera
                .zoom_about(scroll_equivalent, Some(t.focus), *rect);
        }
        if t.pan != Vec2::ZERO {
            let viewport_h = response.rect.height();
            state
                .viz_state
                .camera
                .pan_pivot(t.pan.x, t.pan.y, viewport_h);
        }
        // Double-click still falls through below.
        handle_globe_double_click(response, rect, state);
        return;
    }

    if response.dragged() {
        let delta = response.drag_delta();
        let viewport_h = response.rect.height();
        let ctrl_held = response.ctx.input(|i| i.modifiers.ctrl);
        let right_button = response.dragged_by(egui::PointerButton::Secondary);
        let middle_button = response.dragged_by(egui::PointerButton::Middle);

        if right_button || middle_button || ctrl_held {
            // Right / middle / Ctrl+left drag: horizontal rotates the
            // heading, vertical tilts toward/away from the horizon.
            state
                .viz_state
                .camera
                .adjust_tilt_heading(delta.x, delta.y, viewport_h);
        } else {
            // Left-drag: pan the pivot. Prefer the grab pan (the surface
            // point under the cursor sticks to it exactly); fall back to
            // the delta pan when the cursor is off-globe or the view is
            // tilted near the horizon.
            let grabbed = response.interact_pointer_pos().is_some_and(|pos| {
                state
                    .viz_state
                    .camera
                    .pan_pivot_grab(pos - delta, pos, *rect)
            });
            if !grabbed {
                state
                    .viz_state
                    .camera
                    .pan_pivot(delta.x, delta.y, viewport_h);
            }
        }
    }

    if response.hovered() {
        let scroll_delta = response.ctx.input(|i| i.smooth_scroll_delta);
        if scroll_delta.y != 0.0 {
            state
                .viz_state
                .camera
                .zoom_about(scroll_delta.y, response.hover_pos(), *rect);
        }
    }

    handle_globe_double_click(response, rect, state);
}

/// Double-click: move the pivot to the clicked surface point, or back to
/// the radar site when the click misses the globe.
fn handle_globe_double_click(response: &egui::Response, rect: &Rect, state: &mut AppState) {
    if response.double_clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            if let Some((lat, lon)) = state.viz_state.camera.screen_to_geo(click_pos, *rect) {
                state.viz_state.camera.move_pivot_to(lat, lon);
            } else {
                state.viz_state.camera.focus_site();
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
    diagnostics: &crate::subsystem::Diagnostics,
    derived: &crate::subsystem::Derived,
    projection: &MapProjection,
) {
    // Distance tool: click to place points. Which endpoint the click lands on
    // is the core's call (`decide_distance_click`), applied by the shell.
    if state.viz_state.distance_tool_active && response.clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            let geo = projection.screen_to_geo(click_pos);
            state.push_command(crate::core::Intent::PlaceDistancePoint {
                lat: geo.y,
                lon: geo.x,
            });
        }
    } else if response.clicked() {
        if let Some(click_pos) = response.interact_pointer_pos() {
            // Point-like markers (sites, mPING) sit visually on top of alert
            // polygons, so they hit-test first; alerts catch the rest.
            let mut handled = false;
            if let Some((site_id, lat, lon)) = pick_site_at(click_pos, projection, state) {
                state.push_command(crate::core::Intent::SelectSite {
                    site_id: site_id.to_string(),
                    lat,
                    lon,
                });
                handled = true;
            }
            if !handled && state.layer_state.geo.mping {
                if let Some(id) = pick_mping_report_at(
                    click_pos,
                    projection,
                    &diagnostics.mping.reports,
                    playback.state.playback_position(),
                ) {
                    state.push_command(crate::core::Intent::Diagnostics(
                        crate::core::diagnostics::DiagnosticsIntent::SelectMpingReport(id),
                    ));
                    handled = true;
                }
            }
            let show_warnings = state.layer_state.geo.alerts_warnings;
            let show_other = state.layer_state.geo.alerts_other;
            if !handled && (show_warnings || show_other) && derived.data_is_live {
                let geo = projection.screen_to_geo(click_pos);
                let bounds = projection.visible_bounds();
                // Pure hit-test + severity-rank tie-break lives in the core.
                if let Some(id) = crate::core::diagnostics::select_alert_at(
                    &diagnostics.alerts.alerts,
                    geo.x,
                    geo.y,
                    bounds,
                    show_warnings,
                    show_other,
                ) {
                    state.push_command(crate::core::Intent::Diagnostics(
                        crate::core::diagnostics::DiagnosticsIntent::SelectAlert(id),
                    ));
                    handled = true;
                }
            }
            // Click missed every interactive overlay — dismiss any open
            // mPING popover.
            if !handled {
                state.push_command(crate::core::Intent::Diagnostics(
                    crate::core::diagnostics::DiagnosticsIntent::ClearMpingSelection,
                ));
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
            let scroll_delta = response.ctx.input(|i| i.smooth_scroll_delta);
            if scroll_delta.y != 0.0 {
                // Same log-space step per scroll unit as the 3D camera, so
                // the wheel feels identical in both views.
                let zoom_factor =
                    (scroll_delta.y * crate::geo::camera::ZOOM_LOG_PER_SCROLL_UNIT).exp();
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
        state.viz_state.set_zoom(crate::geo::DEFAULT_FLAT_ZOOM);
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    // KABR is a real NEXRAD site; centering the projection on its coords makes
    // KABR project to the exact screen center. AppState::default()'s site_id is
    // "KDMX" (not KABR), so KABR is a live click candidate unless we exclude it.
    const KABR_LAT: f64 = 45.45583;
    const KABR_LON: f64 = -98.41306;

    /// Projection centered on KABR, 800x600 at origin (screen center = 400,300),
    /// zoom 1.0, no pan.
    fn proj_on_kabr() -> MapProjection {
        let mut p = MapProjection::new(KABR_LAT, KABR_LON);
        p.update(
            1.0,
            Vec2::ZERO,
            Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0)),
        );
        p
    }

    /// A StormReport placed at a chosen screen pixel via the projection inverse,
    /// so geo_to_screen round-trips it back to that pixel.
    fn report_at_pixel(
        p: &MapProjection,
        id: i64,
        px: f32,
        py: f32,
        obtime_ms: f64,
    ) -> StormReport {
        let geo = p.screen_to_geo(egui::Pos2::new(px, py));
        StormReport {
            id,
            obtime_ms,
            category: crate::mping::ReportCategory::Other,
            description: String::new(),
            lat: geo.y,
            lon: geo.x,
        }
    }

    // ---- pick_site_at -------------------------------------------------------

    #[wasm_bindgen_test]
    fn site_click_at_center_hits_centered_site() {
        let p = proj_on_kabr();
        let state = AppState::default(); // site_id = "KDMX", so KABR is a candidate
                                         // KABR projects to screen center (400,300); clicking there is a direct hit.
        let hit = pick_site_at(egui::Pos2::new(400.0, 300.0), &p, &state);
        match hit {
            Some((id, lat, lon)) => {
                assert!(id == "KABR");
                assert!((lat - KABR_LAT).abs() < 1e-9);
                assert!((lon - KABR_LON).abs() < 1e-9);
            }
            None => panic!("expected a site hit at the centered site"),
        }
    }

    #[wasm_bindgen_test]
    fn site_click_within_radius_still_hits() {
        let p = proj_on_kabr();
        let state = AppState::default();
        // 7px from center: dist_sq = 49 <= 100 (SITE_HIT_RADIUS_PX^2).
        let hit = pick_site_at(egui::Pos2::new(407.0, 300.0), &p, &state);
        assert!(matches!(hit, Some((id, _, _)) if id == "KABR"));
    }

    #[wasm_bindgen_test]
    fn site_click_just_outside_radius_misses() {
        let p = proj_on_kabr();
        let state = AppState::default();
        // 11px from center: dist_sq = 121 > 100, and the next-nearest site (KFSD)
        // is ~80px away, so nothing is within the hit radius.
        let hit = pick_site_at(egui::Pos2::new(411.0, 300.0), &p, &state);
        assert!(hit.is_none());
    }

    #[wasm_bindgen_test]
    fn site_click_far_corner_misses() {
        let p = proj_on_kabr();
        let state = AppState::default();
        let hit = pick_site_at(egui::Pos2::new(5.0, 5.0), &p, &state);
        assert!(hit.is_none());
    }

    #[wasm_bindgen_test]
    fn active_site_is_excluded_from_hit() {
        let p = proj_on_kabr();
        let mut state = AppState::default();
        // Make KABR the active site: it must be excluded, and no other site is
        // within 10px of center, so the center click now misses.
        state.viz_state.site_id = "KABR".to_string();
        let hit = pick_site_at(egui::Pos2::new(400.0, 300.0), &p, &state);
        assert!(hit.is_none());
    }

    #[wasm_bindgen_test]
    fn active_site_match_is_case_insensitive() {
        let p = proj_on_kabr();
        let mut state = AppState::default();
        // Lower-case site_id is upper-cased before comparison, so KABR is still
        // excluded and the center click misses.
        state.viz_state.site_id = "kabr".to_string();
        let hit = pick_site_at(egui::Pos2::new(400.0, 300.0), &p, &state);
        assert!(hit.is_none());
    }

    // ---- pick_mping_report_at ----------------------------------------------

    #[wasm_bindgen_test]
    fn mping_click_at_marker_hits_visible_report() {
        let p = proj_on_kabr();
        // Report observed at 1000ms; playback at 1000s (=1_000_000ms) => visible.
        let reports = vec![report_at_pixel(&p, 42, 400.0, 300.0, 1000.0)];
        let hit = pick_mping_report_at(egui::Pos2::new(400.0, 300.0), &p, &reports, 1000.0);
        assert!(hit == Some(42));
    }

    #[wasm_bindgen_test]
    fn mping_future_report_not_hit_testable() {
        let p = proj_on_kabr();
        // obtime 1000ms; playback at 0s => report is in the future of the
        // playhead and must not be hit-tested.
        let reports = vec![report_at_pixel(&p, 42, 400.0, 300.0, 1000.0)];
        let hit = pick_mping_report_at(egui::Pos2::new(400.0, 300.0), &p, &reports, 0.0);
        assert!(hit.is_none());
    }

    #[wasm_bindgen_test]
    fn mping_click_outside_radius_misses() {
        let p = proj_on_kabr();
        let reports = vec![report_at_pixel(&p, 42, 400.0, 300.0, 0.0)];
        // 10px below the marker: dist_sq = 100 > 81 (MPING_HIT_RADIUS_PX^2).
        let hit = pick_mping_report_at(egui::Pos2::new(400.0, 310.0), &p, &reports, 1000.0);
        assert!(hit.is_none());
    }

    #[wasm_bindgen_test]
    fn mping_picks_nearest_of_two_in_radius() {
        let p = proj_on_kabr();
        // A at the click pixel (dist 0); B at (404,303), dist_sq = 25 from click.
        // Both within radius (81); the nearer one (A, id 1) wins.
        let reports = vec![
            report_at_pixel(&p, 1, 400.0, 300.0, 0.0),
            report_at_pixel(&p, 2, 404.0, 303.0, 0.0),
        ];
        let hit = pick_mping_report_at(egui::Pos2::new(400.0, 300.0), &p, &reports, 1000.0);
        assert!(hit == Some(1));
    }
}
